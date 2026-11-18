//! Uses of API that needs opt-in: what Kotlin reports as `OPT_IN_USAGE`
//! (a warning) and `OPT_IN_USAGE_ERROR`.
//!
//! A marker is an annotation class with `@RequiresOptIn(level, message)`
//! (`ERROR` unless it says). A declaration needs the markers it's
//! annotated with; a class also its outer classes', a constructor its
//! class's, a member its class's (the receiver's type). A use is accepted
//! when the module opts in (`-opt-in=Marker`), or when an enclosing
//! declaration, expression or the file is annotated `@OptIn(Marker::class)`
//! or with the marker itself (which makes it need the opt-in in turn).
//!
//! Not modelled, so not reported: markers on the classes of a callable's
//! signature, `@SubclassOptInRequired`, overrides of marked members.

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ItemKind, OptInLevel, OptInMarker};
use hir_def::resolver::{
    Resolution, resolve_import_path, resolve_type_path, resolve_type_path_in_declaration,
};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use intern::{Name, Path};
use rustc_hash::{FxHashMap, FxHashSet};
use std::rc::Rc;
use syntax::{Language, Parse, SyntaxNode, TextRange, has_token, named_children};

use crate::infer::{Reference, infer_file};

/// A use of a declaration that needs an opt-in the use site doesn't give.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OptInUse {
    pub range: TextRange,
    pub declaration: ItemLocation,
    /// The marker's qualified name: `kotlinx.coroutines.ExperimentalCoroutinesApi`.
    pub marker_name: Path,
    pub marker: OptInMarker,
}

/// Every use in `file` that needs an opt-in it doesn't have.
pub fn opt_in_uses(db: &dyn SourceDatabase, file: SourceFile) -> Vec<OptInUse> {
    if file.language(db) != Language::Kotlin {
        return Vec::new();
    }
    // What the module opts into must be known: a model exported without
    // compiler options, or guessed, may opt into anything.
    let Some(opted_in_module) = module_opt_ins(db, file) else {
        return Vec::new();
    };
    let parse = base_db::parse(db, file);
    let mut uses = Uses {
        db,
        file,
        parse: &parse,
        opted_in_module,
        markers: FxHashMap::default(),
        seen: FxHashSet::default(),
        found: Vec::new(),
    };
    uses.in_bodies();
    uses.in_types();
    uses.found.sort_by_key(|found| found.range.start);
    uses.found
}

/// The markers `file`'s module opts into, when the build says.
fn module_opt_ins(db: &dyn SourceDatabase, file: SourceFile) -> Option<FxHashSet<String>> {
    let source_set = file.source_set(db)?;
    let model = db.workspace().model(db);
    let compiler = &model.source_set(source_set).compiler;
    compiler
        .declared
        .then(|| compiler.opt_in.iter().cloned().collect())
}

/// A marker a declaration needs, resolved.
#[derive(Clone, PartialEq, Eq, Debug)]
struct RequiredMarker {
    class: ItemLocation,
    name: Path,
    marker: OptInMarker,
}

/// The markers `location` needs, seen from `from`.
fn required_markers(
    db: &dyn SourceDatabase,
    from: SourceFile,
    location: ItemLocation,
) -> Vec<RequiredMarker> {
    let tree = item_tree(db, location.container);
    let item = tree.item(location.item);
    let mut found = own_markers(db, from, location);
    // A class's outer classes, a constructor's or member's class.
    let mut enclosing = item.parent;
    while let Some(parent) = enclosing {
        let parent_location = ItemLocation {
            container: location.container,
            item: parent,
        };
        for marker in own_markers(db, from, parent_location) {
            if !found.contains(&marker) {
                found.push(marker);
            }
        }
        enclosing = tree.item(parent).parent;
    }
    found
}

/// The markers among the annotations written on `location` itself.
fn own_markers(
    db: &dyn SourceDatabase,
    from: SourceFile,
    location: ItemLocation,
) -> Vec<RequiredMarker> {
    let tree = item_tree(db, location.container);
    let mut found = Vec::new();
    for path in tree.annotations(location.item) {
        let resolution = match location.container {
            ItemContainer::Library(_) => resolve_import_path(db, from, path),
            ItemContainer::File(file) => {
                resolve_type_path_in_declaration(db, file, path, location.item)
            }
        };
        let Resolution::Declaration(class) = resolution else {
            continue;
        };
        let class_tree = item_tree(db, class.container);
        if let Some(marker) = class_tree.opt_in_marker(class.item) {
            found.push(RequiredMarker {
                class,
                name: qualified_name(db, class),
                marker: marker.clone(),
            });
        }
    }
    found
}

