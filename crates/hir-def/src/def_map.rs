//! Per source set: the top-level declarations of each package.
//!
//! A `DefMap` holds only locations, never ranges or signatures, so editing a
//! file recomputes its source set's map but leaves it equal — and everything
//! that depends on the map untouched — unless a top-level declaration
//! appeared, vanished or was renamed.

use std::sync::Arc;

use base_db::{Library, SourceDatabase, SourceFile, SourceSet};
use intern::{Name, Path};
use rustc_hash::FxHashMap;

use crate::item_tree::{ItemTree, Visibility};
use crate::library::library_package_tree;
use crate::{ItemContainer, ItemLocation, file_item_tree};

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
            .push(ItemLocation::in_file(file, *index));
    }
}

/// The top-level declarations named `name` in `package` that `from` sees.
///
/// Workspace sources first: its source set's and the source sets it depends
/// on, minus what visibility hides (`private` outside its file, `internal`
/// outside its module). Only when none has the name, libraries: the JDK,
/// then the source set's classpath in order — the first library that
/// declares the name wins, as it does for the compilers.
pub fn visible_top_level(
    db: &dyn SourceDatabase,
    from: SourceFile,
    package: &Path,
    name: Name,
) -> Vec<ItemLocation> {
    let mut found = Vec::new();
    for source_set in visible_source_sets(db, from).iter() {
        for location in source_set_def_map(db, *source_set).declarations(package, name) {
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
                    found.push(ItemLocation::in_file(from, *index));
                }
            }
        }
    }
    if !found.is_empty() {
        return found;
    }
    for library in libraries_of(db, from) {
        let Some(library_package) = library.packages(db).get(package).copied() else {
            continue;
        };
        if !library_package.declares(db, name) {
            continue;
        }
        let tree = library_package_tree(db, library_package);
        let declared: Vec<ItemLocation> = tree
            .top_level
            .iter()
            .filter(|index| tree.item(**index).name == name)
            .map(|index| ItemLocation {
                container: ItemContainer::Library(library_package),
                item: *index,
            })
            .collect();
        if !declared.is_empty() {
            return declared;
        }
    }
    Vec::new()
}

/// The libraries `from` compiles against: the JDK, then its source set's
/// classpath in order.
pub fn libraries_of(db: &dyn SourceDatabase, from: SourceFile) -> Vec<Library> {
    let classpaths = db.classpaths();
    let mut libraries: Vec<Library> = classpaths.jdk(db).into_iter().collect();
    if let Some(own) = from.source_set(db)
        && let Some(classpath) = classpaths.by_source_set(db).get(own.0 as usize)
    {
        libraries.extend(classpath.iter().copied());
    }
    libraries
}

/// Whether any source set or library `from` sees declares the package.
pub fn package_exists(db: &dyn SourceDatabase, from: SourceFile, package: &Path) -> bool {
    let in_sources = visible_source_sets(db, from)
        .iter()
        .any(|source_set| source_set_def_map(db, *source_set).has_package(package));
    in_sources
        || libraries_of(db, from)
            .into_iter()
            .any(|library| library.packages(db).contains_key(package))
}

/// The inputs of the source sets `from` sees, its own first.
pub fn visible_source_sets(db: &dyn SourceDatabase, from: SourceFile) -> Arc<[SourceSet]> {
    let Some(own) = from.source_set(db) else {
        return Arc::new([]);
    };
    match db.workspace().source_sets(db).get(own.0 as usize) {
        Some(source_set) => source_set_visibility(db, *source_set),
        None => Arc::new([]),
    }
}

/// `source_set` and every source set it sees — computed once per source set
/// rather than walked again for every name looked up.
#[salsa::tracked(returns(clone))]
pub fn source_set_visibility(db: &dyn SourceDatabase, source_set: SourceSet) -> Arc<[SourceSet]> {
    let workspace = db.workspace();
    let model = workspace.model(db);
    let inputs = workspace.source_sets(db);
    model
        .visible_source_sets(source_set.id(db))
        .into_iter()
        .filter_map(|id| inputs.get(id.0 as usize).copied())
        .collect()
}

fn is_visible(db: &dyn SourceDatabase, from: SourceFile, location: ItemLocation) -> bool {
    // Libraries only expose their public and protected declarations.
    let Some(file) = location.file() else {
        return true;
    };
    let tree = file_item_tree(db, file);
    match tree.item(location.item).visibility {
        Visibility::Private => file == from,
        Visibility::Internal => same_module(db, from, file),
        Visibility::PackagePrivate => file_item_tree(db, from).package == tree.package,
        Visibility::Public | Visibility::Protected => true,
    }
}

fn same_module(db: &dyn SourceDatabase, left: SourceFile, right: SourceFile) -> bool {
    let model = db.workspace().model(db);
    let module = |file: SourceFile| file.source_set(db).map(|id| model.source_set(id).module);
    module(left).is_some() && module(left) == module(right)
}
