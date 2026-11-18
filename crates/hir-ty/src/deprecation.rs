//! Uses of deprecated declarations: what Kotlin reports as `DEPRECATION`
//! (a warning) and `DEPRECATION_ERROR`.
//!
//! A declaration is deprecated by its own `@Deprecated` (Kotlin's or
//! Java's, or a Javadoc `@deprecated`), or — a function or property — by a
//! Kotlin `@Deprecated` on a member it overrides; a constructor also by its
//! class's, a type alias by its expansion's. Uses are names in bodies, names
//! in types (signatures, annotations, supertypes) and imports — not the
//! target of an assignment, which a setter's deprecation would decide, and
//! not a `name =` label.

use base_db::{ArchiveKind, JdkRelease, KotlinVersion, SourceDatabase, SourceFile};
use hir_def::item_tree::{
    Deprecation, DeprecationLevel, DeprecationOrigin, ItemKind, Modifiers, Signature, TypeRef,
};
use hir_def::resolver::{Resolution, resolve_import_path};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use intern::{Name, Path};
use rustc_hash::{FxHashMap, FxHashSet};
use syntax::{Language, Parse, SyntaxNode, TextRange, has_token, named_children};

use crate::infer::{Reference, infer_file};
use crate::members::Types;

/// A name that means a deprecated declaration.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DeprecatedUse {
    pub range: TextRange,
    pub declaration: ItemLocation,
    /// The declaration's name, as declared.
    pub name: Name,
    /// The deprecation that applies: the declaration's own or the one it
    /// inherits.
    pub deprecation: Deprecation,
    /// What the deprecation makes the use: a warning or an error.
    pub level: DeprecationLevel,
}

/// Every use of a deprecated declaration in `file`.
pub fn deprecated_uses(db: &dyn SourceDatabase, file: SourceFile) -> Vec<DeprecatedUse> {
    if file.language(db) != Language::Kotlin {
        return Vec::new();
    }
    let parse = base_db::parse(db, file);
    let mut uses = Uses {
        db,
        file,
        parse: &parse,
        deprecations: Deprecations::new(db, file),
        seen: FxHashSet::default(),
        found: Vec::new(),
    };
    uses.in_bodies();
    uses.in_types();
    uses.in_imports();
    uses.found.sort_by_key(|found| found.range.start);
    uses.found
}

/// The deprecation `location` has for a use from `from`, and what it makes
/// the use; `None` when it isn't deprecated, or when its level depends on
/// a Kotlin API version (`@DeprecatedSinceKotlin`) that isn't known.
pub fn effective_deprecation(
    db: &dyn SourceDatabase,
    from: SourceFile,
    location: ItemLocation,
) -> Option<(Deprecation, DeprecationLevel)> {
    Deprecations::new(db, from).of(location)
}

/// Deprecations as one file sees them, each declaration's looked up once:
/// a file uses the same declarations again and again, and finding what a
/// member overrides walks its class's hierarchy.
struct Deprecations<'db> {
    db: &'db dyn SourceDatabase,
    types: Types<'db>,
    api_version: Option<KotlinVersion>,
    known: FxHashMap<ItemLocation, Option<(Deprecation, DeprecationLevel)>>,
}

