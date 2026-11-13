//! Per source set: the top-level declarations of each package.
//!
//! A `DefMap` holds only locations, never ranges or signatures, so editing a
//! file recomputes its source set's map but leaves it equal — and everything
//! that depends on the map untouched — unless a top-level declaration
//! appeared, vanished or was renamed.

use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile, SourceSet};
use intern::{Name, Path};
use rustc_hash::FxHashMap;

use crate::item_tree::{ItemTree, Visibility};
use crate::{ItemLocation, file_item_tree};

#[derive(PartialEq, Eq, Debug, Default)]
pub struct DefMap {
    packages: FxHashMap<Path, FxHashMap<Name, Vec<ItemLocation>>>,
}

impl DefMap {
    /// The top-level declarations named `name` in `package`.
    pub fn declarations(&self, package: &Path, name: Name) -> &[ItemLocation] {
        self.packages
            .get(package)
            .and_then(|scope| scope.get(&name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn has_package(&self, package: &Path) -> bool {
        self.packages.contains_key(package)
    }

    /// Every top-level declaration of `package`.
    pub fn package_declarations(&self, package: &Path) -> impl Iterator<Item = &ItemLocation> {
        self.packages
            .get(package)
            .into_iter()
            .flat_map(|scope| scope.values().flatten())
    }

    pub fn packages(&self) -> impl Iterator<Item = &Path> {
        self.packages.keys()
    }
}

#[salsa::tracked(returns(clone))]
pub fn source_set_def_map(db: &dyn SourceDatabase, source_set: SourceSet) -> Arc<DefMap> {
    let mut map = DefMap::default();
    for file in source_set.files(db).iter() {
        let tree = file_item_tree(db, *file);
        add_file(&mut map, *file, &tree);
    }
    Arc::new(map)
}

fn add_file(map: &mut DefMap, file: SourceFile, tree: &ItemTree) {
    let scope = map.packages.entry(tree.package.clone()).or_default();
    for index in tree.top_level.iter() {
        let item = tree.item(*index);
        scope
            .entry(item.name)
            .or_default()
            .push(ItemLocation { file, item: *index });
    }
}

/// The top-level declarations named `name` in `package` that `from` sees:
/// its source set's and the source sets it depends on, minus what their
/// visibility hides (`private` outside its file, `internal` outside its
/// module).
pub fn visible_top_level(
    db: &dyn SourceDatabase,
    from: SourceFile,
    package: &Path,
    name: Name,
) -> Vec<ItemLocation> {
    let mut found = Vec::new();
    for source_set in visible_source_sets(db, from) {
        for location in source_set_def_map(db, source_set).declarations(package, name) {
            if is_visible(db, from, *location) {
                found.push(*location);
            }
        }
    }
    if found.is_empty() && from.source_set(db).is_none() {
        // A file outside every source set sees its own declarations.
        let tree = file_item_tree(db, from);
        if tree.package == *package {
            for index in tree.top_level.iter() {
                if tree.item(*index).name == name {
                    found.push(ItemLocation {
                        file: from,
                        item: *index,
                    });
                }
            }
        }
    }
    found
}

/// Whether any source set `from` sees declares the package `package`.
pub fn package_exists(db: &dyn SourceDatabase, from: SourceFile, package: &Path) -> bool {
    visible_source_sets(db, from)
        .into_iter()
        .any(|source_set| source_set_def_map(db, source_set).has_package(package))
}

/// The inputs of the source sets `from` sees, its own first.
pub fn visible_source_sets(db: &dyn SourceDatabase, from: SourceFile) -> Vec<SourceSet> {
    let Some(own) = from.source_set(db) else {
        return Vec::new();
    };
    let workspace = db.workspace();
    let model = workspace.model(db);
    let inputs = workspace.source_sets(db);
    model
        .visible_source_sets(own)
        .into_iter()
        .filter_map(|id| inputs.get(id.0 as usize).copied())
        .collect()
}

fn is_visible(db: &dyn SourceDatabase, from: SourceFile, location: ItemLocation) -> bool {
    let tree = file_item_tree(db, location.file);
    match tree.item(location.item).visibility {
        Visibility::Private => location.file == from,
        Visibility::Internal => same_module(db, from, location.file),
        Visibility::PackagePrivate => file_item_tree(db, from).package == tree.package,
        Visibility::Public | Visibility::Protected => true,
    }
}

fn same_module(db: &dyn SourceDatabase, left: SourceFile, right: SourceFile) -> bool {
    let model = db.workspace().model(db).clone();
    let module = |file: SourceFile| file.source_set(db).map(|id| model.source_set(id).module);
    module(left).is_some() && module(left) == module(right)
}
