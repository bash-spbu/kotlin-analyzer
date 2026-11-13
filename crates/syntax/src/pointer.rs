//! A node remembered without borrowing its tree.

use crate::{SyntaxNode, TextRange};

/// A syntax node by kind and range: storable, and found again in any parse
/// of the same text. Several nodes can span one range (a `user_type` around
/// its only `type_identifier`), so the kind tells them apart.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SyntaxNodePtr {
    kind: u16,
    range: TextRange,
}

impl SyntaxNodePtr {
    pub fn new(node: SyntaxNode<'_>) -> Self {
        Self {
            kind: node.kind_id(),
            range: TextRange::of(node),
        }
    }

    pub fn range(self) -> TextRange {
        self.range
    }

    /// The node under `root` this points at, if the tree still has it.
    pub fn to_node(self, root: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
        let mut node =
            root.descendant_for_byte_range(self.range.start as usize, self.range.end as usize)?;
        loop {
            let same_range = TextRange::of(node) == self.range;
            if same_range && node.kind_id() == self.kind {
                return Some(node);
            }
            if !same_range {
                return None;
            }
            node = node.parent()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{Language, descendants, parse};

    use super::*;

    #[test]
    fn a_pointer_finds_its_node_and_not_a_same_range_neighbour() {
        let parse = parse(Arc::from("val x: Foo = Foo()\n"), Language::Kotlin);
        for node in descendants(parse.root()) {
            let pointer = SyntaxNodePtr::new(node);
            let found = pointer.to_node(parse.root()).expect("found again");
            assert_eq!(found.kind_id(), node.kind_id());
            assert_eq!(found.byte_range(), node.byte_range());
        }
    }
}
