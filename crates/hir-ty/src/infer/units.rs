//! Bodies inferred one at a time, as rust-analyzer infers each body on
//! its own: an edit inside one function re-infers that function, not its
//! file.
//!
//! A file splits into units (`file_units`): each function and secondary
//! constructor, each top-level property, each class — its header,
//! initializers, member properties and enum entries; its functions and
//! nested classes are units of their own — and what's left at the top
//! level (the file's annotations). A unit is inferred on a snippet of its
//! own (`unit_layout`): the text of the classes around it down to their
//! body's `{` (what's in scope there: receivers, constructor parameters),
//! then its own text with the units inside it left out. The snippet's text
//! and the items in it, by their range there, are what its inference reads
//! (`unit_text`, `unit_items`) — the same whatever moved around it, so an
//! edit elsewhere in the file leaves it as it was. `infer_file` puts the
//! units' results together at their places in the file.

use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::ItemId;
use hir_def::item_tree::ItemKind;
use rustc_hash::FxHashMap;
use syntax::{Language, SyntaxNode, TextRange, named_children};

use super::InferenceResult;
use super::declarations::Inferrer;
use super::placement::Placement;

/// A part of a file inferred on its own.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BodyUnit {
    /// What's outside every other unit: the file's annotations, its type
    /// aliases.
    File,
    /// A function, a secondary constructor, a top-level property, or a
    /// class without its own units.
    Item(ItemId),
}

/// How a file is inferred.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FileUnits {
    /// Unit by unit.
    Units(Arc<[BodyUnit]>),
    /// Whole: a script's statements run in order at its top level.
    Script,
}

/// The units `file` is inferred in.
#[salsa::tracked(returns(clone))]
pub fn file_units(db: &dyn SourceDatabase, file: SourceFile) -> FileUnits {
    let parse = base_db::parse(db, file);
    if super::declarations::is_script(&parse) {
        return FileUnits::Script;
    }
    let tree = hir_def::file_item_tree(db, file);
    let source_map = hir_def::file_item_source_map(db, file);
    let root = parse.root();
    let mut units = vec![BodyUnit::File];
    for (item, declared) in tree.indexed_items() {
        let inside_classes_only = tree
            .ancestors(item)
            .iter()
            .all(|ancestor| matches!(tree.item(*ancestor).kind, ItemKind::Class(_)));
        if !inside_classes_only {
            continue;
        }
        let Some(node) = node_of(root, source_map.ranges(item).range) else {
            continue;
        };
        let is_unit = match node.kind() {
            "function_declaration" | "secondary_constructor" => true,
            "class_declaration" | "object_declaration" | "companion_object" => true,
            "property_declaration" => declared.parent.is_none(),
            _ => false,
        };
        if is_unit {
            units.push(BodyUnit::Item(item));
        }
    }
    FileUnits::Units(units.into())
}

/// A unit's snippet: its text, the items in it and where its pieces are
/// in the file.
#[derive(PartialEq, Eq, Debug)]
pub struct UnitLayout {
    text: Arc<str>,
    /// Each item in the snippet, by its range there.
    items: Arc<[(TextRange, ItemId)]>,
    /// For an item unit, its own text in the snippet (a property's with
    /// the accessors written after it); for the file unit, the whole.
    own: TextRange,
    map: SnippetMap,
}

impl UnitLayout {
    /// `range` of the snippet as a range of the file.
    pub(crate) fn to_file(&self, range: TextRange) -> TextRange {
        self.map.range_to_file(range)
    }
}

#[salsa::tracked(returns(clone), lru = 1024)]
pub fn unit_layout(db: &dyn SourceDatabase, file: SourceFile, unit: BodyUnit) -> Arc<UnitLayout> {
    let parse = base_db::parse(db, file);
    let index = file_unit_index(db, file);
    Arc::new(build_layout(&parse, &index, unit))
}

/// Where a file's items and units are, for cutting its snippets.
#[derive(PartialEq, Eq, Debug)]
pub struct UnitIndex {
    /// Each item by its range, in text order (an outer one first).
    items: Vec<(TextRange, ItemId)>,
    /// Each item unit's text (a property's with its accessors), in text
    /// order.
    extents: Vec<(TextRange, ItemId)>,
}

