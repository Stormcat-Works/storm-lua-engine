//! Coordinate conversion for immutable UTF-8 source snapshots.
//! Lua diagnostics use one-based byte columns; Source Map consumers use
//! zero-based UTF-16 columns. Both share the same LF/CRLF line index.

/// Count UTF-16 code units without decoding the common all-ASCII case.
/// This preserves source/target accounting, including supplementary characters;
/// byte length is not used for non-ASCII input.
pub fn utf16_len(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.encode_utf16().count()
    }
}

/// A borrowed source snapshot and its line starts. No filesystem access.
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
    // Non-ASCII character ends and cumulative UTF-8 minus UTF-16 lengths.
    // This avoids rescanning a long generated line at every mapping token.
    utf8_excess: Vec<(usize, usize)>,
}

impl<'a> LineIndex<'a> {
    /// Index line starts without normalizing the source or its line endings.
    pub fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        let mut utf8_excess = Vec::new();
        let mut excess = 0;
        for (offset, ch) in text.char_indices() {
            if ch == '\n' {
                starts.push(offset + 1);
            }
            if !ch.is_ascii() {
                excess += ch.len_utf8() - ch.len_utf16();
                utf8_excess.push((offset + ch.len_utf8(), excess));
            }
        }
        Self {
            text,
            starts,
            utf8_excess,
        }
    }

    /// Convert a valid UTF-8 character boundary to one-based line/byte-column.
    /// EOF is a valid boundary; offsets outside the text or inside a character are not.
    pub fn byte_position(&self, offset: usize) -> Option<(u32, u32)> {
        let (line, start) = self.line_at(offset)?;
        Some((
            (line + 1).try_into().ok()?,
            (offset - start + 1).try_into().ok()?,
        ))
    }

    /// Convert a valid UTF-8 boundary to zero-based line/UTF-16-column.
    pub fn utf16_position(&self, offset: usize) -> Option<(u32, u32)> {
        let (line, start) = self.line_at(offset)?;
        let excess = self.excess_at(offset) - self.excess_at(start);
        Some((
            line.try_into().ok()?,
            (offset - start - excess).try_into().ok()?,
        ))
    }

    /// Convert a one-based diagnostic line/byte-column to a UTF-8 boundary.
    /// Reject invalid lines, columns crossing a line and partial UTF-8 characters.
    pub fn byte_offset(&self, line: u32, column: u32) -> Option<usize> {
        let line = usize::try_from(line.checked_sub(1)?).ok()?;
        let column = usize::try_from(column.checked_sub(1)?).ok()?;
        let start = *self.starts.get(line)?;
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.text.len(), |next| next - 1);
        let offset = start.checked_add(column)?;
        (offset <= end && self.text.is_char_boundary(offset)).then_some(offset)
    }

    fn excess_at(&self, offset: usize) -> usize {
        let end = self
            .utf8_excess
            .partition_point(|&(position, _)| position <= offset);
        end.checked_sub(1).map_or(0, |i| self.utf8_excess[i].1)
    }

    fn line_at(&self, offset: usize) -> Option<(usize, usize)> {
        if offset > self.text.len() || !self.text.is_char_boundary(offset) {
            return None;
        }
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        Some((line, self.starts[line]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_crlf_and_eof_coordinates_are_not_interchangeable() {
        let text = "a😀あ\r\nb\n";
        let index = LineIndex::new(text);
        assert_eq!(index.byte_position(5), Some((1, 6)));
        assert_eq!(index.utf16_position(5), Some((0, 3)));
        assert_eq!(index.byte_position(10), Some((2, 1)));
        assert_eq!(index.utf16_position(10), Some((1, 0)));
        assert_eq!(index.byte_position(text.len()), Some((3, 1)));
        assert_eq!(index.utf16_position(text.len()), Some((2, 0)));
        assert_eq!(index.byte_offset(1, 6), Some(5));
        for offset in 0..=text.len() {
            if let Some((line, col)) = index.byte_position(offset) {
                assert_eq!(index.byte_offset(line, col), Some(offset));
            } else {
                assert!(!text.is_char_boundary(offset));
            }
        }
    }

    #[test]
    fn invalid_coordinates_are_rejected_instead_of_snapping() {
        let index = LineIndex::new("😀\nx");
        assert_eq!(index.byte_position(1), None);
        assert_eq!(index.utf16_position(2), None);
        assert_eq!(index.byte_position(100), None);
        assert_eq!(index.byte_offset(0, 1), None);
        assert_eq!(index.byte_offset(1, 0), None);
        assert_eq!(index.byte_offset(1, 2), None);
        assert_eq!(index.byte_offset(1, 6), None);
        assert_eq!(index.byte_offset(3, 1), None);
        assert_eq!(LineIndex::new("").byte_position(0), Some((1, 1)));
    }
}

/// Repeated UTF-16 lookups in an immutable source snapshot. Nearby offsets reuse
/// their line and UTF-8/UTF-16 interval; arbitrary jumps still use binary search.
/// Rejected offsets do not alter the cursor or silently snap to a character.
pub struct Utf16Cursor<'index, 'source> {
    index: &'index LineIndex<'source>,
    line: usize,
    excess_end: usize,
    line_excess: usize,
}
impl<'source> LineIndex<'source> {
    /// Create a non-owning cursor for sequential or repeated coordinate lookups.
    pub fn utf16_cursor(&self) -> Utf16Cursor<'_, 'source> {
        Utf16Cursor {
            index: self,
            line: 0,
            excess_end: 0,
            line_excess: 0,
        }
    }
}
impl Utf16Cursor<'_, '_> {
    /// Return the same zero-based line/UTF-16 column as LineIndex::utf16_position.
    pub fn position(&mut self, offset: usize) -> Option<(u32, u32)> {
        let index = self.index;
        if offset > index.text.len() || !index.text.is_char_boundary(offset) {
            return None;
        }
        if offset < index.starts[self.line]
            || index
                .starts
                .get(self.line + 1)
                .is_some_and(|&next| next <= offset)
        {
            self.line = index.starts.partition_point(|&start| start <= offset) - 1;
            self.line_excess = index.excess_at(index.starts[self.line]);
        }
        let old_end = self.excess_end;
        if old_end
            .checked_sub(1)
            .is_some_and(|i| index.utf8_excess[i].0 > offset)
            || index
                .utf8_excess
                .get(old_end)
                .is_some_and(|&(end, _)| end <= offset)
        {
            self.excess_end = index.utf8_excess.partition_point(|&(end, _)| end <= offset);
        }
        let excess = self
            .excess_end
            .checked_sub(1)
            .map_or(0, |i| index.utf8_excess[i].1);
        Some((
            self.line.try_into().ok()?,
            (offset - index.starts[self.line] - (excess - self.line_excess))
                .try_into()
                .ok()?,
        ))
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;
    #[test]
    fn cursor_matches_random_access_for_forward_reverse_repeated_and_rejected_offsets() {
        for source in [
            "",
            "a",
            "\r\n",
            "a😀あ\r\nb\n",
            "雪😀\0x\rmore",
            "\n\n\n",
            "ascii only no newlines",
        ] {
            let index = LineIndex::new(source);
            let mut cursor = index.utf16_cursor();
            let offsets = (0..=source.len() + 2)
                .chain((0..=source.len()).rev())
                .chain((0..=source.len()).flat_map(|i| [0, i, source.len(), i, usize::MAX]));
            for offset in offsets {
                assert_eq!(
                    cursor.position(offset),
                    index.utf16_position(offset),
                    "{source:?}/{offset}"
                );
            }
        }
        let source = (0..3000)
            .map(|i| {
                if i % 3 == 0 {
                    format!("line{i} 雪😀\r\n")
                } else {
                    format!("line{i}\n")
                }
            })
            .collect::<String>();
        let index = LineIndex::new(&source);
        let mut cursor = index.utf16_cursor();
        for offset in (0..source.len())
            .step_by(31)
            .flat_map(|i| [i, source.len() - i, 0, i])
        {
            assert_eq!(cursor.position(offset), index.utf16_position(offset));
        }
    }
}

#[cfg(test)]
mod utf16_length_tests {
    use super::utf16_len;
    #[test]
    fn ascii_fast_path_and_unicode_fallback_match_code_units_exactly() {
        for count in [0, 1, 7, 8, 15, 16, 31, 32, 127, 128, 1024, 8192] {
            let ascii = "x".repeat(count);
            assert_eq!(utf16_len(&ascii), ascii.encode_utf16().count());
            for part in ["\0\t\r\n", "é", "雪", "😀", "e\u{301}", "\u{10ffff}"] {
                for text in [
                    format!("{part}{ascii}"),
                    format!("{ascii}{part}"),
                    format!("{ascii}{part}{ascii}"),
                ] {
                    assert_eq!(utf16_len(&text), text.encode_utf16().count());
                }
            }
        }
    }
}
