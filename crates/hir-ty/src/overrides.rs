//! Which members are one member as far as renaming goes: a function or
//! property, what it overrides, and what overrides those in the workspace.

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::Signature;
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

/// The workspace classes that extend any of `classes`, directly or not.
fn workspace_subclasses(db: &dyn SourceDatabase, classes: &[ItemLocation]) -> Vec<ItemLocation> {
    let mut known: FxHashSet<ItemLocation> = classes.iter().copied().collect();
    let mut found = Vec::new();
    let mut frontier: Vec<ItemLocation> = classes.to_vec();
    let files: Vec<SourceFile> = db
        .workspace()
        .source_sets(db)
        .iter()
        .flat_map(|source_set| source_set.files(db).iter().copied())
        .collect();
    while !frontier.is_empty() {
        let names: FxHashSet<Name> = frontier
            .iter()
            .map(|class| item_tree(db, class.container).item(class.item).name)
            .collect();
        let mut next = Vec::new();
        for file in &files {
            let tree = file_item_tree(db, *file);
            for (index, item) in tree.indexed_items() {
                let Signature::Class { supertypes, .. } = &item.signature else {
                    continue;
                };
                let location = ItemLocation::in_file(*file, index);
                if known.contains(&location) {
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
                            .is_some_and(|resolved| frontier.contains(&resolved.class))
                    });
                if extends_one {
                    known.insert(location);
                    next.push(location);
                }
            }
        }
        found.extend(next.iter().copied());
        frontier = next;
    }
    found
}
