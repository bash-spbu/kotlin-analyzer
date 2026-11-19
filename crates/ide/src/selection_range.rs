//! `textDocument/selectionRange`: what "expand selection" grows through
//! from a position — the word (inside a string or a comment too), then
//! each syntax node around it, with what lies between a node's brackets or
//! quotes before the node itself.

use hir::{FileId, RootDatabase, SourceDatabase};
use syntax::{SyntaxNode, TextRange};

/// For each offset, the ranges around it from the innermost out, each
/// containing the one before.
pub(crate) fn selection_ranges(
    db: &RootDatabase,
    file: FileId,
    offsets: &[usize],
) -> Vec<Vec<TextRange>> {
    let Some(source) = db.source_file(file) else {
        return offsets.iter().map(|_| Vec::new()).collect();
    };
    let parse = hir::parse(db, source);
    offsets
        .iter()
        .map(|offset| ranges_around(parse.root(), parse.text(), *offset))
        .collect()
}

fn ranges_around(root: SyntaxNode<'_>, text: &str, offset: usize) -> Vec<TextRange> {
    let offset = offset.min(text.len());
    let mut ranges: Vec<TextRange> = Vec::new();
    let token = token_at(root, text, offset);
    if let Some(word) =
        word_around(text, offset).filter(|word| TextRange::of(token).contains_range(*word))
    {
        ranges.push(word);
    }
    let mut current = Some(token);
    while let Some(node) = current {
        if let Some(inner) = between_delimiters(node, text) {
            push_growing(&mut ranges, inner);
        }
        push_growing(&mut ranges, TextRange::of(node));
        current = node.parent();
    }
    ranges
}

/// `range`, unless it is no bigger than the last one or doesn't hold it.
fn push_growing(ranges: &mut Vec<TextRange>, range: TextRange) {
    let grows = ranges
        .last()
        .is_none_or(|last| range.contains_range(*last) && range != *last);
    if grows {
        ranges.push(range);
    }
}

/// The node to start from: the token at `offset`, or — at the end of a word
/// followed by punctuation (`name|)`) — the word.
fn token_at<'tree>(root: SyntaxNode<'tree>, text: &str, offset: usize) -> SyntaxNode<'tree> {
    let at = root
        .descendant_for_byte_range(offset, offset)
        .unwrap_or(root);
    let starts_here = at.start_byte() == offset;
    let ends_a_word = offset > 0 && text[..offset].ends_with(is_word_char);
    let at_a_word = text[offset..].starts_with(is_word_char);
    if starts_here && ends_a_word && !at_a_word {
        let before = root.descendant_for_byte_range(offset - 1, offset - 1);
        if let Some(before) = before.filter(|before| before.end_byte() == offset) {
            return before;
        }
    }
    at
}

/// The run of word characters touching `offset`, if any.
fn word_around(text: &str, offset: usize) -> Option<TextRange> {
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, char)| is_word_char(*char))
        .last()
        .map_or(offset, |(index, _)| index);
    let end = offset
        + text[offset..]
            .char_indices()
            .find(|(_, char)| !is_word_char(*char))
            .map_or(text.len() - offset, |(index, _)| index);
    (start < end).then(|| TextRange::new(start, end))
}

fn is_word_char(char: char) -> bool {
    char.is_alphanumeric() || char == '_'
}

/// What lies between a node's opening and closing delimiter, when it
/// starts and ends with a matching pair: `(…)`, `{…}`, `[…]`, `<…>`, a
/// string's quotes. Whitespace next to the delimiters isn't included.
fn between_delimiters(node: SyntaxNode<'_>, text: &str) -> Option<TextRange> {
    if node.child_count() < 2 {
        return None;
    }
    let first = syntax::children(node).next()?;
    let last = syntax::children(node).last()?;
    let pair = (first.kind(), last.kind());
    let delimited = matches!(
        pair,
        ("(", ")") | ("{", "}") | ("[", "]") | ("<", ">") | ("\"", "\"") | ("\"\"\"", "\"\"\"")
    );
    if !delimited {
        return None;
    }
    let inside = &text[first.end_byte()..last.start_byte()];
    let leading = inside.len() - inside.trim_start().len();
    let trailing = inside.len() - inside.trim_end().len();
    let start = first.end_byte() + leading;
    let end = last.start_byte() - trailing;
    (start < end).then(|| TextRange::new(start, end))
}
