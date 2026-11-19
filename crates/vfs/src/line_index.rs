//! Byte offsets ↔ (line, column), the columns counted as LSP counts them:
//! UTF-16 code units by default, or bytes where client and server agree on
//! UTF-8.

/// What a column counts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColumnUnit {
    #[default]
    Utf16,
    /// Bytes of UTF-8: what the text is stored in, nothing to convert.
    Utf8,
}

/// A line and a column (in the index's unit), both zero-based.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct LineColumn {
    pub line: u32,
    pub column: u32,
}

/// Where each line of a text starts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LineIndex {
    line_starts: Vec<usize>,
    text_length: usize,
    unit: ColumnUnit,
}

impl LineIndex {
    /// Columns in UTF-16 code units.
    pub fn new(text: &str) -> Self {
        Self::with_unit(text, ColumnUnit::Utf16)
    }

    pub fn with_unit(text: &str, unit: ColumnUnit) -> Self {
        let mut line_starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self {
            line_starts,
            text_length: text.len(),
            unit,
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The position of byte `offset` in `text` (the text this index was
    /// built from). An offset inside a multi-byte character counts the
    /// character as started.
    pub fn line_column(&self, text: &str, offset: usize) -> LineColumn {
        let offset = offset.min(self.text_length);
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let line_start = self.line_starts[line];
        let column = match self.unit {
            ColumnUnit::Utf8 => offset - line_start,
            ColumnUnit::Utf16 => text
                .get(line_start..offset)
                .map(|prefix| prefix.encode_utf16().count())
                .unwrap_or_else(|| utf16_length_until(&text[line_start..], offset - line_start)),
        };
        LineColumn {
            line: line as u32,
            column: column as u32,
        }
    }

    /// The byte offset of `position` in `text`; a column past the end of its
    /// line lands at the line's end, a line past the end at the text's end.
    pub fn offset(&self, text: &str, position: LineColumn) -> usize {
        let Some(line_start) = self.line_starts.get(position.line as usize).copied() else {
            return self.text_length;
        };
        let line_end = self
            .line_starts
            .get(position.line as usize + 1)
            .map(|next| next - 1)
            .unwrap_or(self.text_length);
        let mut units = 0u32;
        for (relative, character) in text[line_start..line_end].char_indices() {
            if units >= position.column {
                return line_start + relative;
            }
            units += match self.unit {
                ColumnUnit::Utf16 => character.len_utf16() as u32,
                ColumnUnit::Utf8 => character.len_utf8() as u32,
            };
        }
        line_end
    }
}

/// UTF-16 units of the characters of `line` that start before `bytes`.
fn utf16_length_until(line: &str, bytes: usize) -> usize {
    line.char_indices()
        .take_while(|(offset, _)| *offset < bytes)
        .map(|(_, character)| character.len_utf16())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_count_utf16_units_and_round_trip() {
        // `ё` is 2 bytes / 1 unit; `𝄞` is 4 bytes / 2 units.
        let text = "val a = 1\nval ё𝄞 = \"x\"\n";
        let index = LineIndex::new(text);
        let equals = text.find(" = \"").unwrap();
        let position = index.line_column(text, equals);
        assert_eq!(position, LineColumn { line: 1, column: 7 });
        assert_eq!(index.offset(text, position), equals);
        assert_eq!(index.line_count(), 3);
    }

    #[test]
    fn utf8_columns_count_bytes_and_round_trip() {
        let text = "val a = 1\nval ё𝄞 = \"x\"\n";
        let index = LineIndex::with_unit(text, ColumnUnit::Utf8);
        let equals = text.find(" = \"").unwrap();
        let position = index.line_column(text, equals);
        // `val ` 4 bytes, `ё` 2, `𝄞` 4.
        assert_eq!(
            position,
            LineColumn {
                line: 1,
                column: 10
            }
        );
        assert_eq!(index.offset(text, position), equals);
    }

    #[test]
    fn positions_past_the_end_clamp() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        assert_eq!(
            index.offset(
                text,
                LineColumn {
                    line: 0,
                    column: 99
                }
            ),
            2
        );
        assert_eq!(index.offset(text, LineColumn { line: 9, column: 0 }), 5);
        assert_eq!(
            index.line_column(text, 99),
            LineColumn { line: 1, column: 2 }
        );
    }
}
