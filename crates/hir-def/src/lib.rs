//! Definitions and name resolution.
//!
//! - [`file_item_tree`]: one file's declarations with their signatures — no
//!   bodies — so editing inside a function doesn't invalidate anything that
//!   only needs declarations.
//! - [`source_set_def_map`]: per source set, which top-level declarations each package
//!   holds.
//! - [`resolver`]: which declaration a written name means at a position,
//!   following Kotlin's scope levels and the source set's visibility.

pub mod decompiled;
pub mod def_map;
pub mod expect_actual;
pub mod item_tree;
pub mod library;
pub mod library_tree_store;
pub mod resolver;

use std::sync::Arc;

use base_db::{Language, LibraryPackage, SourceDatabase, SourceFile};

pub use def_map::{DefMap, source_set_def_map};
pub use item_tree::{ItemId, ItemRanges, ItemSourceMap, ItemTree};

/// Where declarations come from: a workspace file, or a package of a
/// library.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ItemContainer {
    File(SourceFile),
    Library(LibraryPackage),
}

/// A declaration anywhere: its container and its item there.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ItemLocation {
    pub container: ItemContainer,
    pub item: ItemId,
}

impl ItemLocation {
    pub fn in_file(file: SourceFile, item: ItemId) -> ItemLocation {
        ItemLocation {
            container: ItemContainer::File(file),
            item,
        }
    }

    /// The workspace file that declares it; `None` for a library.
    pub fn file(self) -> Option<SourceFile> {
        match self.container {
            ItemContainer::File(file) => Some(file),
            ItemContainer::Library(_) => None,
        }
    }
}

/// The declarations of a container.
pub fn item_tree(db: &dyn SourceDatabase, container: ItemContainer) -> Arc<ItemTree> {
    match container {
        ItemContainer::File(file) => file_item_tree(db, file),
        ItemContainer::Library(package) => library::library_package_tree(db, package),
    }
}

/// Where a container's declarations are in its text (nowhere, for a
/// library's).
pub fn item_source_map(db: &dyn SourceDatabase, container: ItemContainer) -> Arc<ItemSourceMap> {
    match container {
        ItemContainer::File(file) => file_item_source_map(db, file),
        ItemContainer::Library(_) => Arc::default(),
    }
}

/// A file's declarations and where they are, lowered together.
///
/// Parses the file itself instead of asking the `parse` query: a syntax tree
/// is many times the size of its text, and the query would keep every one
/// it produced until the next revision. Features that need a tree go through
/// `parse`, which keeps only recently used ones.
#[salsa::tracked(returns(clone))]
fn lowered_file(db: &dyn SourceDatabase, file: SourceFile) -> (Arc<ItemTree>, Arc<ItemSourceMap>) {
    let parse = syntax::parse(file.text(db).clone(), file.language(db));
    let (tree, source_map) = match file.language(db) {
        Language::Kotlin => item_tree::lower_kotlin(&parse),
        Language::Java => item_tree::lower_java(&parse),
    };
    (Arc::new(tree), Arc::new(source_map))
}

/// The declarations of a file. An edit that leaves them as they were — in
/// a body, or one that only moves them — leaves this unchanged, and
/// nothing that depends on it alone is recomputed.
#[salsa::tracked(returns(clone))]
pub fn file_item_tree(db: &dyn SourceDatabase, file: SourceFile) -> Arc<ItemTree> {
    lowered_file(db, file).0
}

/// Where a file's declarations are: changes with almost every edit.
#[salsa::tracked(returns(clone))]
pub fn file_item_source_map(db: &dyn SourceDatabase, file: SourceFile) -> Arc<ItemSourceMap> {
    lowered_file(db, file).1
}

#[cfg(test)]
mod tests;
