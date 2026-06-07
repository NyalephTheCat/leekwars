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
/// Edits are kept sorted by start offset. Overlap is rejected
/// at push time. Adjacent edits (`a.end == b.start`) are allowed.
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
        let idx = self.edits.partition_point(|e| e.start < start);
        if idx > 0 {
            let prev = &self.edits[idx - 1];
            if prev.end > start {
                return Err(EditError::Overlap {
                    existing: (prev.start, prev.end),
                    incoming: (start, end),
                });
            }
        }
        if idx < self.edits.len() {
            let next = &self.edits[idx];
            if end > next.start {
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

    /// Apply every edit to `source`. The set must have been built
    /// against `source` (we don't re-check length here; callers can
    /// always re-build).
    ///
    /// Edits are applied in reverse-start order so later offsets
    /// stay valid as earlier ones are rewritten.
    pub fn apply(&self, source: &str) -> String {
        if self.edits.is_empty() {
            return source.to_string();
        }
        let mut out = String::with_capacity(source.len());
        let mut cursor = 0usize;
        for e in &self.edits {
            let s = e.start as usize;
            let t = e.end as usize;
            if s > cursor {
                // `get` (not direct slicing) so a malformed offset / non-UTF-8
                // boundary degrades instead of panicking. Edits are validated
                // at push time, so this is defensive against invariant breaks.
                out.push_str(source.get(cursor..s).unwrap_or(""));
            }
            out.push_str(&e.replacement);
            cursor = t;
        }
        if cursor < source.len() {
            out.push_str(source.get(cursor..).unwrap_or(""));
        }
        out
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
        assert_eq!(set.apply(src()), src());
    }

    #[test]
    fn single_replace() {
        let mut set = EditSet::new(src().len());
        set.replace_span(span(4, 5), "y".into()).unwrap();
        assert_eq!(set.apply(src()), "var y = 1;\nvar y = 2;\nvar z = 3;\n");
    }

    #[test]
    fn multiple_disjoint_edits_apply_in_order() {
        let mut set = EditSet::new(src().len());
        set.replace_span(span(4, 5), "a".into()).unwrap();
        set.replace_span(span(15, 16), "b".into()).unwrap();
        set.replace_span(span(26, 27), "c".into()).unwrap();
        assert_eq!(set.apply(src()), "var a = 1;\nvar b = 2;\nvar c = 3;\n");
    }

    #[test]
    fn out_of_order_pushes_get_sorted() {
        let mut set = EditSet::new(src().len());
        set.replace_span(span(26, 27), "c".into()).unwrap();
        set.replace_span(span(4, 5), "a".into()).unwrap();
        set.replace_span(span(15, 16), "b".into()).unwrap();
        assert_eq!(set.apply(src()), "var a = 1;\nvar b = 2;\nvar c = 3;\n");
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
        assert_eq!(set.apply(src()), "var AB= 1;\nvar y = 2;\nvar z = 3;\n");
    }

    #[test]
    fn insertion_via_zero_length_range() {
        let mut set = EditSet::new(src().len());
        set.push(0, 0, "// header\n".into()).unwrap();
        assert!(set.apply(src()).starts_with("// header\nvar x"));
    }

    #[test]
    fn deletion_with_empty_replacement() {
        let mut set = EditSet::new(src().len());
        set.delete_span(span(0, 11)).unwrap();
        assert_eq!(set.apply(src()), "var y = 2;\nvar z = 3;\n");
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
        assert_eq!(set.apply(src()), "var a = 1;\nvar b = 2;\nvar z = 3;\n");
    }
}
