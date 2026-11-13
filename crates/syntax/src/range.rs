//! Byte ranges in a file's text.

use crate::SyntaxNode;

/// A byte range `start..end` of a file's text. `u32` keeps stored ranges
/// small; files over 4 GiB aren't source code.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

impl TextRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    pub fn of(node: SyntaxNode<'_>) -> Self {
        Self::new(node.start_byte(), node.end_byte())
    }

    pub fn contains(self, offset: usize) -> bool {
        let offset = offset as u32;
        self.start <= offset && offset <= self.end
    }

    pub fn contains_range(self, other: TextRange) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    pub fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn as_range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}
