//! `textDocument/definition` for what M1 resolves: type names (written in
//! signatures, supertypes, type arguments — anywhere a `user_type` is), the
//! segments of import paths, and declaration names themselves.

use hir::{FileId, ItemLocation, Path, RootDatabase, SourceDatabase, SourceFile};
use syntax::ast::AstNode;
use syntax::ast::kotlin::{Identifier, UserType};
use syntax::{Language, SyntaxNode, TextRange};

use crate::NavigationTarget;

pub(crate) fn goto_definition(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Vec<NavigationTarget> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    let parse = hir::parse(db, source);
    let Some(identifier) = identifier_at(parse.root(), offset) else {
        return Vec::new();
    };
    let targets = match source.language(db) {
        Language::Kotlin => kotlin_targets(db, source, parse, identifier),
        Language::Java => Vec::new(),
    };
    if !targets.is_empty() {
        return targets;
    }
    declaration_named_at(db, source, TextRange::of(identifier))
}

/// The identifier under the cursor — or just before it, when the cursor
/// sits right after a name.
fn identifier_at(root: SyntaxNode<'_>, offset: usize) -> Option<SyntaxNode<'_>> {
    let is_identifier = |node: &SyntaxNode<'_>| {
        matches!(
            node.kind(),
            "simple_identifier" | "type_identifier" | "identifier"
        )
    };
    let at = root.named_descendant_for_byte_range(offset, offset);
    if let Some(node) = at.filter(is_identifier) {
        return Some(node);
    }
    let before = offset.checked_sub(1)?;
    root.named_descendant_for_byte_range(before, before)
        .filter(is_identifier)
}

fn kotlin_targets(
    db: &RootDatabase,
    file: SourceFile,
    parse: &syntax::Parse,
    identifier: SyntaxNode<'_>,
) -> Vec<NavigationTarget> {
    let Some(parent) = identifier.parent() else {
        return Vec::new();
    };
    let resolution = match parent.kind() {
        "user_type" => {
            let user_type = UserType::new_unchecked(parent);
            let segments = user_type.segments();
            let Some(position) = segments
                .iter()
                .position(|(segment, _)| segment.id() == identifier.id())
            else {
                return Vec::new();
            };
            let path = Path::new(
                segments[..=position]
                    .iter()
                    .map(|(segment, _)| hir::Name::new(parse.text_of(*segment))),
            );
            hir::resolve_type_path(db, file, &path, identifier.start_byte())
        }
        "identifier" if is_import_path(parent) => {
            let path_node = Identifier::new_unchecked(parent);
            let segments: Vec<SyntaxNode<'_>> = path_node.segments().collect();
            let Some(position) = segments
                .iter()
                .position(|segment| segment.id() == identifier.id())
            else {
                return Vec::new();
            };
            let path = Path::new(
                segments[..=position]
                    .iter()
                    .map(|segment| hir::Name::new(parse.text_of(*segment))),
            );
            hir::resolve_import_path(db, file, &path)
        }
        _ => return Vec::new(),
    };
    resolution
        .declarations()
        .into_iter()
        .map(|location| NavigationTarget::of(db, location))
        .collect()
}

fn is_import_path(identifier: SyntaxNode<'_>) -> bool {
    identifier
        .parent()
        .is_some_and(|parent| parent.kind() == "import_header")
}

/// When the cursor is on a declaration's own name, that declaration.
fn declaration_named_at(
    db: &RootDatabase,
    file: SourceFile,
    range: TextRange,
) -> Vec<NavigationTarget> {
    let tree = hir::file_item_tree(db, file);
    tree.indexed_items()
        .filter(|(_, item)| item.name_range == range)
        .map(|(index, _)| NavigationTarget::of(db, ItemLocation { file, item: index }))
        .collect()
}
