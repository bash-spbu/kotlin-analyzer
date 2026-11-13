//! What the name under the cursor refers to: names in bodies (as inference
//! resolved them), type names (written in signatures, supertypes, type
//! arguments — anywhere a `user_type` is), the segments of import paths,
//! and declaration names themselves. Shared by `definition` and `hover`.

use hir::{
    FileId, ItemLocation, Local, Path, Reference, RootDatabase, SourceDatabase, SourceFile, Ty,
};
use syntax::ast::AstNode;
use syntax::ast::kotlin::{Identifier, UserType};
use syntax::{Language, SyntaxNode, TextRange};

use crate::NavigationTarget;

/// The name under the cursor and what it refers to.
pub(crate) struct NameAt {
    pub(crate) range: TextRange,
    pub(crate) target: NameTarget,
    /// The value's type there, when the name is in a body.
    pub(crate) ty: Option<Ty>,
}

pub(crate) enum NameTarget {
    /// Declarations: one, or the candidates of an unresolved overload.
    Declarations(Vec<ItemLocation>),
    /// A local of the file.
    Local { file: SourceFile, local: Local },
}

pub(crate) fn goto_definition(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Vec<NavigationTarget> {
    let Some(name) = name_at(db, file, offset) else {
        return Vec::new();
    };
    match name.target {
        NameTarget::Declarations(declarations) => declarations
            .into_iter()
            .filter_map(|location| NavigationTarget::of(db, location))
            .collect(),
        NameTarget::Local { file, local } => vec![NavigationTarget::of_local(db, file, &local)],
    }
}

pub(crate) fn name_at(db: &RootDatabase, file: FileId, offset: usize) -> Option<NameAt> {
    let source = db.source_file(file)?;
    let parse = hir::parse(db, source);
    let identifier = identifier_at(parse.root(), offset)?;
    let range = TextRange::of(identifier);
    if source.language(db) == Language::Kotlin
        && let Some(in_body) = body_name(db, source, range)
    {
        return Some(in_body);
    }
    let mut declarations = match source.language(db) {
        Language::Kotlin => kotlin_declarations(db, source, &parse, identifier),
        Language::Java => Vec::new(),
    };
    if declarations.is_empty() {
        declarations = declaration_named_at(db, source, range);
    }
    Some(NameAt {
        range,
        target: NameTarget::Declarations(declarations),
        ty: None,
    })
}

/// A name inference resolved — a reference in a body, or a local's
/// declaration.
fn body_name(db: &RootDatabase, file: SourceFile, range: TextRange) -> Option<NameAt> {
    let inference = hir::infer_file(db, file);
    let local_target = |local| NameTarget::Local {
        file,
        local: inference.local(local).clone(),
    };
    if let Some(resolved) = inference.name_at(range.start) {
        let target = match &resolved.reference {
            Reference::Local(local) => local_target(*local),
            Reference::Declaration(location) => NameTarget::Declarations(vec![*location]),
            Reference::Candidates(candidates) => NameTarget::Declarations(candidates.to_vec()),
        };
        return Some(NameAt {
            range,
            target,
            ty: Some(resolved.ty.clone()),
        });
    }
    let local = inference.local_declared_at(range.start)?;
    Some(NameAt {
        range,
        target: local_target(local),
        ty: None,
    })
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

fn kotlin_declarations(
    db: &RootDatabase,
    file: SourceFile,
    parse: &syntax::Parse,
    identifier: SyntaxNode<'_>,
) -> Vec<ItemLocation> {
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
    resolution.declarations()
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
) -> Vec<ItemLocation> {
    let tree = hir::file_item_tree(db, file);
    tree.indexed_items()
        .filter(|(_, item)| item.name_range == range)
        .map(|(index, _)| ItemLocation::in_file(file, index))
        .collect()
}
