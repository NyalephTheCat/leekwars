//! Sorted, validated edit collections.

use leek_diagnostics::{Suggestion, TextEdit as DiagTextEdit};
use leek_span::Span;
use leek_syntax::{SyntaxNode, SyntaxToken};

use crate::edit::{Edit, EditError};

/// Sorted, validated collection of edits over a single source
/// string of known length.
///
/// Construction with [`EditSet::new`](EditSet::new) makes the source's
/// length explicit so [`push`](EditSet::push) methods can reject
/// out-of-bounds spans immediately rather than at apply time.
///
/// Edits are kept in a total order that does not depend on the
/// order they were pushed in: by start offset, then insertions
/// (`start == end`) before the replacement that starts at the same
/// offset, then by push sequence among otherwise-equal edits. Two
/// insertions at one offset therefore apply in the order they were
/// pushed.
///
/// Overlap is rejected at push time, symmetrically: a pair of edits
/// is accepted or rejected the same way whichever one arrives first.
/// Adjacent edits (`a.end == b.start`) are allowed, and so is an
/// insertion at another edit's start or end — an insertion owns no
/// bytes, so it only conflicts strictly inside a replaced range.
#[derive(Debug, Clone, Default)]
pub struct EditSet {
    source_len: u32,
    edits: Vec<Edit>,
}

