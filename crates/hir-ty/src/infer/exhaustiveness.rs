//! Whether a `when` over an enum, a sealed type or a `Boolean` covers every
//! case. Reported only when all of it is known: every condition resolved,
//! the subject's cases all found, and the subject not tested before the
//! `when` (Kotlin's data flow may already have ruled cases out).

use hir_def::item_tree::{ClassKind, ItemKind, Modifiers, Signature};
use hir_def::{ItemContainer, ItemLocation};
use syntax::{SyntaxNode, TextRange, has_token, named_children};

use super::declarations::Inferrer;
use super::fit::Fit;
use super::narrowing::StablePath;
use super::{NonExhaustiveWhen, Reference};
use crate::lower::lower_supertype;
use crate::ty::{Nullability, Ty};

/// What a `when`'s branches cover.
#[derive(Default)]
struct Covered {
    /// Enum entries and objects named by a condition.
    declarations: Vec<ItemLocation>,
    /// `is T` conditions.
    types: Vec<Ty>,
    booleans: Vec<bool>,
    null: bool,
}

/// What a subject's type requires a `when` to cover.
enum Cases {
    Booleans,
    EnumEntries(Vec<ItemLocation>),
    /// A sealed type's direct subclasses (and subobjects).
    Inheritors(Vec<ItemLocation>),
}