impl<'db> Deprecations<'db> {
    fn new(db: &'db dyn SourceDatabase, from: SourceFile) -> Deprecations<'db> {
        Deprecations {
            db,
            types: Types::new(db, from),
            api_version: api_version(db, from),
            known: FxHashMap::default(),
        }
    }

    fn of(&mut self, location: ItemLocation) -> Option<(Deprecation, DeprecationLevel)> {
        if let Some(known) = self.known.get(&location) {
            return known.clone();
        }
        let found = self.applying(location).and_then(|deprecation| {
            let level = level(&deprecation, self.api_version)?;
            Some((deprecation, level))
        });
        self.known.insert(location, found.clone());
        found
    }

    /// The worst deprecation among the declaration's own and those it
    /// inherits.
    fn applying(&self, location: ItemLocation) -> Option<Deprecation> {
        let tree = item_tree(self.db, location.container);
        let item = tree.item(location.item);
        let own = tree
            .deprecation(location.item)
            .filter(|_| !from_assumed_jdk(self.db, location))
            .cloned();
        let inherited = match item.kind {
            ItemKind::Function | ItemKind::Property { .. } if own.is_none() => {
                self.overridden(location)
            }
            ItemKind::Constructor => item.parent.and_then(|class| {
                self.applying(ItemLocation {
                    container: location.container,
                    item: class,
                })
            }),
            ItemKind::TypeAlias => match &item.signature {
                Signature::TypeAlias { target } => self.expansion(location, target),
                _ => None,
            },
            _ => None,
        };
        worst(own, inherited)
    }

    /// A Kotlin deprecation of a member `location` overrides: it passes to
    /// overrides (Java's doesn't). A Kotlin member written without
    /// `override` overrides nothing.
    fn overridden(&self, location: ItemLocation) -> Option<Deprecation> {
        if !may_override(self.db, location) {
            return None;
        }
        let mut found = None;
        for overridden in self.types.overridden(location) {
            let tree = item_tree(self.db, overridden.container);
            let passes = tree
                .deprecation(overridden.item)
                .filter(|deprecation| deprecation.origin == DeprecationOrigin::Kotlin)
                .cloned();
            found = worst(found, passes);
        }
        found
    }

    /// The deprecation of the class a type alias expands to.
    fn expansion(&self, alias: ItemLocation, target: &TypeRef) -> Option<Deprecation> {
        let TypeRef::Named { segments, .. } = target else {
            return None;
        };
        let path = Path::new(segments.iter().map(|segment| segment.name));
        let hir_def::ItemContainer::File(alias_file) = alias.container else {
            return None;
        };
        let Resolution::Declaration(expanded) = hir_def::resolver::resolve_type_path_in_declaration(
            self.db, alias_file, &path, alias.item,
        ) else {
            return None;
        };
        self.applying(expanded)
    }
}

/// Whether `member` may override another: anything but a Kotlin source
/// member written without `override` (compiled code doesn't say).
fn may_override(db: &dyn SourceDatabase, member: ItemLocation) -> bool {
    let ItemContainer::File(file) = member.container else {
        return true;
    };
    if file.language(db) != Language::Kotlin {
        return true;
    }
    item_tree(db, member.container)
        .item(member.item)
        .modifiers
        .contains(Modifiers::OVERRIDE)
}

/// Whether `location` is the JDK's, read for the release the build runs
/// on rather than one it declares: what's deprecated there may not be in
/// the release a module compiles against (`URL(String)`, deprecated in 20,
/// for code built with JDK 8).
fn from_assumed_jdk(db: &dyn SourceDatabase, location: ItemLocation) -> bool {
    let ItemContainer::Library(package) = location.container else {
        return false;
    };
    matches!(
        package.library(db).source(db).kind,
        ArchiveKind::JdkSymbols {
            known: JdkRelease::RunningJdk,
            ..
        }
    )
}

fn worst(first: Option<Deprecation>, second: Option<Deprecation>) -> Option<Deprecation> {
    match (first, second) {
        (Some(first), Some(second)) if second.level > first.level => Some(second),
        (Some(first), _) => Some(first),
        (None, second) => second,
    }
}

/// The level a deprecation gives a use. `@DeprecatedSinceKotlin` decides
/// by the Kotlin API version the code compiles against instead: hidden,
/// an error or a warning from the versions it names on, not deprecated
/// before — and no level when the version isn't known.
fn level(
    deprecation: &Deprecation,
    api_version: Option<KotlinVersion>,
) -> Option<DeprecationLevel> {
    let Some(since) = &deprecation.since_kotlin else {
        return Some(deprecation.level);
    };
    let api_version = api_version?;
    let reached = |version: &Option<Box<str>>| {
        version
            .as_deref()
            .and_then(KotlinVersion::parse)
            .is_some_and(|version| version <= api_version)
    };
    if reached(&since.hidden_since) {
        Some(DeprecationLevel::Hidden)
    } else if reached(&since.error_since) {
        Some(DeprecationLevel::Error)
    } else if reached(&since.warning_since) {
        Some(DeprecationLevel::Warning)
    } else {
        None
    }
}

/// The API version `file`'s source set compiles against, when the model
/// says.
fn api_version(db: &dyn SourceDatabase, file: SourceFile) -> Option<KotlinVersion> {
    let source_set = file.source_set(db)?;
    let model = db.workspace().model(db);
    model.source_set(source_set).compiler.api_version()
}

struct Uses<'db, 'parse> {
    db: &'db dyn SourceDatabase,
    file: SourceFile,
    parse: &'parse Parse,
    deprecations: Deprecations<'db>,
    /// Where a use was already found: a constructor call's name may be both
    /// a resolved name and a type.
    seen: FxHashSet<u32>,
    found: Vec<DeprecatedUse>,
}

impl Uses<'_, '_> {
    fn add(&mut self, range: TextRange, declaration: ItemLocation) {
        if !self.seen.insert(range.start) {
            return;
        }
        let Some((deprecation, level)) = self.deprecations.of(declaration) else {
            return;
        };
        // Code can't reach a hidden declaration: resolving to one is our
        // mistake.
        if level == DeprecationLevel::Hidden || !self.choice_is_certain(declaration) {
            return;
        }
        let name = item_tree(self.db, declaration.container)
            .item(declaration.item)
            .name;
        self.found.push(DeprecatedUse {
            range,
            declaration,
            name,
            deprecation,
            level,
        });
    }