impl EditSet {
    pub fn new(source_len: usize) -> Self {
        Self {
            source_len: leek_span::offset(source_len),
            edits: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn len(&self) -> usize {
        self.edits.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Edit> {
        self.edits.iter()
    }

    /// Add an edit replacing the byte range `[start, end)` with
    /// `replacement`. Returns `Err` on out-of-range or overlap.
    ///
    /// The edit lands at its position in the set's total order —
    /// `(start, replacement-after-insertion, push sequence)` — so the
    /// stored sequence, and therefore [`apply`](Self::apply)'s output,
    /// is the same for any push order of the same edits.
    pub fn push(&mut self, start: u32, end: u32, replacement: String) -> Result<(), EditError> {
        if start > end {
            return Err(EditError::InvalidRange { start, end });
        }
        if end > self.source_len {
            return Err(EditError::OutOfBounds {
                end,
                source_len: self.source_len,
            });
        }
        // `<=` (not `<`) puts the incoming edit *after* everything that
        // compares equal, so ties keep push order.
        let key = (start, start != end);
        let idx = self
            .edits
            .partition_point(|e| (e.start, e.start != e.end) <= key);
        // Everything before `idx` starts at or before `start`, and at
        // most one such edit can still be open there — necessarily the
        // nearest one, since an earlier edit reaching past `start`
        // would already overlap it.
        if let Some(prev) = idx.checked_sub(1).map(|i| &self.edits[i])
            && prev.conflicts_with(start, end)
        {
            return Err(EditError::Overlap {
                existing: (prev.start, prev.end),
                incoming: (start, end),
            });
        }
        // Forward, several edits may begin inside `[start, end)` (an
        // insertion and the replacement it precedes share a start), so
        // scan until one starts at or after `end`.
        for next in &self.edits[idx..] {
            if next.start >= end {
                break;
            }
            if next.conflicts_with(start, end) {
                return Err(EditError::Overlap {
                    existing: (next.start, next.end),
                    incoming: (start, end),
                });
            }
        }
        self.edits.insert(
            idx,
            Edit {
                start,
                end,
                replacement,
            },
        );
        Ok(())
    }

    /// Convenience: replace `span` with `replacement`. The span's
    /// `SourceId` is ignored — callers are expected to use the same
    /// source as the [`EditSet`] was built for.
    pub fn replace_span(&mut self, span: Span, replacement: String) -> Result<(), EditError> {
        self.push(span.start, span.end, replacement)
    }

    /// Replace a token's text. Useful for renames.
    pub fn replace_token(
        &mut self,
        token: &SyntaxToken,
        replacement: String,
    ) -> Result<(), EditError> {
        let r = token.text_range();
        self.push(u32::from(r.start()), u32::from(r.end()), replacement)
    }

    /// Replace a node's full text range. Useful for "format this
    /// subtree" and structural refactors.
    pub fn replace_node(
        &mut self,
        node: &SyntaxNode,
        replacement: String,
    ) -> Result<(), EditError> {
        let r = node.text_range();
        self.push(u32::from(r.start()), u32::from(r.end()), replacement)
    }

    /// Insert `text` immediately before `node`'s first byte.
    pub fn insert_before(&mut self, node: &SyntaxNode, text: String) -> Result<(), EditError> {
        let off = u32::from(node.text_range().start());
        self.push(off, off, text)
    }

    /// Insert `text` immediately after `node`'s last byte.
    pub fn insert_after(&mut self, node: &SyntaxNode, text: String) -> Result<(), EditError> {
        let off = u32::from(node.text_range().end());
        self.push(off, off, text)
    }

    /// Delete `span`'s bytes.
    pub fn delete_span(&mut self, span: Span) -> Result<(), EditError> {
        self.push(span.start, span.end, String::new())
    }

    /// Add every [`TextEdit`] from a [`Suggestion`]. Returns the
    /// first error if any edit conflicts; the [`EditSet`] is left in
    /// the state it was in just before the failing edit (partial
    /// edits already added stay).
    pub fn push_suggestion(&mut self, sug: &Suggestion) -> Result<(), EditError> {
        for e in &sug.edits {
            self.push_diag_edit(e)?;
        }
        Ok(())
    }

    /// Add one [`leek_diagnostics::TextEdit`] (the diag-crate form).
    pub fn push_diag_edit(&mut self, e: &DiagTextEdit) -> Result<(), EditError> {
        self.push(e.span.start, e.span.end, e.replacement.clone())
    }

    /// Apply every edit to `source`, walking left to right in the
    /// set's stored order so each edit's offsets still refer to the
    /// original text.
    ///
    /// The set is expected to have been built against `source`; every
    /// offset is re-checked against it here, because an edit set
    /// replayed onto different text is the one way a caller can turn
    /// validated edits back into nonsense.
    ///
    /// # Errors
    ///
    /// [`EditError::OutOfBounds`] if an offset points past the end of
    /// `source`, [`EditError::NotCharBoundary`] if one falls inside a
    /// multi-byte character. Either way the text is left alone rather
    /// than silently truncated.
    pub fn apply(&self, source: &str) -> Result<String, EditError> {
        if self.edits.is_empty() {
            return Ok(source.to_string());
        }
        let source_len = leek_span::offset(source.len());
        let mut out = String::with_capacity(source.len());
        let mut cursor = 0usize;
        for e in &self.edits {
            for offset in [e.start, e.end] {
                if offset > source_len {
                    return Err(EditError::OutOfBounds {
                        end: offset,
                        source_len,
                    });
                }
                if !source.is_char_boundary(offset as usize) {
                    return Err(EditError::NotCharBoundary { offset });
                }
            }
            let s = e.start as usize;
            if s > cursor {
                out.push_str(&source[cursor..s]);
            }
            out.push_str(&e.replacement);
            cursor = e.end as usize;
        }
        if cursor < source.len() {
            out.push_str(&source[cursor..]);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_span::SourceId;

    fn src() -> &'static str {
        "var x = 1;\nvar y = 2;\nvar z = 3;\n"
    }

    fn span(start: u32, end: u32) -> Span {
        Span::new(SourceId::new(1).unwrap(), start, end)
    }

    #[test]
    fn empty_set_is_identity() {
        let set = EditSet::new(src().len());
        assert_eq!(set.apply(src()).unwrap(), src());
    }

    #[test]
    fn single_replace() {
        let mut set = EditSet::new(src().len());
        set.replace_span(span(4, 5), "y".into()).unwrap();
        assert_eq!(
            set.apply(src()).unwrap(),
            "var y = 1;\nvar y = 2;\nvar z = 3;\n"
        );
    }

    #[test]
    fn multiple_disjoint_edits_apply_in_order() {
        let mut set = EditSet::new(src().len());
        set.replace_span(span(4, 5), "a".into()).unwrap();
        set.replace_span(span(15, 16), "b".into()).unwrap();
        set.replace_span(span(26, 27), "c".into()).unwrap();
        assert_eq!(
            set.apply(src()).unwrap(),
            "var a = 1;\nvar b = 2;\nvar c = 3;\n"
        );
    }

    #[test]
    fn out_of_order_pushes_get_sorted() {
        let mut set = EditSet::new(src().len());
        set.replace_span(span(26, 27), "c".into()).unwrap();
        set.replace_span(span(4, 5), "a".into()).unwrap();
        set.replace_span(span(15, 16), "b".into()).unwrap();
        assert_eq!(
            set.apply(src()).unwrap(),
            "var a = 1;\nvar b = 2;\nvar c = 3;\n"
        );
    }

    #[test]
    fn overlap_is_rejected() {
        let mut set = EditSet::new(src().len());
        set.replace_span(span(4, 8), "long".into()).unwrap();
        let e = set.replace_span(span(6, 9), "x".into());
        assert!(matches!(e, Err(EditError::Overlap { .. })));
    }

    #[test]
    fn adjacent_edits_are_allowed() {
        let mut set = EditSet::new(src().len());
        set.replace_span(span(4, 5), "A".into()).unwrap();
        set.replace_span(span(5, 6), "B".into()).unwrap();
        assert_eq!(
            set.apply(src()).unwrap(),
            "var AB= 1;\nvar y = 2;\nvar z = 3;\n"
        );
    }

    #[test]
    fn insertion_via_zero_length_range() {
        let mut set = EditSet::new(src().len());
        set.push(0, 0, "// header\n".into()).unwrap();
        assert!(set.apply(src()).unwrap().starts_with("// header\nvar x"));
    }

    #[test]
    fn deletion_with_empty_replacement() {
        let mut set = EditSet::new(src().len());
        set.delete_span(span(0, 11)).unwrap();
        assert_eq!(set.apply(src()).unwrap(), "var y = 2;\nvar z = 3;\n");
    }

    #[test]
    fn out_of_bounds_is_rejected() {
        let mut set = EditSet::new(10);
        let e = set.replace_span(span(0, 20), "x".into());
        assert!(matches!(e, Err(EditError::OutOfBounds { .. })));
    }

    #[test]
    fn invalid_range_is_rejected() {
        let mut set = EditSet::new(src().len());
        let e = set.push(10, 5, "x".into());
        assert!(matches!(e, Err(EditError::InvalidRange { .. })));
    }

    #[test]
    fn push_suggestion_replays_all_edits() {
        use leek_diagnostics::{Applicability, Suggestion, TextEdit as DiagTE};
        let sug = Suggestion {
            message: "rename".into(),
            edits: vec![
                DiagTE {
                    span: span(4, 5),
                    replacement: "a".into(),
                },
                DiagTE {
                    span: span(15, 16),
                    replacement: "b".into(),
                },
            ],
            applicability: Applicability::MachineApplicable,
        };
        let mut set = EditSet::new(src().len());
        set.push_suggestion(&sug).unwrap();
        assert_eq!(
            set.apply(src()).unwrap(),
            "var a = 1;\nvar b = 2;\nvar z = 3;\n"
        );
    }

    /// An insertion at a replacement's start is "touching", not
    /// overlapping, and the pair applies the same way whichever one
    /// was pushed first.
    #[test]
    fn insert_at_a_replacement_start_is_order_independent() {
        let mut replace_first = EditSet::new(src().len());
        replace_first.push(4, 5, "R".into()).unwrap();
        replace_first.push(4, 4, "I".into()).unwrap();

        let mut insert_first = EditSet::new(src().len());
        insert_first.push(4, 4, "I".into()).unwrap();
        insert_first.push(4, 5, "R".into()).unwrap();

        let expected = "var IR = 1;\nvar y = 2;\nvar z = 3;\n";
        assert_eq!(replace_first.apply(src()).unwrap(), expected);
        assert_eq!(insert_first.apply(src()).unwrap(), expected);
    }

    /// Same for an insertion at a replacement's end.
    #[test]
    fn insert_at_a_replacement_end_is_order_independent() {
        let mut replace_first = EditSet::new(src().len());
        replace_first.push(4, 5, "R".into()).unwrap();
        replace_first.push(5, 5, "I".into()).unwrap();

        let mut insert_first = EditSet::new(src().len());
        insert_first.push(5, 5, "I".into()).unwrap();
        insert_first.push(4, 5, "R".into()).unwrap();

        let expected = "var RI = 1;\nvar y = 2;\nvar z = 3;\n";
        assert_eq!(replace_first.apply(src()).unwrap(), expected);
        assert_eq!(insert_first.apply(src()).unwrap(), expected);
    }

    #[test]
    fn inserts_at_the_same_offset_apply_in_push_order() {
        let mut set = EditSet::new(src().len());
        set.push(0, 0, "A".into()).unwrap();
        set.push(0, 0, "B".into()).unwrap();
        set.push(0, 0, "C".into()).unwrap();
        assert!(set.apply(src()).unwrap().starts_with("ABCvar x"));
    }

    #[test]
    fn insert_inside_a_replacement_is_rejected_either_way() {
        let mut replace_first = EditSet::new(src().len());
        replace_first.push(4, 8, "long".into()).unwrap();
        assert!(matches!(
            replace_first.push(6, 6, "x".into()),
            Err(EditError::Overlap { .. })
        ));

        let mut insert_first = EditSet::new(src().len());
        insert_first.push(6, 6, "x".into()).unwrap();
        assert!(matches!(
            insert_first.push(4, 8, "long".into()),
            Err(EditError::Overlap { .. })
        ));
    }

    /// The set stays sorted when an edit is pushed between an
    /// insertion and the replacement sharing its offset.
    #[test]
    fn a_replacement_spanning_a_tie_is_rejected_from_either_side() {
        let mut set = EditSet::new(src().len());
        set.push(5, 5, "I".into()).unwrap();
        set.push(5, 8, "R".into()).unwrap();
        // 3..7 covers bytes of the 5..8 replacement, two slots past
        // the insertion point the search lands on.
        assert!(matches!(
            set.push(3, 7, "x".into()),
            Err(EditError::Overlap { .. })
        ));
    }

    #[test]
    fn apply_rejects_an_offset_inside_a_character() {
        let text = "héllo";
        let mut set = EditSet::new(text.len());
        // 2 is the second byte of `é`; push can't know that, apply can.
        set.push(2, 2, "x".into()).unwrap();
        assert_eq!(
            set.apply(text),
            Err(EditError::NotCharBoundary { offset: 2 })
        );
    }

    #[test]
    fn apply_rejects_edits_past_the_end_of_the_given_source() {
        let mut set = EditSet::new(src().len());
        set.push(20, 25, "x".into()).unwrap();
        assert!(matches!(
            set.apply("short"),
            Err(EditError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn apply_keeps_multibyte_text_intact() {
        let text = "var é = 1;\n";
        let mut set = EditSet::new(text.len());
        set.push(0, 3, "let".into()).unwrap();
        assert_eq!(set.apply(text).unwrap(), "let é = 1;\n");
    }
}
