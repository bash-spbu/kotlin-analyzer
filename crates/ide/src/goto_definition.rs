//! What the name under the cursor refers to: names in bodies (as inference
//! resolved them), type names (written in signatures, supertypes, type
//! arguments — anywhere a `user_type` is), the segments of import paths,
//! and declaration names themselves. Shared by `definition` and `hover`.

use hir::{
    FileId, ItemLocation, Local, LocalKind, Path, Reference, RootDatabase, SourceDatabase,
    SourceFile, Ty,
};
use syntax::ast::AstNode;
use syntax::ast::kotlin::{Identifier, UserType};
use syntax::{Language, SyntaxNode, TextRange};

use crate::NavigationTarget;

/// Where a declaration is declared, for `textDocument/declaration`.
enum Declared {
    /// An `actual`'s `expect`s.
    Expected(Vec<ItemLocation>),
    /// What a member that overrides others overrides directly.
    Overridden(Vec<ItemLocation>),
    /// Anything else: as its definition.
    Itself,
}

impl Declared {
    fn of(db: &RootDatabase, declaration: ItemLocation) -> Declared {
        let expected: Vec<ItemLocation> = hir::expect_actual_family(db, declaration)
            .into_iter()
            .filter(|member| {
                hir::item_tree(db, member.container)
                    .item(member.item)
                    .modifiers
                    .contains(hir::Modifiers::EXPECT)
            })
            .collect();
        if !expected.is_empty() {
            return Declared::Expected(expected);
        }
        let overridden = hir::overridden_directly(db, declaration);
        if !overridden.is_empty() {
            return Declared::Overridden(overridden);
        }
        Declared::Itself
    }
}

/// `textDocument/declaration` for a member's name or use, only what it
/// overrides directly (the ↑ code lens); nothing for anything else.
pub(crate) fn goto_super(db: &RootDatabase, file: FileId, offset: usize) -> Vec<NavigationTarget> {
    let Some(name) = name_at(db, file, offset) else {
        return Vec::new();
    };
    let NameTarget::Declarations(declarations) = name.target else {
        return Vec::new();
    };
    let mut overridden: Vec<ItemLocation> = Vec::new();
    for declaration in declarations {
        for member in hir::overridden_directly(db, declaration) {
            if !overridden.contains(&member) {
                overridden.push(member);
            }
        }
    }
    overridden
        .into_iter()
        .filter_map(|location| NavigationTarget::of(db, location))
        .collect()
}

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

/// `textDocument/declaration`: where the name at `offset` is declared for
/// every platform — an `actual` declaration's `expect` — or, for a member
/// that overrides others, what it overrides directly; anything else, as
/// its definition.
pub(crate) fn goto_declaration(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Vec<NavigationTarget> {
    let Some(name) = name_at(db, file, offset) else {
        return Vec::new();
    };
    let declarations = match name.target {
        NameTarget::Declarations(declarations) => declarations,
        NameTarget::Local { file, local } => {
            return vec![NavigationTarget::of_local(db, file, &local)];
        }
    };
    let mut targets: Vec<hir::ItemLocation> = Vec::new();
    for declaration in declarations {
        let found = match Declared::of(db, declaration) {
            Declared::Expected(expected) => expected,
            Declared::Overridden(overridden) => overridden,
            Declared::Itself => vec![declaration],
        };
        for location in found {
            if !targets.contains(&location) {
                targets.push(location);
            }
        }
    }
    targets
        .into_iter()
        .filter_map(|location| NavigationTarget::of(db, location))
        .collect()
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
    let local_target = |local| {
        let local = inference.local(local).clone();
        // A `val`/`var` constructor parameter is the class's property: the
        // item declared at the same name.
        if local.kind == LocalKind::PropertyParameter {
            let property = declaration_named_at(db, file, local.name_range);
            if !property.is_empty() {
                return NameTarget::Declarations(property);
            }
        }
        NameTarget::Local { file, local }
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
    // The character after the cursor, then the one before it: an empty
    // range at a node's boundary can find the enclosing node instead (an
    // annotation's arguments, for a name right after its `(`).
    let after = root.named_descendant_for_byte_range(offset, offset + 1);
    if let Some(node) = after.filter(is_identifier) {
        return Some(node);
    }
    let before = offset.checked_sub(1)?;
    root.named_descendant_for_byte_range(before, offset)
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
        // `Type::class` and `Type::member` in an annotation: the grammar
        // leaves the type a bare name, not a `user_type`.
        "callable_reference" if identifier.kind() == "type_identifier" => {
            let path = Path::new([hir::Name::new(parse.text_of(identifier))]);
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
    let source_map = hir::file_item_source_map(db, file);
    tree.indexed_items()
        .filter(|(index, _)| source_map.ranges(*index).name_range == range)
        .map(|(index, _)| ItemLocation::in_file(file, index))
        .collect()
}