#[salsa::tracked(returns(clone), lru = 64)]
fn file_unit_index(db: &dyn SourceDatabase, file: SourceFile) -> Arc<UnitIndex> {
    let parse = base_db::parse(db, file);
    let root = parse.root();
    let tree = hir_def::file_item_tree(db, file);
    let source_map = hir_def::file_item_source_map(db, file);
    let mut items: Vec<(TextRange, ItemId)> = tree
        .indexed_items()
        .map(|(item, _)| (source_map.ranges(item).range, item))
        .collect();
    items.sort_by_key(|(range, item)| (range.start, std::cmp::Reverse(range.end), *item));
    let mut extents: Vec<(TextRange, ItemId)> = match file_units(db, file) {
        FileUnits::Units(units) => units
            .iter()
            .filter_map(|unit| match unit {
                BodyUnit::Item(item) => {
                    let node = node_of(root, source_map.ranges(*item).range)?;
                    Some((extent(node), *item))
                }
                BodyUnit::File => None,
            })
            .collect(),
        FileUnits::Script => Vec::new(),
    };
    extents.sort_by_key(|(range, _)| range.start);
    Arc::new(UnitIndex { items, extents })
}

/// A unit's snippet text: what its inference parses.
#[salsa::tracked(returns(clone), lru = 1024)]
pub fn unit_text(db: &dyn SourceDatabase, file: SourceFile, unit: BodyUnit) -> Arc<str> {
    unit_layout(db, file, unit).text.clone()
}

/// The items in a unit's snippet, by their range there; and the unit's own
/// text there.
#[salsa::tracked(returns(clone), lru = 1024)]
pub fn unit_items(
    db: &dyn SourceDatabase,
    file: SourceFile,
    unit: BodyUnit,
) -> (Arc<[(TextRange, ItemId)]>, TextRange) {
    let layout = unit_layout(db, file, unit);
    (layout.items.clone(), layout.own)
}

/// What inference finds in one unit, at the unit's snippet's offsets.
#[salsa::tracked(returns(clone), lru = 512)]
pub fn infer_body(
    db: &dyn SourceDatabase,
    file: SourceFile,
    unit: BodyUnit,
) -> Arc<InferenceResult> {
    let (items, own) = unit_items(db, file, unit);
    let parse = syntax::parse(unit_text(db, file, unit), Language::Kotlin);
    let mut inferrer = Inferrer::placed(db, file, &parse, Placement::unit(items.clone()));
    let node = match unit {
        BodyUnit::File => None,
        BodyUnit::Item(item) => items
            .iter()
            .find(|(_, found)| *found == item)
            .and_then(|(range, _)| node_of(parse.root(), *range)),
    };
    inferrer.unit_declarations(node, own);
    Arc::new(inferrer.finish())
}

/// The type of `item` (a function or a property that doesn't write it),
/// inferred in a snippet of its own — the classes around it and its text,
/// as a unit's (a member property's is no unit: its class is); `None` when
/// the file isn't inferred unit by unit.
pub(super) fn unit_declaration_type(
    db: &dyn SourceDatabase,
    file: SourceFile,
    item: ItemId,
) -> Option<crate::ty::Ty> {
    if file_units(db, file) == FileUnits::Script {
        return None;
    }
    let unit = BodyUnit::Item(item);
    let (items, _) = unit_items(db, file, unit);
    let range = items.iter().find(|(_, found)| *found == item)?.0;
    let parse = syntax::parse(unit_text(db, file, unit), Language::Kotlin);
    let mut inferrer = Inferrer::placed(db, file, &parse, Placement::unit(items.clone()));
    Some(inferrer.declaration_type(range))
}

/// Every unit's result at its place in the file, as one.
pub(super) fn infer_units(
    db: &dyn SourceDatabase,
    file: SourceFile,
    units: &[BodyUnit],
) -> InferenceResult {
    let mut merged = InferenceResult::default();
    let mut locals_by_declaration: FxHashMap<(TextRange, intern::Name), super::LocalId> =
        FxHashMap::default();
    for unit in units {
        let result = infer_body(db, file, *unit);
        let layout = unit_layout(db, file, *unit);
        merged.absorb(&result, &layout, &mut locals_by_declaration);
    }
    merged
}

