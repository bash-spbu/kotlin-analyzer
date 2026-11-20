//! Which members are one member as far as renaming goes: a function or
//! property, what it overrides, and what overrides those in the workspace;
//! what implements a class or member there (`textDocument/implementation`);
//! and what a member overrides directly (`textDocument/declaration`).

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ItemKind, Signature};
use hir_def::{ItemContainer, ItemLocation, file_item_tree, item_tree};
use intern::Name;
use rustc_hash::FxHashSet;

use crate::lower::lower_supertype;
use crate::members::Types;

/// `member` with every member that overrides it or that it overrides,
/// through the whole hierarchy — library ones included (they make a
/// rename impossible). Just `member` when it isn't a class's.
pub fn override_family(db: &dyn SourceDatabase, member: ItemLocation) -> Vec<ItemLocation> {
    let tree = item_tree(db, member.container);
    let item = tree.item(member.item);
    let (Some(class), ItemContainer::File(file)) = (item.parent, member.container) else {
        return vec![member];
    };
    let name = item.name;
    let mut roots = vec![member];
    roots.extend(Types::new(db, file).overridden(member));
    let root_classes: Vec<ItemLocation> = roots
        .iter()
        .filter_map(|root| {
            let parent = item_tree(db, root.container).item(root.item).parent?;
            Some(ItemLocation {
                container: root.container,
                item: parent,
            })
        })
        .chain(std::iter::once(ItemLocation::in_file(file, class)))
        .collect();
    let mut family = roots.clone();
    for subclass in workspace_subclasses(db, &root_classes) {
        let ItemContainer::File(subclass_file) = subclass.container else {
            continue;
        };
        let types = Types::new(db, subclass_file);
        let subclass_tree = file_item_tree(db, subclass_file);
        for candidate in subclass_tree.named(Some(subclass.item), name) {
            let candidate = ItemLocation::in_file(subclass_file, *candidate);
            let overrides_a_root = types
                .overridden(candidate)
                .iter()
                .any(|overridden| roots.contains(overridden));
            if overrides_a_root && !family.contains(&candidate) {
                family.push(candidate);
            }
        }
    }
    family
}

/// The members `member` overrides directly, nearest first: in each
/// supertype branch the nearest — one it overrides only through another of
/// them is left out (`C.f` overrides `B.f`, not `A.f` that `B.f` overrides).
/// Nothing for a member of no class, or one overriding nothing.
pub fn overridden_directly(db: &dyn SourceDatabase, member: ItemLocation) -> Vec<ItemLocation> {
    let ItemContainer::File(file) = member.container else {
        return Vec::new();
    };
    let types = Types::new(db, file);
    let mut direct: Vec<ItemLocation> = Vec::new();
    for candidate in types.overridden(member) {
        let through_another = direct
            .iter()
            .any(|kept| types.overridden(*kept).contains(&candidate));
        if !through_another {
            direct.push(candidate);
        }
    }
    direct
}

/// What implements `declaration` in the workspace: a class's or an
/// interface's subclasses, directly or not; a function's or property's
/// overrides in those, at any depth. Nothing for anything else.
pub fn implementations(db: &dyn SourceDatabase, declaration: ItemLocation) -> Vec<ItemLocation> {
    let tree = item_tree(db, declaration.container);
    let item = tree.item(declaration.item);
    match item.kind {
        ItemKind::Class(_) => workspace_subclasses(db, &[declaration]),
        ItemKind::Function | ItemKind::Property { .. } => overrides_of(db, declaration),
        _ => Vec::new(),
    }
}

/// The workspace members that override `member`, directly or not.
fn overrides_of(db: &dyn SourceDatabase, member: ItemLocation) -> Vec<ItemLocation> {
    let tree = item_tree(db, member.container);
    let item = tree.item(member.item);
    let Some(parent) = item.parent else {
        return Vec::new();
    };
    let class = ItemLocation {
        container: member.container,
        item: parent,
    };
    let mut found = Vec::new();
    for subclass in workspace_subclasses(db, &[class]) {
        let ItemContainer::File(subclass_file) = subclass.container else {
            continue;
        };
        let types = Types::new(db, subclass_file);
        let subclass_tree = file_item_tree(db, subclass_file);
        for candidate in subclass_tree.named(Some(subclass.item), item.name) {
            let candidate = ItemLocation::in_file(subclass_file, *candidate);
            if types.overridden(candidate).contains(&member) && !found.contains(&candidate) {
                found.push(candidate);
            }
        }
    }
    found
}

/// The workspace classes that extend any of `classes`, directly or not.
fn workspace_subclasses(db: &dyn SourceDatabase, classes: &[ItemLocation]) -> Vec<ItemLocation> {
    let mut known: FxHashSet<ItemLocation> = classes.iter().copied().collect();
    let mut found = Vec::new();
    let mut frontier: Vec<ItemLocation> = classes.to_vec();
    let files = workspace_files(db);
    while !frontier.is_empty() {
        let next = subclasses_among(db, &files, &frontier, &known);
        known.extend(next.iter().copied());
        found.extend(next.iter().copied());
        frontier = next;
    }
    found
}

/// The workspace classes that name `class` among their supertypes.
pub fn direct_subclasses(db: &dyn SourceDatabase, class: ItemLocation) -> Vec<ItemLocation> {
    let known: FxHashSet<ItemLocation> = [class].into_iter().collect();
    subclasses_among(db, &workspace_files(db), &[class], &known)
}

fn workspace_files(db: &dyn SourceDatabase) -> Vec<SourceFile> {
    db.workspace()
        .source_sets(db)
        .iter()
        .flat_map(|source_set| source_set.files(db).iter().copied())
        .collect()
}

/// The classes of `files`, not `known` yet, that name one of `classes`
/// among their supertypes.
fn subclasses_among(
    db: &dyn SourceDatabase,
    files: &[SourceFile],
    classes: &[ItemLocation],
    known: &FxHashSet<ItemLocation>,
) -> Vec<ItemLocation> {
    let names: FxHashSet<Name> = classes
        .iter()
        .map(|class| item_tree(db, class.container).item(class.item).name)
        .collect();
    let mut found = Vec::new();
    for file in files {
        let tree = file_item_tree(db, *file);
        for (index, item) in tree.indexed_items() {
            let Signature::Class { supertypes, .. } = &item.signature else {
                continue;
            };
            let location = ItemLocation::in_file(*file, index);
            if known.contains(&location) || found.contains(&location) {
                continue;
            }
            // Only a supertype spelled with one of the names is resolved.
            let extends_one = supertypes
                .iter()
                .filter(|supertype| {
                    supertype
                        .path()
                        .and_then(|path| path.last())
                        .is_some_and(|last| names.contains(&last))
                })
                .any(|supertype| {
                    lower_supertype(db, *file, location, supertype)
                        .is_some_and(|resolved| classes.contains(&resolved.class))
                });
            if extends_one {
                found.push(location);
            }
        }
    }
    found
}
