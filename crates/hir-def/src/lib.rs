//! Definitions and name resolution.
//!
//! - [`file_item_tree`]: one file's declarations with their signatures — no
//!   bodies — so editing inside a function doesn't invalidate anything that
//!   only needs declarations.
//! - [`source_set_def_map`]: per source set, which top-level declarations each package
//!   holds.
//! - [`resolver`]: which declaration a written name means at a position,
//!   following Kotlin's scope levels and the source set's visibility.

pub mod def_map;
pub mod item_tree;
pub mod library;
pub mod resolver;

use std::sync::Arc;

use base_db::{Language, LibraryPackage, SourceDatabase, SourceFile};

pub use def_map::{DefMap, source_set_def_map};
pub use item_tree::{ItemIndex, ItemTree};

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
    pub item: ItemIndex,
}

impl ItemLocation {
    pub fn in_file(file: SourceFile, item: ItemIndex) -> ItemLocation {
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

/// The declarations of a file.
///
/// Parses the file itself instead of asking the `parse` query: a syntax tree
/// is many times the size of its text, and the query would keep every one
/// it produced until the next revision. Features that need a tree go through
/// `parse`, which keeps only recently used ones.
#[salsa::tracked(returns(clone))]
pub fn file_item_tree(db: &dyn SourceDatabase, file: SourceFile) -> Arc<ItemTree> {
    let parse = syntax::parse(file.text(db).clone(), file.language(db));
    let tree = match file.language(db) {
        Language::Kotlin => item_tree::lower_kotlin(&parse),
        Language::Java => item_tree::lower_java(&parse),
    };
    Arc::new(tree)
}

#[cfg(test)]
mod tests;
