//! The member-level declaration around a position — of the file or of a
//! class: a function, a property (with accessors written on their own
//! lines), an initializer, a secondary constructor. Completion walks only
//! it; data-flow checks look for tests of a value anywhere in it.

use syntax::SyntaxNode;

/// The member-level code a node is in.
#[derive(Clone, Copy)]
pub(super) enum MemberAround<'tree> {
    /// A function, a property, an initializer, a secondary constructor.
    Declaration(SyntaxNode<'tree>),
    /// An accessor on its own line, which the grammar makes the property's
    /// sibling: the code is in `accessor`, the declaration is `property`.
    Accessor {
        accessor: SyntaxNode<'tree>,
        property: SyntaxNode<'tree>,
    },
}

impl<'tree> MemberAround<'tree> {
    /// The declaration to walk to infer the code.
    pub(super) fn declaration(self) -> SyntaxNode<'tree> {
        match self {
            MemberAround::Declaration(declaration) => declaration,
            MemberAround::Accessor { property, .. } => property,
        }
    }

    /// The node whose text holds the code.
    fn code(self) -> SyntaxNode<'tree> {
        match self {
            MemberAround::Declaration(declaration) => declaration,
            MemberAround::Accessor { accessor, .. } => accessor,
        }
    }
}

/// The member-level code `node` is in; `None` outside any (a class
/// header, a script's statements).
pub(super) fn enclosing_member(node: SyntaxNode<'_>) -> Option<MemberAround<'_>> {
    let mut current = node;
    loop {
        let parent = current.parent()?;
        let at_member_level = matches!(
            parent.kind(),
            "class_body" | "enum_class_body" | "source_file"
        );
        if at_member_level {
            return match current.kind() {
                "function_declaration"
                | "property_declaration"
                | "secondary_constructor"
                | "anonymous_initializer" => Some(MemberAround::Declaration(current)),
                "getter" | "setter" => Some(MemberAround::Accessor {
                    accessor: current,
                    property: accessed_property(current)?,
                }),
                _ => None,
            };
        }
        current = parent;
    }
}

/// The member-level code around `node`, else the whole file: where a
/// data-flow check looks for what could have narrowed a value.
pub(super) fn data_flow_scope(node: SyntaxNode<'_>) -> SyntaxNode<'_> {
    if let Some(member) = enclosing_member(node) {
        return member.code();
    }
    let mut root = node;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    root
}

/// The property an accessor written on its own line belongs to: the
/// nearest property declared before it.
fn accessed_property(accessor: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut previous = syntax::previous_named_sibling(accessor);
    while let Some(sibling) = previous {
        match sibling.kind() {
            "property_declaration" => return Some(sibling),
            "getter" | "setter" => previous = syntax::previous_named_sibling(sibling),
            _ => return None,
        }
    }
    None
}