/// `package.Outer.Name` of a class.
fn qualified_name(db: &dyn SourceDatabase, class: ItemLocation) -> Path {
    let tree = item_tree(db, class.container);
    let mut names = vec![tree.item(class.item).name];
    let mut parent = tree.item(class.item).parent;
    while let Some(outer) = parent {
        names.push(tree.item(outer).name);
        parent = tree.item(outer).parent;
    }
    names.reverse();
    let mut segments: Vec<Name> = tree.package.segments().to_vec();
    segments.extend(names);
    Path::new(segments)
}

struct Uses<'db, 'parse> {
    db: &'db dyn SourceDatabase,
    file: SourceFile,
    parse: &'parse Parse,
    /// The markers the module opts into (`-opt-in`), by qualified name.
    opted_in_module: FxHashSet<String>,
    /// Each declaration's markers, resolved once: a file uses the same
    /// declarations again and again.
    markers: FxHashMap<ItemLocation, Rc<[RequiredMarker]>>,
    seen: FxHashSet<u32>,
    found: Vec<OptInUse>,
}

impl Uses<'_, '_> {
    fn add(&mut self, name: SyntaxNode<'_>, declaration: ItemLocation) {
        let range = TextRange::of(name);
        if !self.seen.insert(range.start) {
            return;
        }
        let required = self.required(declaration);
        if required.is_empty() || !self.choice_is_certain(declaration, &required) {
            return;
        }
        for marker in required.iter() {
            if self.accepted(name, marker) {
                continue;
            }
            self.found.push(OptInUse {
                range,
                declaration,
                marker_name: marker.name.clone(),
                marker: marker.marker.clone(),
            });
        }
    }

    fn required(&mut self, declaration: ItemLocation) -> Rc<[RequiredMarker]> {
        if let Some(known) = self.markers.get(&declaration) {
            return known.clone();
        }
        let found: Rc<[RequiredMarker]> = required_markers(self.db, self.file, declaration).into();
        self.markers.insert(declaration, found.clone());
        found
    }

    /// Overloads are chosen without every argument type judged: when one
    /// needs other markers, resolving to `declaration` says nothing.
    fn choice_is_certain(
        &mut self,
        declaration: ItemLocation,
        required: &[RequiredMarker],
    ) -> bool {
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
        let classes = |markers: &[RequiredMarker]| -> FxHashSet<ItemLocation> {
            markers.iter().map(|marker| marker.class).collect()
        };
        let chosen = classes(required);
        overloads
            .into_iter()
            .all(|overload| classes(&self.required(overload)) == chosen)
    }

    /// Whether the module, or an annotation around `use_site`, opts in to
    /// `marker`.
    fn accepted(&self, use_site: SyntaxNode<'_>, marker: &RequiredMarker) -> bool {
        if self.opted_in_module.contains(&marker.name.to_string()) {
            return true;
        }
        let mut node = Some(use_site);
        while let Some(current) = node {
            if self.annotations_opt_in(current, marker) {
                return true;
            }
            // An accessor on a line of its own is its property's sibling
            // in the tree: the property's annotations cover it.
            if let Some(property) = accessed_property(current)
                && self.annotations_opt_in(property, marker)
            {
                return true;
            }
            node = current.parent();
        }
        self.file_opts_in(marker)
    }

    /// Whether `node`'s own annotations opt in to `marker`: a
    /// declaration's `modifiers`, or the annotations an expression or an
    /// argument starts with (`@OptIn(…) contract { … }`, `f(@OptIn(…) id =
    /// x)`), which the grammar makes its children.
    fn annotations_opt_in(&self, node: SyntaxNode<'_>, marker: &RequiredMarker) -> bool {
        let mut holders: Vec<SyntaxNode<'_>> = vec![node];
        holders.extend(
            named_children(node)
                .filter(|child| matches!(child.kind(), "modifiers" | "parameter_modifiers")),
        );
        holders.into_iter().any(|holder| {
            named_children(holder)
                .filter(|child| child.kind() == "annotation")
                .any(|annotation| self.annotation_opts_in(annotation, marker))
        })
    }

    /// `@file:OptIn(…)`.
    fn file_opts_in(&self, marker: &RequiredMarker) -> bool {
        named_children(self.parse.root())
            .filter(|child| child.kind() == "file_annotation")
            .any(|annotation| self.annotation_opts_in(annotation, marker))
    }

