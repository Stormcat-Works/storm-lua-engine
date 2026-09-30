//! Coordinate conversion for immutable UTF-8 source snapshots.
//! Lua diagnostics use one-based byte columns; Source Map consumers use
//! zero-based UTF-16 columns. Both share the same LF/CRLF line index.

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