impl Inferrer<'_> {
    /// Records `node` (a `when` without `else`) when it misses cases.
    pub(super) fn check_exhaustive(&mut self, node: SyntaxNode<'_>, subject_type: &Ty) {
        let Some(covered) = self.covered(node) else {
            return;
        };
        if self.subject_tested_before(node) {
            return;
        }
        let Some(cases) = self.cases_of(subject_type) else {
            return;
        };
        let mut missing: Vec<String> = match cases {
            Cases::Booleans => [true, false]
                .into_iter()
                .filter(|value| !covered.booleans.contains(value))
                .map(|value| value.to_string())
                .collect(),
            Cases::EnumEntries(entries) => {
                let whole_enum_covered = covered
                    .types
                    .iter()
                    .any(|ty| self.types.fits(subject_type, ty) == Fit::Yes);
                match subject_type.class() {
                    Some(enum_type) if !whole_enum_covered => {
                        self.uncovered_entries(enum_type.class, entries, &covered)
                    }
                    _ => Vec::new(),
                }
            }
            Cases::Inheritors(inheritors) => {
                let mut missing = Vec::new();
                for inheritor in inheritors {
                    match self.uncovered_inheritor(inheritor, &covered, 0) {
                        InheritorCoverage::Covered => {}
                        InheritorCoverage::Missing(names) => missing.extend(names),
                        InheritorCoverage::Unknown => return,
                    }
                }
                missing
            }
        };
        if subject_type.nullability() == Nullability::Nullable && !covered.null {
            missing.push("null".to_owned());
        }
        if missing.is_empty() {
            return;
        }
        let start = node.start_byte();
        self.record_non_exhaustive_when(NonExhaustiveWhen {
            range: TextRange::new(start, start + "when".len()),
            missing,
        });
    }

    /// What the branches' conditions cover; `None` when a condition is one
    /// this check doesn't model (a range, a negated type test, a guard, an
    /// arbitrary value) or doesn't resolve.
    fn covered(&self, node: SyntaxNode<'_>) -> Option<Covered> {
        let mut covered = Covered::default();
        for entry in named_children(node).filter(|child| child.kind() == "when_entry") {
            if has_token(entry, "if") {
                return None;
            }
            for condition in named_children(entry).filter(|child| child.kind() == "when_condition")
            {
                let value = named_children(condition).next()?;
                match value.kind() {
                    "type_test" => {
                        if has_token(value, "!is") {
                            return None;
                        }
                        let target = self.type_child(value)?;
                        let ty = self.written_type(target);
                        if ty.is_unknown() {
                            return None;
                        }
                        covered.types.push(ty);
                    }
                    "null_literal" => covered.null = true,
                    "boolean_literal" => covered.booleans.push(self.text(value) == "true"),
                    "simple_identifier" | "navigation_expression" => {
                        let name = last_identifier(value)?;
                        let resolved = self.recorded_at(name)?;
                        let Reference::Declaration(location) = resolved.reference else {
                            return None;
                        };
                        covered.declarations.push(location);
                    }
                    _ => return None,
                }
            }
        }
        Some(covered)
    }

    /// Whether the subject was compared or type-checked earlier in the
    /// declaration: then Kotlin's data flow may have excluded cases.
    fn subject_tested_before(&self, node: SyntaxNode<'_>) -> bool {
        let Some(subject) = named_children(node)
            .find(|child| child.kind() == "when_subject")
            .and_then(|subject| named_children(subject).last())
        else {
            return true;
        };
        let path = StablePath::of(self.text(subject));
        let scope = enclosing_body(node);
        syntax::descendants(scope)
            .filter(|candidate| candidate.end_byte() <= node.start_byte())
            .any(|candidate| self.compares(candidate, &path) || self.guards(candidate, &path))
    }

    /// Whether `node` compares the expression spelled `path` with anything,
    /// or tests its type.
    fn compares(&self, node: SyntaxNode<'_>, path: &StablePath) -> bool {
        let is_test = matches!(
            node.kind(),
            "equality_expression" | "check_expression" | "when_subject"
        );
        is_test && named_children(node).any(|operand| StablePath::of(self.text(operand)) == *path)
    }

    fn cases_of(&self, subject_type: &Ty) -> Option<Cases> {
        let class = subject_type.class()?.class;
        if matches!(self.types.builtin("Boolean"), Ty::Class(boolean) if boolean.class == class) {
            return Some(Cases::Booleans);
        }
        let tree = hir_def::item_tree(self.db, class.container);
        let item = tree.item(class.item);
        match item.kind {
            ItemKind::Class(ClassKind::Enum) => {
                Some(Cases::EnumEntries(self.entries_of_enum(class)?))
            }
            ItemKind::Class(_) if item.modifiers.contains(Modifiers::SEALED) => {
                Some(Cases::Inheritors(self.sealed_inheritors(class)?))
            }
            _ => None,
        }
    }

    /// The direct subclasses of a workspace sealed class: in its package
    /// and source set. `None` for a compiled one, whose list isn't read.
    fn sealed_inheritors(&self, sealed: ItemLocation) -> Option<Vec<ItemLocation>> {
        let ItemContainer::File(file) = sealed.container else {
            return None;
        };
        let source_set_id = file.source_set(self.db)?;
        let source_set = *self
            .db
            .workspace()
            .source_sets(self.db)
            .get(source_set_id.0 as usize)?;
        let sealed_tree = hir_def::file_item_tree(self.db, file);
        let package = sealed_tree.package.clone();
        let sealed_name = sealed_tree.item(sealed.item).name;
        let mut inheritors = Vec::new();
        for candidate_file in source_set.files(self.db).iter() {
            let tree = hir_def::file_item_tree(self.db, *candidate_file);
            if tree.package != package {
                continue;
            }
            for (index, item) in tree.indexed_items() {
                let Signature::Class { supertypes, .. } = &item.signature else {
                    continue;
                };
                let location = ItemLocation::in_file(*candidate_file, index);
                // Only a supertype spelled with the sealed class's name is
                // resolved (a type alias to it isn't followed: an inheritor
                // missed that way leaves the check silent, never wrong).
                let extends_sealed = supertypes
                    .iter()
                    .filter(|supertype| {
                        supertype.path().and_then(|path| path.last()) == Some(sealed_name)
                    })
                    .any(|supertype| {
                        lower_supertype(self.db, self.file, location, supertype)
                            .is_some_and(|resolved| resolved.class == sealed)
                    });
                if extends_sealed {
                    inheritors.push(location);
                }
            }
        }
        Some(inheritors)
    }

    /// Whether `inheritor` is covered: named (an object), type-tested
    /// (itself or a supertype), or — when it's sealed too — through all
    /// its own inheritors.
    fn uncovered_inheritor(
        &self,
        inheritor: ItemLocation,
        covered: &Covered,
        depth: usize,
    ) -> InheritorCoverage {
        if covered.declarations.contains(&inheritor) {
            return InheritorCoverage::Covered;
        }
        let inheritor_type = Ty::Class(self.types.self_type(inheritor));
        let type_tested = covered
            .types
            .iter()
            .any(|ty| self.types.fits(&inheritor_type, ty) == Fit::Yes);
        if type_tested {
            return InheritorCoverage::Covered;
        }
        let tree = hir_def::item_tree(self.db, inheritor.container);
        let item = tree.item(inheritor.item);
        if item.kind == ItemKind::Class(ClassKind::Enum) {
            let Some(entries) = self.entries_of_enum(inheritor) else {
                return InheritorCoverage::Unknown;
            };
            let missing = self.uncovered_entries(inheritor, entries, covered);
            return if missing.is_empty() {
                InheritorCoverage::Covered
            } else {
                InheritorCoverage::Missing(missing)
            };
        }
        if item.modifiers.contains(Modifiers::SEALED) && depth < 8 {
            let Some(own) = self.sealed_inheritors(inheritor) else {
                return InheritorCoverage::Unknown;
            };
            let mut missing = Vec::new();
            for nested in own {
                match self.uncovered_inheritor(nested, covered, depth + 1) {
                    InheritorCoverage::Covered => {}
                    InheritorCoverage::Missing(names) => missing.extend(names),
                    InheritorCoverage::Unknown => return InheritorCoverage::Unknown,
                }
            }
            return if missing.is_empty() {
                InheritorCoverage::Covered
            } else {
                InheritorCoverage::Missing(missing)
            };
        }
        let name = self.item_name(inheritor);
        let is_object = matches!(item.kind, ItemKind::Class(ClassKind::Object));
        InheritorCoverage::Missing(vec![if is_object {
            name
        } else {
            format!("is {name}")
        }])
    }

    /// An enum's entries; `None` for a compiled Java enum, which lists them
    /// as statics, not as entries.
    fn entries_of_enum(&self, enum_class: ItemLocation) -> Option<Vec<ItemLocation>> {
        let tree = hir_def::item_tree(self.db, enum_class.container);
        let entries: Vec<ItemLocation> = tree
            .item(enum_class.item)
            .children
            .iter()
            .filter(|child| tree.item(**child).kind == ItemKind::EnumEntry)
            .map(|child| ItemLocation {
                container: enum_class.container,
                item: *child,
            })
            .collect();
        (!entries.is_empty()).then_some(entries)
    }

    /// The entries no branch names, qualified: `Side.SELL`.
    fn uncovered_entries(
        &self,
        enum_class: ItemLocation,
        entries: Vec<ItemLocation>,
        covered: &Covered,
    ) -> Vec<String> {
        let enum_name = crate::display::class_name(self.db, enum_class);
        entries
            .into_iter()
            .filter(|entry| !covered.declarations.contains(entry))
            .map(|entry| format!("{enum_name}.{}", self.item_name(entry)))
            .collect()
    }

    fn item_name(&self, location: ItemLocation) -> String {
        let tree = hir_def::item_tree(self.db, location.container);
        let item = tree.item(location.item);
        if item.kind == ItemKind::EnumEntry {
            return item.name.to_string();
        }
        crate::display::class_name(self.db, location)
    }
}

enum InheritorCoverage {
    Covered,
    Missing(Vec<String>),
    Unknown,
}

/// The last name of `a.b.C` (or `C`).
fn last_identifier(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    match node.kind() {
        "simple_identifier" => Some(node),
        "navigation_expression" => named_children(node).last().and_then(|suffix| {
            named_children(suffix).find(|child| child.kind() == "simple_identifier")
        }),
        _ => None,
    }
}

/// The function (or other member-level declaration) around `node`.
fn enclosing_body(node: SyntaxNode<'_>) -> SyntaxNode<'_> {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(
            parent.kind(),
            "function_declaration"
                | "getter"
                | "setter"
                | "anonymous_initializer"
                | "secondary_constructor"
        ) {
            return parent;
        }
        current = parent;
    }
    current
}
