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

    #[test]
    fn paths_and_uris_round_trip_with_spaces_and_cyrillic() {
        let path = Path::new("/Users/me/Мои проекты/A B.kt");
        let uri = uri_of(path).unwrap();
        assert!(uri.as_str().starts_with("file:///Users/me/"));
        assert_eq!(path_of(&uri).unwrap(), path);
    }
}
