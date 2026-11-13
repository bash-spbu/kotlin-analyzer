//! Typed views of syntax nodes: a wrapper per node kind, with accessors for
//! the parts of it, so code above reads `class.name()` instead of searching
//! children by kind strings. Text is read through [`crate::Parse::text_of`].

pub mod kotlin;

use crate::{SyntaxNode, TextRange};

/// A typed wrapper of one node kind.
pub trait AstNode<'tree>: Sized + Copy {
    /// The tree-sitter kind this wraps.
    const KIND: &'static str;

    fn new_unchecked(node: SyntaxNode<'tree>) -> Self;

    fn syntax(self) -> SyntaxNode<'tree>;

    fn cast(node: SyntaxNode<'tree>) -> Option<Self> {
        (node.kind() == Self::KIND).then(|| Self::new_unchecked(node))
    }

    fn range(self) -> TextRange {
        TextRange::of(self.syntax())
    }
}

/// The first named child of `node` that is a `T`.
pub fn child<'tree, T: AstNode<'tree>>(node: SyntaxNode<'tree>) -> Option<T> {
    crate::named_children(node).find_map(T::cast)
}

/// Every named child of `node` that is a `T`.
pub fn children<'tree, T: AstNode<'tree> + 'tree>(
    node: SyntaxNode<'tree>,
) -> impl Iterator<Item = T> + 'tree {
    crate::named_children(node).filter_map(T::cast)
}

macro_rules! ast_nodes {
    ($($(#[$meta:meta])* $name:ident => $kind:literal),* $(,)?) => {
        $(
            $(#[$meta])*
            #[derive(Clone, Copy, Debug)]
            pub struct $name<'tree>(crate::SyntaxNode<'tree>);

            impl<'tree> crate::ast::AstNode<'tree> for $name<'tree> {
                const KIND: &'static str = $kind;

                fn new_unchecked(node: crate::SyntaxNode<'tree>) -> Self {
                    Self(node)
                }

                fn syntax(self) -> crate::SyntaxNode<'tree> {
                    self.0
                }
            }
        )*
    };
}
pub(crate) use ast_nodes;
