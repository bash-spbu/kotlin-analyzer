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
pub mod resolver;

use std::sync::Arc;

use base_db::{Language, SourceDatabase, SourceFile};

pub use def_map::{DefMap, source_set_def_map};
pub use item_tree::{ItemIndex, ItemTree};

/// A declaration anywhere in the workspace: a file and an item in it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ItemLocation {
    pub file: SourceFile,
    pub item: ItemIndex,
}

/// The declarations of a file.
#[salsa::tracked(returns(clone))]
pub fn file_item_tree(db: &dyn SourceDatabase, file: SourceFile) -> Arc<ItemTree> {
    let parse = base_db::parse(db, file);
    let tree = match file.language(db) {
        Language::Kotlin => item_tree::lower_kotlin(parse),
        Language::Java => item_tree::lower_java(parse),
    };
    Arc::new(tree)
}

#[cfg(test)]
mod tests;