    /// Whether resolving to `declaration` rather than an overload is
    /// certain as far as its deprecation goes: overloads are chosen without
    /// every argument type judged, so when one with another deprecation
    /// exists (an old `ERROR` constructor beside the new one), the choice
    /// says nothing.
    fn choice_is_certain(&mut self, declaration: ItemLocation) -> bool {
        let tree = item_tree(self.db, declaration.container);
        let item = tree.item(declaration.item);
        let is_callable = matches!(item.kind, ItemKind::Function | ItemKind::Constructor);
        if !is_callable {
            return true;
        }
        let overloads: Vec<ItemLocation> = match item.parent {
            Some(parent) => tree
                .item(parent)
                .children
                .iter()
                .filter(|child| {
                    let child = tree.item(**child);
                    child.kind == item.kind
                        && (child.name == item.name || item.kind == ItemKind::Constructor)
                })
                .map(|child| ItemLocation {
                    container: declaration.container,
                    item: *child,
                })
                .collect(),
            None => {
                hir_def::def_map::visible_top_level(self.db, self.file, &tree.package, item.name)
                    .into_iter()
                    .filter(|overload| {
                        item_tree(self.db, overload.container)
                            .item(overload.item)
                            .kind
                            == ItemKind::Function
                    })
                    .collect()
            }
        };
        let chosen = self.deprecations.of(declaration).map(|(_, level)| level);
        overloads
            .into_iter()
            .all(|overload| self.deprecations.of(overload).map(|(_, level)| level) == chosen)
    }

    /// Names inference resolved to one declaration.
    fn in_bodies(&mut self) {
        let inference = infer_file(self.db, self.file);
        let mut resolved: Vec<(u32, ItemLocation)> = inference
            .names()
            .filter_map(|(start, name)| match name.reference {
                Reference::Declaration(location) => Some((start, location)),
                _ => None,
            })
            .collect();
        resolved.sort_by_key(|(start, _)| *start);
        for (start, location) in resolved {
            // The syntax is only looked at for what is deprecated: finding
            // a name's node and its parents walks the tree.
            if self.deprecations.of(location).is_none() {
                continue;
            }
            let Some(name) = self.identifier_at(start) else {
                continue;
            };
            if is_assigned(name) || is_label(name) {
                continue;
            }
            self.add(TextRange::of(name), location);
        }
    }

    /// Each segment of every written type that names a class or alias.
    fn in_types(&mut self) {
        for reference in crate::type_references::type_references(self.db, self.file).iter() {
            self.add(reference.range, reference.declaration);
        }
    }

    /// `import a.b.Old`: reported when everything it imports is deprecated.
    fn in_imports(&mut self) {
        let headers: Vec<SyntaxNode<'_>> = syntax::descendants(self.parse.root())
            .filter(|node| node.kind() == "import_header")
            .collect();
        for header in headers {
            let star = has_token(header, "*") || has_token(header, ".*");
            let Some(identifier) =
                named_children(header).find(|child| child.kind() == "identifier")
            else {
                continue;
            };
            let segments: Vec<SyntaxNode<'_>> = named_children(identifier)
                .filter(|child| child.kind() == "simple_identifier")
                .collect();
            let Some(&last) = segments.last() else {
                continue;
            };
            if star {
                continue;
            }
            let path = Path::new(
                segments
                    .iter()
                    .map(|segment| Name::new(self.parse.text_of(*segment).trim_matches('`'))),
            );
            let declarations = resolve_import_path(self.db, self.file, &path).declarations();
            let all_deprecated = !declarations.is_empty()
                && declarations
                    .iter()
                    .all(|declaration| self.deprecations.of(*declaration).is_some());
            if all_deprecated {
                self.add(TextRange::of(last), declarations[0]);
            }
        }
    }

    fn identifier_at(&self, start: u32) -> Option<SyntaxNode<'_>> {
        let start = start as usize;
        self.parse
            .root()
            .named_descendant_for_byte_range(start, start + 1)
            .filter(|node| node.kind() == "simple_identifier")
    }
}

/// Whether `name` is what an assignment assigns: `name = …`,
/// `receiver.name = …`.
fn is_assigned(name: SyntaxNode<'_>) -> bool {
    let Some(parent) = name.parent() else {
        return false;
    };
    match parent.kind() {
        "directly_assignable_expression" => true,
        "navigation_suffix" => parent
            .parent()
            .is_some_and(|grandparent| grandparent.kind() == "directly_assignable_expression"),
        _ => false,
    }
}

/// Whether `name` is the label of a named argument (`name = value`).
fn is_label(name: SyntaxNode<'_>) -> bool {
    let Some(parent) = name.parent() else {
        return false;
    };
    let first = named_children(parent).next();
    parent.kind() == "value_argument" && has_token(parent, "=") && first == Some(name)
}
