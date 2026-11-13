//! `textDocument/semanticTokens/full`: what each name is, as resolution
//! found it — so an editor colours a property, a local, a parameter and a
//! class differently even where the grammar can't tell.

use hir::{
    ClassKind, FileId, ItemKind, ItemLocation, LocalKind, Modifiers, Name, Path, Reference,
    Resolution, RootDatabase, SourceDatabase,
};
use syntax::{Language, TextRange, named_children};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TokenKind {
    Namespace,
    Class,
    Interface,
    Enum,
    TypeParameter,
    Parameter,
    Variable,
    Property,
    EnumMember,
    Function,
    Method,
}

impl TokenKind {
    /// The legend's order: a token's index there is its type in LSP.
    pub const ALL: [TokenKind; 11] = [
        TokenKind::Namespace,
        TokenKind::Class,
        TokenKind::Interface,
        TokenKind::Enum,
        TokenKind::TypeParameter,
        TokenKind::Parameter,
        TokenKind::Variable,
        TokenKind::Property,
        TokenKind::EnumMember,
        TokenKind::Function,
        TokenKind::Method,
    ];
}

/// Token modifiers, as bits in the legend's order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct TokenModifiers(pub u32);

impl TokenModifiers {
    pub const DECLARATION: TokenModifiers = TokenModifiers(1 << 0);
    pub const READONLY: TokenModifiers = TokenModifiers(1 << 1);
    pub const STATIC: TokenModifiers = TokenModifiers(1 << 2);
    pub const DEPRECATED: TokenModifiers = TokenModifiers(1 << 3);
    /// A `var`: reassignable (not a standard LSP modifier; editors may style
    /// it, as IntelliJ underlines them).
    pub const MUTABLE: TokenModifiers = TokenModifiers(1 << 4);
    /// The legend's names, by bit.
    pub const NAMES: [&'static str; 5] =
        ["declaration", "readonly", "static", "deprecated", "mutable"];

    fn with(self, other: TokenModifiers) -> TokenModifiers {
        TokenModifiers(self.0 | other.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SemanticToken {
    pub range: TextRange,
    pub kind: TokenKind,
    pub modifiers: TokenModifiers,
}

pub(crate) fn semantic_tokens(db: &RootDatabase, file: FileId) -> Vec<SemanticToken> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    if source.language(db) != Language::Kotlin {
        return Vec::new();
    }
    let mut tokens: Vec<SemanticToken> = Vec::new();
    let tree = hir::file_item_tree(db, source);
    for (index, item) in tree.indexed_items() {
        let written =
            &source.text(db)[item.name_range.start as usize..item.name_range.end as usize];
        // An unnamed companion's name (`Companion`) isn't written anywhere.
        let names_itself = written.trim_matches('`') == item.name.as_str();
        if item.kind == ItemKind::Constructor || !names_itself {
            continue;
        }
        let location = ItemLocation::in_file(source, index);
        if let Some((kind, modifiers)) = declaration_token(db, location) {
            tokens.push(SemanticToken {
                range: item.name_range,
                kind,
                modifiers: modifiers.with(TokenModifiers::DECLARATION),
            });
        }
    }
    let inference = hir::infer_file(db, source);
    for (_, local) in inference
        .locals()
        .filter(|(_, local)| local.kind.is_written())
    {
        tokens.push(SemanticToken {
            range: local.name_range,
            kind: local_kind(local.kind),
            modifiers: local_modifiers(local.kind).with(TokenModifiers::DECLARATION),
        });
    }
    let text = source.text(db);
    for (start, resolved) in inference.names() {
        let length = text[start as usize..]
            .chars()
            .take_while(|char| char.is_alphanumeric() || *char == '_' || *char == '`')
            .map(char::len_utf8)
            .sum::<usize>();
        let range = TextRange::new(start as usize, start as usize + length);
        let token = match &resolved.reference {
            Reference::Local(local) => {
                let local = inference.local(*local);
                Some((local_kind(local.kind), local_modifiers(local.kind)))
            }
            Reference::Declaration(location) => declaration_token(db, *location),
            Reference::Candidates(candidates) => {
                let tokens: Vec<_> = candidates
                    .iter()
                    .filter_map(|candidate| declaration_token(db, *candidate))
                    .collect();
                tokens
                    .first()
                    .copied()
                    .filter(|first| tokens.iter().all(|token| token.0 == first.0))
            }
        };
        if let Some((kind, modifiers)) = token {
            tokens.push(SemanticToken {
                range,
                kind,
                modifiers,
            });
        }
    }
    tokens.extend(type_tokens(db, source));
    tokens.sort_by_key(|token| (token.range.start, token.range.end));
    tokens.dedup_by_key(|token| token.range.start);
    tokens
}

/// Names in types (`List<Money>`), as resolution says.
fn type_tokens(db: &RootDatabase, source: hir::SourceFile) -> Vec<SemanticToken> {
    let parse = hir::parse(db, source);
    let mut tokens = Vec::new();
    for user_type in syntax::descendants(parse.root()).filter(|node| node.kind() == "user_type") {
        let mut path: Vec<Name> = Vec::new();
        for segment in named_children(user_type).filter(|child| child.kind() == "type_identifier") {
            path.push(Name::new(parse.text_of(segment)));
            let resolution =
                hir::resolve_type_path(db, source, &Path::new(path.clone()), segment.start_byte());
            let token = match resolution {
                Resolution::Declaration(location) => declaration_token(db, location),
                Resolution::TypeParameter { .. } => {
                    Some((TokenKind::TypeParameter, TokenModifiers::default()))
                }
                Resolution::Package(_) => Some((TokenKind::Namespace, TokenModifiers::default())),
                Resolution::Ambiguous(_) | Resolution::Unresolved => None,
            };
            if let Some((kind, modifiers)) = token {
                tokens.push(SemanticToken {
                    range: TextRange::of(segment),
                    kind,
                    modifiers,
                });
            }
        }
    }
    tokens
}

fn declaration_token(
    db: &RootDatabase,
    location: ItemLocation,
) -> Option<(TokenKind, TokenModifiers)> {
    let tree = hir::item_tree(db, location.container);
    let item = tree.item(location.item);
    let is_member = item.parent.is_some();
    let kind = match item.kind {
        ItemKind::Class(ClassKind::Interface | ClassKind::Annotation) => TokenKind::Interface,
        ItemKind::Class(ClassKind::Enum) => TokenKind::Enum,
        ItemKind::Class(_) | ItemKind::TypeAlias | ItemKind::Constructor => TokenKind::Class,
        ItemKind::Function if is_member => TokenKind::Method,
        ItemKind::Function => TokenKind::Function,
        ItemKind::Property { .. } => TokenKind::Property,
        ItemKind::EnumEntry => TokenKind::EnumMember,
    };
    let mut modifiers = TokenModifiers::default();
    if matches!(
        item.kind,
        ItemKind::Property { mutable: false } | ItemKind::EnumEntry
    ) {
        modifiers = modifiers.with(TokenModifiers::READONLY);
    }
    if matches!(item.kind, ItemKind::Property { mutable: true }) {
        modifiers = modifiers.with(TokenModifiers::MUTABLE);
    }
    // Kotlin's statics: top-level properties, members of objects and
    // companions (and Java's `static`).
    let in_object = item.parent.is_some_and(|parent| {
        matches!(
            tree.item(parent).kind,
            ItemKind::Class(ClassKind::Object | ClassKind::CompanionObject)
        )
    });
    let top_level_property =
        item.parent.is_none() && matches!(item.kind, ItemKind::Property { .. });
    let static_member = !item.kind.is_classifier() && in_object;
    if item.modifiers.contains(Modifiers::STATIC) || top_level_property || static_member {
        modifiers = modifiers.with(TokenModifiers::STATIC);
    }
    if item.modifiers.contains(Modifiers::DEPRECATED) {
        modifiers = modifiers.with(TokenModifiers::DEPRECATED);
    }
    Some((kind, modifiers))
}

fn local_kind(kind: LocalKind) -> TokenKind {
    match kind {
        LocalKind::Parameter | LocalKind::LambdaParameter | LocalKind::ImplicitIt => {
            TokenKind::Parameter
        }
        LocalKind::Function => TokenKind::Function,
        LocalKind::Value | LocalKind::Variable | LocalKind::BackingField => TokenKind::Variable,
    }
}

fn local_modifiers(kind: LocalKind) -> TokenModifiers {
    match kind {
        LocalKind::Variable | LocalKind::BackingField => TokenModifiers::MUTABLE,
        _ => TokenModifiers::READONLY,
    }
}
