//! Per source set: the top-level declarations of each package.
//!
//! A `DefMap` holds only locations, never ranges or signatures, so editing a
//! file recomputes its source set's map but leaves it equal — and everything
//! that depends on the map untouched — unless a top-level declaration
//! appeared, vanished or was renamed.

use std::sync::Arc;

use base_db::{Library, LibraryPackage, SourceDatabase, SourceFile, SourceSet};
use intern::{Name, Path};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::item_tree::{ItemTree, Visibility};
use crate::library::library_package_tree;
use crate::{ItemContainer, ItemLocation, file_item_tree};

#[derive(PartialEq, Eq, Debug, Default)]
pub struct DefMap {
    packages: FxHashMap<Path, FxHashMap<Name, Vec<ItemLocation>>>,
    /// Every package and every prefix of one (`a`, `a.b` for `a.b.c`).
    package_prefixes: FxHashSet<Path>,
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

    /// The top-level names of `package`.
    pub fn package_names(&self, package: &Path) -> impl Iterator<Item = Name> + '_ {
        self.packages
            .get(package)
            .into_iter()
            .flat_map(|scope| scope.keys().copied())
    }

    /// Every package with its top-level names.
    pub fn names(&self) -> impl Iterator<Item = (&Path, Name)> {
        self.packages
            .iter()
            .flat_map(|(package, scope)| scope.keys().map(move |name| (package, *name)))
    }

    /// Whether `path` is a package or the start of one.
    pub fn has_package_prefix(&self, path: &Path) -> bool {
        self.package_prefixes.contains(path)
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
    add_prefixes(&mut map.package_prefixes, &tree.package);
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
    let mut found: Vec<ItemLocation> = match source_set_input(db, from) {
        Some(source_set) => workspace_top_level(db, source_set, package, name)
            .into_iter()
            .filter(|location| is_visible(db, from, *location))
            .collect(),
        None => Vec::new(),
    };
    if found.is_empty() && from.source_set(db).is_none() {
        // A file outside every source set sees its own declarations.
        let tree = file_item_tree(db, from);
        if tree.package == *package {
            for index in tree.named(None, name) {
                found.push(ItemLocation::in_file(from, *index));
            }
        }
    }
    if !found.is_empty() {
        return found;
    }
    for library_package in classpath_packages(db, from, package) {
        if !library_package.declares(db, name) {
            continue;
        }
        let tree = library_package_tree(db, library_package);
        let declared: Vec<ItemLocation> = tree
            .named(None, name)
            .iter()
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

/// The def maps of the source sets `source_set` sees, its own first —
/// fetched once, so a lookup is plain hashing rather than a query per
/// source set. (Memoising lookups by package and name instead would intern
/// every combination ever asked, which Salsa never frees.)
#[salsa::tracked(returns(clone))]
pub fn visible_def_maps(db: &dyn SourceDatabase, source_set: SourceSet) -> Arc<[Arc<DefMap>]> {
    source_set_visibility(db, source_set)
        .iter()
        .map(|visible| source_set_def_map(db, *visible))
        .collect()
}

/// The top-level declarations named `name` in `package` of the source sets
/// `source_set` sees, before visibility (which depends on the asking file)
/// filters them.
pub fn workspace_top_level(
    db: &dyn SourceDatabase,
    source_set: SourceSet,
    package: &Path,
    name: Name,
) -> Vec<ItemLocation> {
    visible_def_maps(db, source_set)
        .iter()
        .flat_map(|def_map| def_map.declarations(package, name).iter().copied())
        .collect()
}

/// Every library's packages, by package name: which libraries declare
/// each (a package is usually in one or two).
#[derive(PartialEq, Eq, Debug, Default)]
pub struct LibraryPackages {
    by_package: FxHashMap<Path, Vec<LibraryPackage>>,
    /// Every package and every prefix of one.
    prefixes: FxHashSet<Path>,
}

#[salsa::tracked(returns(clone))]
pub fn library_packages(db: &dyn SourceDatabase) -> Arc<LibraryPackages> {
    let classpaths = db.classpaths();
    let mut libraries: Vec<Library> = classpaths.jdk(db).into_iter().collect();
    for classpath in classpaths.by_source_set(db).iter() {
        libraries.extend(classpath.iter().copied());
    }
    let mut seen = FxHashSet::default();
    let mut index = LibraryPackages::default();
    for library in libraries {
        if !seen.insert(library) {
            continue;
        }
        for (package, library_package) in library.packages(db).iter() {
            index
                .by_package
                .entry(package.clone())
                .or_default()
                .push(*library_package);
            add_prefixes(&mut index.prefixes, package);
        }
    }
    Arc::new(index)
}

/// Every library's top-level names, with the packages declaring each.
#[salsa::tracked(returns(clone))]
pub fn library_names(db: &dyn SourceDatabase) -> Arc<FxHashMap<Name, Vec<LibraryPackage>>> {
    let index = library_packages(db);
    let mut names: FxHashMap<Name, Vec<LibraryPackage>> = FxHashMap::default();
    for declaring in index.by_package.values() {
        for library_package in declaring {
            for name in library_package.names(db).iter() {
                names.entry(*name).or_default().push(*library_package);
            }
        }
    }
    Arc::new(names)
}

/// The top-level names of the source sets `source_set` sees, with the
/// packages declaring each.
#[salsa::tracked(returns(clone))]
pub fn workspace_names(
    db: &dyn SourceDatabase,
    source_set: SourceSet,
) -> Arc<FxHashMap<Name, Vec<Path>>> {
    let mut names: FxHashMap<Name, Vec<Path>> = FxHashMap::default();
    for visible in source_set_visibility(db, source_set).iter() {
        for (package, name) in source_set_def_map(db, *visible).names() {
            let packages = names.entry(name).or_default();
            if !packages.contains(package) {
                packages.push(package.clone());
            }
        }
    }
    Arc::new(names)
}

/// The packages with a top-level `name` that `from` sees — what an import
/// of the name could import.
pub fn packages_declaring(db: &dyn SourceDatabase, from: SourceFile, name: Name) -> Vec<Path> {
    let mut packages: Vec<Path> = Vec::new();
    if let Some(source_set) = source_set_input(db, from) {
        if let Some(declaring) = workspace_names(db, source_set).get(&name) {
            packages.extend(declaring.iter().cloned());
        }
        let positions = classpath_positions(db, source_set);
        if let Some(declaring) = library_names(db).get(&name) {
            for library_package in declaring {
                let on_classpath = positions.contains_key(&library_package.library(db));
                let package = library_package.package(db);
                if on_classpath && !packages.contains(package) {
                    packages.push(package.clone());
                }
            }
        }
    }
    packages
}

/// The top-level names of `package` that `from` sees (from its source
/// sets and classpath), each once.
pub fn package_names(db: &dyn SourceDatabase, from: SourceFile, package: &Path) -> Vec<Name> {
    let mut names: Vec<Name> = Vec::new();
    if let Some(source_set) = source_set_input(db, from) {
        for def_map in visible_def_maps(db, source_set).iter() {
            names.extend(def_map.package_names(package));
        }
    }
    for library_package in classpath_packages(db, from, package) {
        names.extend(library_package.names(db).iter().copied());
    }
    names.sort();
    names.dedup();
    names
}

/// Every top-level name with the packages declaring it that `from` sees —
/// what completion can offer to import.
pub fn importable_names(db: &dyn SourceDatabase, from: SourceFile) -> Vec<(Name, Path)> {
    let mut importable = Vec::new();
    let Some(source_set) = source_set_input(db, from) else {
        return importable;
    };
    for (name, packages) in workspace_names(db, source_set).iter() {
        importable.extend(packages.iter().map(|package| (*name, package.clone())));
    }
    let positions = classpath_positions(db, source_set);
    for (name, declaring) in library_names(db).iter() {
        for library_package in declaring {
            if positions.contains_key(&library_package.library(db)) {
                importable.push((*name, library_package.package(db).clone()));
            }
        }
    }
    importable
}

/// Where each library is on a source set's classpath (the JDK first).
#[salsa::tracked(returns(clone))]
pub fn classpath_positions(
    db: &dyn SourceDatabase,
    source_set: SourceSet,
) -> Arc<FxHashMap<Library, u32>> {
    let classpaths = db.classpaths();
    let classpath = classpaths
        .by_source_set(db)
        .get(source_set.id(db).0 as usize)
        .into_iter()
        .flat_map(|classpath| classpath.iter().copied());
    let mut positions = FxHashMap::default();
    for (position, library) in classpaths.jdk(db).into_iter().chain(classpath).enumerate() {
        positions.entry(library).or_insert(position as u32);
    }
    Arc::new(positions)
}

/// The packages named `package` of the libraries `from` compiles against,
/// in classpath order.
fn classpath_packages(
    db: &dyn SourceDatabase,
    from: SourceFile,
    package: &Path,
) -> Vec<LibraryPackage> {
    let index = library_packages(db);
    let Some(declaring) = index.by_package.get(package) else {
        return Vec::new();
    };
    let positions = match source_set_input(db, from) {
        Some(source_set) => classpath_positions(db, source_set),
        None => {
            let jdk = db.classpaths().jdk(db);
            return declaring
                .iter()
                .filter(|library_package| Some(library_package.library(db)) == jdk)
                .copied()
                .collect();
        }
    };
    let mut ordered: Vec<(u32, LibraryPackage)> = declaring
        .iter()
        .filter_map(|library_package| {
            positions
                .get(&library_package.library(db))
                .map(|position| (*position, *library_package))
        })
        .collect();
    ordered.sort_by_key(|(position, _)| *position);
    ordered
        .into_iter()
        .map(|(_, library_package)| library_package)
        .collect()
}

pub fn source_set_input(db: &dyn SourceDatabase, file: SourceFile) -> Option<SourceSet> {
    let id = file.source_set(db)?;
    db.workspace().source_sets(db).get(id.0 as usize).copied()
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

fn add_prefixes(prefixes: &mut FxHashSet<Path>, package: &Path) {
    let segments = package.segments();
    for length in 1..=segments.len() {
        prefixes.insert(Path::new(segments[..length].iter().copied()));
    }
}

/// Whether `path` is a package `from` sees, or the start of one — what a
/// qualified name's leading segments (`java` in `java.time.Instant`) are.
pub fn package_prefix_exists(db: &dyn SourceDatabase, from: SourceFile, path: &Path) -> bool {
    let in_sources = source_set_input(db, from).is_some_and(|source_set| {
        visible_def_maps(db, source_set)
            .iter()
            .any(|def_map| def_map.has_package_prefix(path))
    });
    in_sources || library_packages(db).prefixes.contains(path)
}

/// Whether any source set or library `from` sees declares the package.
pub fn package_exists(db: &dyn SourceDatabase, from: SourceFile, package: &Path) -> bool {
    let in_sources = source_set_input(db, from).is_some_and(|source_set| {
        visible_def_maps(db, source_set)
            .iter()
            .any(|def_map| def_map.has_package(package))
    });
    in_sources || !classpath_packages(db, from, package).is_empty()
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

/// Whether code in `from` can name `location` at all: the declaration's
/// source set is one `from`'s sees (a library's always is), and its
/// visibility lets `from` in. What a references search narrows its files by.
pub fn sees(db: &dyn SourceDatabase, from: SourceFile, location: ItemLocation) -> bool {
    let Some(declared_in) = location.file() else {
        return true;
    };
    let source_set_visible = match (
        source_set_input(db, from),
        source_set_input(db, declared_in),
    ) {
        (Some(own), Some(declaring)) => source_set_visibility(db, own).contains(&declaring),
        // A file outside every source set sees only itself.
        _ => from == declared_in,
    };
    source_set_visible && is_visible(db, from, location)
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
