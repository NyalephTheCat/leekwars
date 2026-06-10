//! LSP `Position` ↔ byte-offset conversion.
//!
//! LSP positions address a character by **UTF-16 code units** within a line,
//! while the compiler works in **byte** offsets. Converting between them needs
//! the line's source text, so the conversion lives on [`PosMap`], which pairs
//! a [`LineTable`] with the text it was built from. For ASCII this is a no-op
//! (1 byte = 1 UTF-16 unit), but for any line containing non-ASCII characters
//! the byte and UTF-16 columns diverge, and getting this wrong makes every
//! position-anchored feature (hover, completion, definition, rename, …) point
//! at the wrong place.

use leek_span::{LineTable, Span};
use tower_lsp::lsp_types as lsp;

/// A [`LineTable`] paired with the source text it was built from. Together they
/// translate between LSP UTF-16 positions and byte offsets. Cheap to copy — it
/// is just two references.
#[derive(Clone, Copy)]
pub struct PosMap<'a> {
    pub line_table: &'a LineTable,
    pub text: &'a str,
}

impl<'a> PosMap<'a> {
    #[must_use]
    pub fn new(line_table: &'a LineTable, text: &'a str) -> Self {
        Self { line_table, text }
    }

    /// LSP `Position` (UTF-16 column) → byte offset. `None` if the line is out
    /// of range. A `character` past the end of the line clamps to the line's
    /// end (matching how editors treat an over-long column).
    #[must_use]
    pub fn to_offset(&self, position: lsp::Position) -> Option<u32> {
        let line_start = self.line_table.line_start(position.line as usize)?;
        let line = self
            .line_table
            .line_text(self.text, position.line as usize)
            .unwrap_or("");
        // Accumulate in `usize` (char lengths are usize) and convert once.
        let target = position.character as usize;
        let mut utf16 = 0usize;
        let mut byte = 0usize;
        for ch in line.chars() {
            if utf16 >= target {
                break;
            }
            utf16 += ch.len_utf16();
            byte += ch.len_utf8();
        }
        Some(line_start + u32::try_from(byte).ok()?)
    }

    /// Byte offset → LSP `Position` (UTF-16 column).
    #[must_use]
    pub fn to_position(&self, offset: u32) -> lsp::Position {
        let lc = self.line_table.line_col(offset);
        // `LineCol` is 1-based; LSP is 0-based. `col` is a 1-based *byte*
        // column within the line, so `byte_col` is the 0-based byte offset.
        let line_idx = lc.line.saturating_sub(1);
        let byte_col = lc.col.saturating_sub(1) as usize;
        let line = self
            .line_table
            .line_text(self.text, line_idx as usize)
            .unwrap_or("");
        let mut byte = 0usize;
        let mut utf16 = 0usize;
        for ch in line.chars() {
            if byte >= byte_col {
                break;
            }
            byte += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        lsp::Position {
            line: line_idx,
            character: u32::try_from(utf16).unwrap_or(u32::MAX),
        }
    }

    /// Convert a [`Span`] to an LSP `Range`.
    #[must_use]
    pub fn span_range(&self, span: Span) -> lsp::Range {
        lsp::Range {
            start: self.to_position(span.start),
            end: self.to_position(span.end),
        }
    }
}

/// Convert an LSP zero-based `Position` to a byte offset. Returns `None` if the
/// position's line is out of range. UTF-16-aware (see [`PosMap`]).
#[must_use]
pub fn position_to_offset(pm: PosMap, position: lsp::Position) -> Option<u32> {
    pm.to_offset(position)
}

/// Convert a byte offset to an LSP `Position` (UTF-16 column).
#[must_use]
pub fn offset_to_position(pm: PosMap, offset: u32) -> lsp::Position {
    pm.to_position(offset)
}

/// Convert a [`Span`] to an LSP `Range` against the given map.
#[must_use]
pub fn span_to_range(pm: PosMap, span: Span) -> lsp::Range {
    pm.span_range(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(text: &str, byte_offset: u32) -> u32 {
        let lt = LineTable::new(text);
        let pm = PosMap::new(&lt, text);
        let pos = pm.to_position(byte_offset);
        pm.to_offset(pos).unwrap()
    }

    #[test]
    fn ascii_is_identity() {
        let text = "var x = 1\nreturn x\n";
        let lt = LineTable::new(text);
        let pm = PosMap::new(&lt, text);
        // `var x = 1`: the `1` is at byte/UTF-16 column 8 (ASCII → identity).
        let off = pm
            .to_offset(lsp::Position {
                line: 0,
                character: 8,
            })
            .unwrap();
        assert_eq!(off, 8);
        assert_eq!(&text[(off as usize)..=(off as usize)], "1");
        // Reverse direction is identity too.
        assert_eq!(
            pm.to_position(8),
            lsp::Position {
                line: 0,
                character: 8
            }
        );
    }

    #[test]
    fn non_ascii_column_is_utf16() {
        // `"é"` is 2 UTF-8 bytes but 1 UTF-16 unit; `"😀"` is 4 bytes / 2 units.
        let text = "var s = \"é😀\"\nreturn s\n";
        let lt = LineTable::new(text);
        let pm = PosMap::new(&lt, text);
        // The closing quote on line 0: count UTF-16 units up to it.
        // `var s = "` = 9 units, then `é`=1, `😀`=2 → closing quote at unit 12.
        let off = pm
            .to_offset(lsp::Position {
                line: 0,
                character: 12,
            })
            .unwrap();
        assert_eq!(
            text.as_bytes()[off as usize],
            b'"',
            "should land on closing quote"
        );
        // And the reverse maps that byte back to UTF-16 column 12.
        let pos = pm.to_position(off);
        assert_eq!(
            pos,
            lsp::Position {
                line: 0,
                character: 12
            }
        );
    }

    #[test]
    fn round_trips_through_non_ascii() {
        let text = "// héllo 😀 wörld\nx";
        // The `x` on line 1 round-trips.
        let x_byte = u32::try_from(text.find('x').unwrap()).unwrap();
        assert_eq!(round_trip(text, x_byte), x_byte);
        // A byte on the non-ASCII line round-trips too (the `w`).
        let w_byte = u32::try_from(text.find('w').unwrap()).unwrap();
        assert_eq!(round_trip(text, w_byte), w_byte);
    }
}
