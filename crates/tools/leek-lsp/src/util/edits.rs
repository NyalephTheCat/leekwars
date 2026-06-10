//! Applying `textDocument/didChange` content changes to a buffer.
//!
//! With `TextDocumentSyncKind::INCREMENTAL` the client sends a list of
//! [`TextDocumentContentChangeEvent`]s per change notification instead
//! of the whole new document. Each event either replaces a `range`
//! (the common case — a keystroke) or, when `range` is `None`, replaces
//! the entire document (some clients still send full snapshots).
//!
//! The LSP spec requires applying the events **in order**, with each
//! event's range interpreted against the document state produced by the
//! preceding events in the same batch. We honour that by re-deriving a
//! line table from the working text before each ranged splice.
//!
//! Ranges are UTF-16 positions, so the conversion routes through
//! [`PosMap`] — the same byte↔UTF-16 mapping every other handler uses.
//! Getting this wrong would corrupt the mirrored buffer the instant a
//! line contains a non-ASCII character.

use leek_span::LineTable;
use tower_lsp::lsp_types as lsp;

use crate::util::position::PosMap;

/// Apply `changes` to `text` in order, returning the resulting buffer.
#[must_use]
pub fn apply_content_changes(
    mut text: String,
    changes: &[lsp::TextDocumentContentChangeEvent],
) -> String {
    for change in changes {
        match &change.range {
            // Whole-document replacement.
            None => text.clone_from(&change.text),
            // Ranged splice. Convert the UTF-16 range to byte offsets
            // against the *current* working text.
            Some(range) => {
                let line_table = LineTable::new(&text);
                let pm = PosMap::new(&line_table, &text);
                let len = text.len();
                // A missing/over-long position clamps to the buffer end,
                // matching how editors treat an out-of-range column.
                let start = pm
                    .to_offset(range.start)
                    .map_or(len, |o| o as usize)
                    .min(len);
                let end = pm
                    .to_offset(range.end)
                    .map_or(len, |o| o as usize)
                    .clamp(start, len);
                text.replace_range(start..end, &change.text);
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(range: Option<lsp::Range>, text: &str) -> lsp::TextDocumentContentChangeEvent {
        lsp::TextDocumentContentChangeEvent {
            range,
            range_length: None,
            text: text.to_string(),
        }
    }

    fn rng(sl: u32, sc: u32, el: u32, ec: u32) -> lsp::Range {
        lsp::Range {
            start: lsp::Position {
                line: sl,
                character: sc,
            },
            end: lsp::Position {
                line: el,
                character: ec,
            },
        }
    }

    #[test]
    fn full_replacement_when_range_is_none() {
        let out = apply_content_changes("old".into(), &[change(None, "brand new")]);
        assert_eq!(out, "brand new");
    }

    #[test]
    fn single_char_insertion() {
        // Insert `X` at column 3 of `abcd` → `abcXd`.
        let out = apply_content_changes("abcd".into(), &[change(Some(rng(0, 3, 0, 3)), "X")]);
        assert_eq!(out, "abcXd");
    }

    #[test]
    fn range_deletion() {
        // Delete `bc` (cols 1..3) from `abcd` → `ad`.
        let out = apply_content_changes("abcd".into(), &[change(Some(rng(0, 1, 0, 3)), "")]);
        assert_eq!(out, "ad");
    }

    #[test]
    fn replacement_across_lines() {
        // Replace from line0 col5 (`o` of `one`) through line1 col4 (the
        // space before `two`) with `X` → `line X two\n`.
        let out = apply_content_changes(
            "line one\nline two\n".into(),
            &[change(Some(rng(0, 5, 1, 4)), "X")],
        );
        assert_eq!(out, "line X two\n");
    }

    #[test]
    fn sequential_changes_in_one_batch() {
        // Two edits in order: insert `_` after `a`, then `!` at the end.
        // The second range is interpreted against the post-first state.
        let out = apply_content_changes(
            "ab".into(),
            &[
                change(Some(rng(0, 1, 0, 1)), "_"), // "a_b"
                change(Some(rng(0, 3, 0, 3)), "!"), // "a_b!"
            ],
        );
        assert_eq!(out, "a_b!");
    }

    #[test]
    fn utf16_aware_insertion_after_non_ascii() {
        // `é` is 1 UTF-16 unit / 2 bytes. Inserting at UTF-16 column 2
        // (right after `é`) must land on the byte boundary after `é`,
        // not two bytes earlier.
        let out = apply_content_changes("aé".into(), &[change(Some(rng(0, 2, 0, 2)), "Z")]);
        assert_eq!(out, "aéZ");
    }

    #[test]
    fn insertion_past_end_clamps() {
        // A column past the line end clamps to the end rather than
        // panicking on a bad byte index.
        let out = apply_content_changes("hi".into(), &[change(Some(rng(0, 99, 0, 99)), "!")]);
        assert_eq!(out, "hi!");
    }
}
