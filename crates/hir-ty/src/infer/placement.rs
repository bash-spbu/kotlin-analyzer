//! Where the syntax inference walks sits: a whole file's tree, or one
//! body's snippet (see `units`) — whose offsets are its own, so what's
//! asked by position (which declaration a node is, which types are in
//! scope) is asked of the snippet's items instead of the file's.

use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::ItemId;
use hir_def::resolver::Resolution;
use intern::Path;
use rustc_hash::FxHashMap;
use syntax::TextRange;

use crate::lower::TypeScope;

pub(crate) enum Placement {
    /// The file's own tree: positions are the file's.
    File {
        items_by_range: FxHashMap<TextRange, ItemId>,
    },
    /// A body's snippet: the items in it, by their range there.
    Unit {
        items: Arc<[(TextRange, ItemId)]>,
        items_by_range: FxHashMap<TextRange, ItemId>,
    },
}

impl Placement {
    pub(crate) fn file(db: &dyn SourceDatabase, file: SourceFile) -> Placement {
        let tree = hir_def::file_item_tree(db, file);
        let source_map = hir_def::file_item_source_map(db, file);
        let items_by_range = tree
            .indexed_items()
            .map(|(item, _)| (source_map.ranges(item).range, item))
            .collect();
        Placement::File { items_by_range }
    }

    pub(crate) fn unit(items: Arc<[(TextRange, ItemId)]>) -> Placement {
        let items_by_range = items.iter().map(|(range, item)| (*range, *item)).collect();
        Placement::Unit {
            items,
            items_by_range,
        }
    }

    /// The declaration spanning exactly `range`.
    pub(super) fn item_spanning(&self, range: TextRange) -> Option<ItemId> {
        let (Placement::File { items_by_range } | Placement::Unit { items_by_range, .. }) = self;
        items_by_range.get(&range).copied()
    }

    /// Where a type written at `offset` is read.
    pub(super) fn type_scope(
        &self,
        db: &dyn SourceDatabase,
        file: SourceFile,
        offset: usize,
    ) -> TypeScope {
        match self {
            Placement::File { .. } => TypeScope::written_at(db, file, offset),
            Placement::Unit { items, .. } => TypeScope::inside(db, file, innermost(items, offset)),
        }
    }

    /// What the type name `path` written at `offset` refers to.
    pub(super) fn resolve_type_path(
        &self,
        db: &dyn SourceDatabase,
        file: SourceFile,
        path: &Path,
        offset: usize,
    ) -> Resolution {
        match self {
            Placement::File { .. } => hir_def::resolver::resolve_type_path(db, file, path, offset),
            Placement::Unit { items, .. } => hir_def::resolver::resolve_type_path_inside(
                db,
                file,
                path,
                innermost(items, offset),
            ),
        }
    }
}

/// The smallest of `items` whose range holds `offset`.
fn innermost(items: &[(TextRange, ItemId)], offset: usize) -> Option<ItemId> {
    items
        .iter()
        .filter(|(range, _)| range.contains(offset))
        .min_by_key(|(range, _)| range.end - range.start)
        .map(|(_, item)| *item)
}
