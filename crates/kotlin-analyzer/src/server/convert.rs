//! LSP ↔ internal: URIs ↔ paths, positions ↔ byte offsets, ranges.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use lsp_types::{Position, Range, Uri};
use syntax::TextRange;
use vfs::{LineColumn, LineIndex};

/// The path of a `file:` URI.
pub fn path_of(uri: &Uri) -> Option<PathBuf> {
    let text = uri.as_str();
    let encoded = text.strip_prefix("file://")?;
    Some(PathBuf::from(percent_decode(encoded)))
}

/// The `file:` URI of an absolute path.
pub fn uri_of(path: &Path) -> Option<Uri> {
    let mut encoded = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        let unreserved = byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte);
        if unreserved {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    Uri::from_str(&encoded).ok()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = bytes[index] == b'%' && index + 2 < bytes.len();
        if escape {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                decoded.push(value);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

pub fn offset(text: &str, position: Position) -> usize {
    LineIndex::new(text).offset(
        text,
        LineColumn {
            line: position.line,
            column: position.character,
        },
    )
}

/// `text` after `changes`, in order: each replaces its range (positions in
/// the text as the changes before it left it), or the whole text when it
/// has none.
pub fn apply_changes(
    text: &str,
    changes: Vec<lsp_types::TextDocumentContentChangeEvent>,
) -> String {
    let mut text = text.to_owned();
    for change in changes {
        match change.range {
            Some(range) => {
                let index = LineIndex::new(&text);
                let start = index.offset(&text, line_column(range.start));
                let end = index.offset(&text, line_column(range.end)).max(start);
                text.replace_range(start..end, &change.text);
            }
            None => text = change.text,
        }
    }
    text
}

fn line_column(position: Position) -> LineColumn {
    LineColumn {
        line: position.line,
        column: position.character,
    }
}

pub fn position(index: &LineIndex, text: &str, offset: usize) -> Position {
    let line_column = index.line_column(text, offset);
    Position::new(line_column.line, line_column.column)
}

pub fn range(index: &LineIndex, text: &str, range: TextRange) -> Range {
    Range::new(
        position(index, text, range.start as usize),
        position(index, text, range.end as usize),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(
        start: (u32, u32),
        end: (u32, u32),
        text: &str,
    ) -> lsp_types::TextDocumentContentChangeEvent {
        lsp_types::TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn changes_apply_in_order_counting_columns_in_utf16() {
        let text = "val a = 1\nval 😀b = \"x\"\n";
        let changed = apply_changes(
            text,
            vec![
                // After the emoji (two UTF-16 units): `b` → `bee`.
                change((1, 6), (1, 7), "bee"),
                // On the text as the first change left it.
                change((1, 13), (1, 14), "yy"),
                change((0, 4), (0, 5), "alpha"),
                // Across lines: the line break and `val`.
                change((0, 13), (1, 3), ";"),
            ],
        );
        assert_eq!(changed, "val alpha = 1; 😀bee = \"yy\"\n");
    }

    #[test]
    fn a_change_without_a_range_replaces_the_text() {
        let whole = lsp_types::TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: "class B".to_owned(),
        };
        assert_eq!(apply_changes("class A", vec![whole]), "class B");
    }

    #[test]
    fn paths_and_uris_round_trip_with_spaces_and_cyrillic() {
        let path = Path::new("/Users/me/Мои проекты/A B.kt");
        let uri = uri_of(path).unwrap();
        assert!(uri.as_str().starts_with("file:///Users/me/"));
        assert_eq!(path_of(&uri).unwrap(), path);
    }
}