    /// Whether `annotation` is `@OptIn(…, Marker::class, …)` or the marker
    /// itself.
    fn annotation_opts_in(&self, annotation: SyntaxNode<'_>, marker: &RequiredMarker) -> bool {
        let invocation =
            named_children(annotation).find(|child| child.kind() == "constructor_invocation");
        let user_type = match invocation {
            Some(invocation) => {
                named_children(invocation).find(|child| child.kind() == "user_type")
            }
            None => named_children(annotation).find(|child| child.kind() == "user_type"),
        };
        let Some(user_type) = user_type else {
            return false;
        };
        // An annotation whose class doesn't resolve (a source set that
        // doesn't see the standard library: Kotlin/Native code here) may be
        // `@OptIn`: it may opt in.
        let Some(class) = self.resolve_class(user_type) else {
            return true;
        };
        if class == marker.class {
            return true;
        }
        let is_opt_in = item_tree(self.db, class.container)
            .item(class.item)
            .name
            .as_str()
            == "OptIn";
        if !is_opt_in {
            return false;
        }
        let Some(arguments) = invocation.and_then(|invocation| {
            named_children(invocation).find(|child| child.kind() == "value_arguments")
        }) else {
            return false;
        };
        syntax::descendants(arguments)
            .filter(|node| {
                node.kind() == "callable_reference" || node.kind() == "navigation_expression"
            })
            .any(|reference| {
                let written = self.parse.text_of(reference);
                let Some(class_part) = written.strip_suffix("::class") else {
                    return false;
                };
                let path = Path::new(
                    class_part
                        .split('.')
                        .map(|segment| Name::new(segment.trim())),
                );
                matches!(
                    resolve_type_path(self.db, self.file, &path, reference.start_byte()),
                    Resolution::Declaration(found) if found == marker.class
                )
            })
    }

    fn resolve_class(&self, user_type: SyntaxNode<'_>) -> Option<ItemLocation> {
        let written = self.parse.text_of(user_type);
        let path = Path::new(
            written
                .split('.')
                .map(|segment| Name::new(segment.trim_matches('`'))),
        );
        match resolve_type_path(self.db, self.file, &path, user_type.start_byte()) {
            Resolution::Declaration(class) => Some(class),
            _ => None,
        }
    }

    /// Names inference resolved to one declaration — not a `name =` label.
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
            // The syntax is only looked at for what needs an opt-in:
            // finding a name's node and its parents walks the tree.
            if self.required(location).is_empty() {
                continue;
            }
            let start = start as usize;
            let Some(name) = self
                .parse
                .root()
                .named_descendant_for_byte_range(start, start + 1)
                .filter(|node| node.kind() == "simple_identifier")
            else {
                continue;
            };
            if is_label(name) {
                continue;
            }
            self.add(name, location);
        }
    }

    /// Each segment of every written type that names a class — not an
    /// annotation's own class (`@Beta` is how one opts in).
    fn in_types(&mut self) {
        let references = crate::type_references::type_references(self.db, self.file);
        for reference in references
            .iter()
            .filter(|reference| !reference.annotation_class)
        {
            if self.required(reference.declaration).is_empty() {
                continue;
            }
            let start = reference.range.start as usize;
            let Some(segment) = self
                .parse
                .root()
                .named_descendant_for_byte_range(start, start + 1)
            else {
                continue;
            };
            self.add(segment, reference.declaration);
        }
    }
}

/// The property a getter or setter written on its own line belongs to:
/// the nearest property declared before it.
fn accessed_property(accessor: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    if !matches!(accessor.kind(), "getter" | "setter") {
        return None;
    }
    let mut previous = syntax::previous_named_sibling(accessor);
    while let Some(sibling) = previous {
        match sibling.kind() {
            "property_declaration" => return Some(sibling),
            "getter" | "setter" => previous = syntax::previous_named_sibling(sibling),
            _ => return None,
        }
    }
    None
}

/// Whether `name` is the label of a named argument (`name = value`).
fn is_label(name: SyntaxNode<'_>) -> bool {
    let Some(parent) = name.parent() else {
        return false;
    };
    let first = named_children(parent).next();
    parent.kind() == "value_argument" && has_token(parent, "=") && first == Some(name)
}

impl OptInUse {
    /// `WARNING` markers make a warning, `ERROR` ones an error.
    pub fn level(&self) -> OptInLevel {
        self.marker.level
    }
}
