//! An item tree as it is written to disk (a library package's, see
//! `library_tree_store`): its declarations, without the lookups built from
//! them, which reading builds again. Names and paths are written as text.

use std::sync::Arc;

use intern::Path;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::{Contract, Deprecation, Import, Item, ItemId, ItemTree, OptInMarker};

#[derive(Serialize)]
struct WrittenTree<'tree> {
    package: &'tree Path,
    source_files: &'tree [(ItemId, Box<str>)],
    compiled_classes: Vec<(ItemId, &'tree str)>,
    imports: &'tree [Import],
    items: &'tree [Item],
    ids: &'tree [ItemId],
    top_level: &'tree [ItemId],
    deprecations: &'tree [(ItemId, Deprecation)],
    annotations: &'tree [(ItemId, Box<[Path]>)],
    opt_in_markers: &'tree [(ItemId, OptInMarker)],
    contracts: &'tree [(ItemId, Contract)],
}

#[derive(Deserialize)]
struct ReadTree {
    package: Path,
    source_files: Box<[(ItemId, Box<str>)]>,
    compiled_classes: Vec<(ItemId, Box<str>)>,
    imports: Box<[Import]>,
    items: Box<[Item]>,
    ids: Box<[ItemId]>,
    top_level: Box<[ItemId]>,
    deprecations: Vec<(ItemId, Deprecation)>,
    annotations: Vec<(ItemId, Box<[Path]>)>,
    opt_in_markers: Vec<(ItemId, OptInMarker)>,
    contracts: Vec<(ItemId, Contract)>,
}

impl ItemTree {
    /// The tree as bytes to keep on disk.
    pub(crate) fn to_bytes(&self) -> Option<Vec<u8>> {
        let written = WrittenTree {
            package: &self.package,
            source_files: &self.source_files,
            compiled_classes: self
                .compiled_classes
                .iter()
                .map(|(item, class)| (*item, &**class))
                .collect(),
            imports: &self.imports,
            items: &self.items,
            ids: &self.ids,
            top_level: &self.top_level,
            deprecations: self.deprecations.entries(),
            annotations: self.annotations.entries(),
            opt_in_markers: self.opt_in_markers.entries(),
            contracts: self.contracts.entries(),
        };
        postcard::to_stdvec(&written).ok()
    }

    /// The tree `to_bytes` wrote, or `None` for bytes that aren't one.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<ItemTree> {
        let read: ReadTree = postcard::from_bytes(bytes).ok()?;
        let positions: FxHashMap<ItemId, u32> = read
            .ids
            .iter()
            .enumerate()
            .map(|(position, id)| (*id, position as u32))
            .collect();
        let every_item_has_an_id = read.ids.len() == read.items.len();
        let ids_are_unique = positions.len() == read.ids.len();
        if !every_item_has_an_id || !ids_are_unique {
            return None;
        }
        let named = ItemTree::named_items(&read.ids, &read.items);
        Some(ItemTree {
            package: read.package,
            source_files: read.source_files,
            compiled_classes: shared_class_names(read.compiled_classes),
            imports: read.imports,
            items: read.items,
            ids: read.ids,
            positions,
            named,
            top_level: read.top_level,
            deprecations: read.deprecations.into_iter().collect(),
            annotations: read.annotations.into_iter().collect(),
            opt_in_markers: read.opt_in_markers.into_iter().collect(),
            contracts: read.contracts.into_iter().collect(),
        })
    }
}

/// The members of one class file share its name, as lowering made them.
fn shared_class_names(classes: Vec<(ItemId, Box<str>)>) -> Box<[(ItemId, Arc<str>)]> {
    let mut shared: FxHashMap<Box<str>, Arc<str>> = FxHashMap::default();
    classes
        .into_iter()
        .map(|(item, class)| {
            let name = shared
                .entry(class)
                .or_insert_with_key(|class| Arc::from(&**class))
                .clone();
            (item, name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_of_one_class_file_share_its_name_when_read() {
        let classes = vec![
            (ItemId(1), "kotlin/TuplesKt".into()),
            (ItemId(2), "kotlin/TuplesKt".into()),
            (ItemId(3), "kotlin/StringsKt".into()),
        ];
        let shared = shared_class_names(classes);
        assert!(Arc::ptr_eq(&shared[0].1, &shared[1].1));
        assert!(!Arc::ptr_eq(&shared[0].1, &shared[2].1));
        assert_eq!(&*shared[2].1, "kotlin/StringsKt");
    }
}