/// The node spanning exactly `range`.
pub(super) fn node_of(root: SyntaxNode<'_>, range: TextRange) -> Option<SyntaxNode<'_>> {
    let found = root.descendant_for_byte_range(range.start as usize, range.end as usize)?;
    let mut current = Some(found);
    while let Some(candidate) = current {
        if TextRange::of(candidate) == range {
            return Some(candidate);
        }
        if candidate.end_byte() - candidate.start_byte() > (range.end - range.start) as usize {
            return None;
        }
        current = candidate.parent();
    }
    None
}

// --- snippets -------------------------------------------------------------

/// Where a snippet's pieces are in the file.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct SnippetMap {
    /// Copied pieces, in snippet order.
    pieces: Vec<Piece>,
}

/// A piece of the file's text copied into a snippet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Piece {
    snippet_start: u32,
    file_start: u32,
    len: u32,
}

impl SnippetMap {
    /// The file offset of a snippet offset: in the piece holding it (an
    /// offset in text the snippet made up goes to the end of the piece
    /// before).
    fn offset_to_file(&self, offset: u32) -> u32 {
        let index = self
            .pieces
            .partition_point(|piece| piece.snippet_start <= offset);
        let Some(piece) = index.checked_sub(1).map(|index| self.pieces[index]) else {
            return self.pieces.first().map_or(offset, |piece| piece.file_start);
        };
        piece.file_start + (offset - piece.snippet_start).min(piece.len)
    }

    fn range_to_file(&self, range: TextRange) -> TextRange {
        let start = self.offset_to_file(range.start);
        let end = if range.end > range.start {
            self.offset_to_file(range.end - 1) + 1
        } else {
            start
        };
        TextRange { start, end }
    }

    /// The snippet offset of a file offset copied into it.
    fn offset_to_snippet(&self, offset: u32) -> Option<u32> {
        self.pieces
            .iter()
            .find(|piece| piece.file_start <= offset && offset <= piece.file_start + piece.len)
            .map(|piece| piece.snippet_start + (offset - piece.file_start))
    }
}

struct SnippetBuilder<'text> {
    file_text: &'text str,
    text: String,
    map: SnippetMap,
}

impl SnippetBuilder<'_> {
    fn position(&self) -> u32 {
        self.text.len() as u32
    }

    fn copy(&mut self, start: usize, end: usize) {
        if end <= start {
            return;
        }
        self.map.pieces.push(Piece {
            snippet_start: self.position(),
            file_start: start as u32,
            len: (end - start) as u32,
        });
        self.text.push_str(&self.file_text[start..end]);
    }

    fn make_up(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// `range` of the file without `left_out` (sorted, inside it): each
    /// left out piece becomes a line break.
    fn copy_leaving_out(&mut self, range: TextRange, left_out: &[TextRange]) {
        let mut cursor = range.start as usize;
        for piece in left_out {
            if (piece.start as usize) < cursor {
                continue;
            }
            self.copy(cursor, piece.start as usize);
            self.make_up("\n");
            cursor = piece.end as usize;
        }
        self.copy(cursor, range.end as usize);
    }
}

fn build_layout(parse: &syntax::Parse, index: &UnitIndex, unit: BodyUnit) -> UnitLayout {
    let root = parse.root();
    let item_at = |range: TextRange| -> Option<ItemId> {
        let first = index
            .items
            .partition_point(|(found, _)| found.start < range.start);
        index.items[first..]
            .iter()
            .take_while(|(found, _)| found.start == range.start)
            .find(|(found, _)| *found == range)
            .map(|(_, item)| *item)
    };
    let mut builder = SnippetBuilder {
        file_text: parse.text(),
        text: String::new(),
        map: SnippetMap::default(),
    };
    let mut items: Vec<(TextRange, ItemId)> = Vec::new();
    let item_range = |item: ItemId| {
        index
            .items
            .iter()
            .find(|(_, found)| *found == item)
            .map(|(range, _)| *range)
    };
    let node = match unit {
        BodyUnit::File => None,
        BodyUnit::Item(item) => item_range(item).and_then(|range| node_of(root, range)),
    };
    let (own_range, ancestors) = match node {
        Some(node) => (extent(node), enclosing_classes(node)),
        None => (TextRange::of(root), Vec::new()),
    };
    // The classes around, down to their body's `{`.
    let mut open: Vec<(u32, SyntaxNode<'_>)> = Vec::new();
    for class in &ancestors {
        let start = builder.position();
        let Some(body) = class_body(*class) else {
            continue;
        };
        builder.copy(class.start_byte(), body.start_byte() + 1);
        if body.kind() == "enum_class_body" {
            builder.make_up("\n;");
        }
        builder.make_up("\n");
        open.push((start, *class));
    }
    // The unit's own text, without the units in it.
    let own_start = builder.position();
    // The outermost units inside it (each takes the ones inside it along).
    let first_inside = index
        .extents
        .partition_point(|(range, _)| range.start < own_range.start);
    let nested: Vec<TextRange> = index.extents[first_inside..]
        .iter()
        .take_while(|(range, _)| range.start <= own_range.end)
        .map(|(range, _)| *range)
        .filter(|range| own_range.contains_range(*range) && *range != own_range)
        .collect();
    let inside: Vec<TextRange> = nested
        .iter()
        .copied()
        .filter(|range| {
            !nested
                .iter()
                .any(|outer| outer != range && outer.contains_range(*range))
        })
        .collect();
    builder.copy_leaving_out(own_range, &inside);
    let own = TextRange {
        start: own_start,
        end: builder.position(),
    };
    for (start, class) in open.into_iter().rev() {
        builder.make_up("\n}");
        if let Some(item) = item_at(TextRange::of(class)) {
            items.push((
                TextRange {
                    start,
                    end: builder.position(),
                },
                item,
            ));
        }
    }
    // The unit's own items: itself (its range spans what's left out), and
    // the ones in its kept text.
    if let (Some(node), BodyUnit::Item(item)) = (node, unit) {
        let length = node.end_byte() - node.start_byte();
        let left_out: usize = inside
            .iter()
            .filter(|range| TextRange::of(node).contains_range(**range))
            .map(|range| range.len())
            .sum();
        let breaks = inside
            .iter()
            .filter(|range| TextRange::of(node).contains_range(**range))
            .count();
        let end = own.start as usize + length - left_out + breaks;
        items.push((
            TextRange {
                start: own.start,
                end: end as u32,
            },
            item,
        ));
    }
    let first_item = index
        .items
        .partition_point(|(range, _)| range.start < own_range.start);
    let in_own_range = index.items[first_item..]
        .iter()
        .take_while(|(range, _)| range.start <= own_range.end);
    for (range, item) in in_own_range {
        let is_unit_itself = matches!(unit, BodyUnit::Item(own_item) if own_item == *item);
        let kept = own_range.contains_range(*range)
            && !is_unit_itself
            && !inside
                .iter()
                .any(|left_out| left_out.contains_range(*range));
        if !kept {
            continue;
        }
        let start = builder.map.offset_to_snippet(range.start);
        let end = builder.map.offset_to_snippet(range.end);
        if let (Some(start), Some(end)) = (start, end) {
            items.push((TextRange { start, end }, *item));
        }
    }
    items.sort_by_key(|(range, item)| (range.start, std::cmp::Reverse(range.end), *item));
    UnitLayout {
        text: Arc::from(builder.text),
        items: items.into(),
        own,
        map: builder.map,
    }
}

/// A unit's text: a property's with the accessors written on their own
/// lines after it (the grammar makes them its siblings).
fn extent(node: SyntaxNode<'_>) -> TextRange {
    let mut range = TextRange::of(node);
    if node.kind() == "property_declaration" {
        let mut next = syntax::next_named_sibling(node);
        while let Some(accessor) = next.filter(|next| matches!(next.kind(), "getter" | "setter")) {
            range.end = accessor.end_byte() as u32;
            next = syntax::next_named_sibling(accessor);
        }
    }
    range
}

/// The classes `node` is declared in, outermost first.
fn enclosing_classes(node: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
    let mut classes = Vec::new();
    let mut current = node.parent();
    while let Some(ancestor) = current {
        if matches!(
            ancestor.kind(),
            "class_declaration" | "object_declaration" | "companion_object"
        ) {
            classes.push(ancestor);
        }
        current = ancestor.parent();
    }
    classes.reverse();
    classes
}

fn class_body(class: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    named_children(class).find(|child| matches!(child.kind(), "class_body" | "enum_class_body"))
}

impl InferenceResult {
    /// Takes in `unit`'s findings at their places in the file; a local
    /// declared in several units (a constructor's parameter, seen from
    /// each initializer's) is one.
    fn absorb(
        &mut self,
        unit: &InferenceResult,
        layout: &UnitLayout,
        locals_by_declaration: &mut FxHashMap<(TextRange, intern::Name), super::LocalId>,
    ) {
        let range = |range: TextRange| layout.to_file(range);
        let offset = |offset: u32| layout.map.offset_to_file(offset);
        let local_ids: Vec<super::LocalId> = unit
            .locals
            .iter()
            .map(|local| {
                let mut placed = local.clone();
                placed.name_range = range(local.name_range);
                let key = (placed.name_range, placed.name);
                *locals_by_declaration.entry(key).or_insert_with(|| {
                    self.locals.push(placed);
                    super::LocalId(self.locals.len() as u32 - 1)
                })
            })
            .collect();
        let local = |id: super::LocalId| local_ids[id.0 as usize];
        for (start, resolved) in &unit.names {
            let mut resolved = resolved.clone();
            if let super::Reference::Local(id) = resolved.reference {
                resolved.reference = super::Reference::Local(local(id));
            }
            self.names.insert(offset(*start), resolved);
        }
        for (start, id) in &unit.local_declarations {
            self.local_declarations.insert(offset(*start), local(*id));
        }
        self.unresolved
            .extend(unit.unresolved.iter().map(|name| super::UnresolvedName {
                range: range(name.range),
                ..name.clone()
            }));
        self.argument_mismatches
            .extend(
                unit.argument_mismatches
                    .iter()
                    .map(|mismatch| super::ArgumentMismatch {
                        range: range(mismatch.range),
                        ..mismatch.clone()
                    }),
            );
        self.unsafe_calls
            .extend(unit.unsafe_calls.iter().map(|call| super::UnsafeCall {
                range: range(call.range),
                ..call.clone()
            }));
        self.non_exhaustive_whens
            .extend(
                unit.non_exhaustive_whens
                    .iter()
                    .map(|when| super::NonExhaustiveWhen {
                        range: range(when.range),
                        ..when.clone()
                    }),
            );
        self.redundant_null_checks
            .extend(unit.redundant_null_checks.iter().map(|check| {
                let kind = match &check.kind {
                    super::NullCheckKind::Elvis { removable } => super::NullCheckKind::Elvis {
                        removable: range(*removable),
                    },
                    other => other.clone(),
                };
                super::RedundantNullCheck {
                    kind,
                    range: range(check.range),
                    ty: check.ty.clone(),
                }
            }));
        self.val_reassignments
            .extend(unit.val_reassignments.iter().map(|found| range(*found)));
        self.missing_returns
            .extend(unit.missing_returns.iter().map(|found| range(*found)));
        for (start, finding) in &unit.type_operations {
            let placed = finding.as_ref().map(|finding| {
                let kind = match &finding.kind {
                    super::TypeOperationKind::UselessCast { removable } => {
                        super::TypeOperationKind::UselessCast {
                            removable: range(*removable),
                        }
                    }
                    other => other.clone(),
                };
                super::TypeOperationFinding {
                    kind,
                    range: range(finding.range),
                }
            });
            self.type_operations.insert(offset(*start), placed);
        }
        self.when_findings
            .extend(unit.when_findings.iter().map(|finding| match finding {
                super::WhenFinding::RedundantElse { keyword, entry } => {
                    super::WhenFinding::RedundantElse {
                        keyword: range(*keyword),
                        entry: range(*entry),
                    }
                }
                super::WhenFinding::DuplicateCondition { range: found } => {
                    super::WhenFinding::DuplicateCondition {
                        range: range(*found),
                    }
                }
            }));
        for (start, mismatch) in &unit.type_mismatches {
            let placed = mismatch.as_ref().map(|mismatch| super::TypeMismatch {
                range: range(mismatch.range),
                ..mismatch.clone()
            });
            self.type_mismatches.insert(offset(*start), placed);
        }
    }
}
