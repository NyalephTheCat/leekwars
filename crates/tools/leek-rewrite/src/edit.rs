//! Single edit and validation errors.

use leek_span::Span;
use leek_syntax::{SyntaxNode, SyntaxToken};

/// A single text-replacement edit. Byte offsets are into the
/// original source the [`EditSet`](crate::EditSet) was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Inclusive start (byte offset).
    pub start: u32,
    /// Exclusive end (byte offset).
    pub end: u32,
    /// Text that replaces `[start, end)`. Empty string means a pure
    /// deletion; zero-length range means a pure insertion.
    pub replacement: String,
}

impl Edit {
    /// An edit replacing the byte range `[start, end)`.
    #[must_use]
    pub fn new(start: u32, end: u32, replacement: String) -> Self {
        Self {
            start,
            end,
            replacement,
        }
    }

    /// An edit replacing `span`'s bytes. The span's `SourceId` is
    /// ignored — offsets are into the source the set was built for.
    #[must_use]
    pub fn for_span(span: Span, replacement: String) -> Self {
        Self::new(span.start, span.end, replacement)
    }

    /// An edit replacing a token's text.
    #[must_use]
    pub fn for_token(token: &SyntaxToken, replacement: String) -> Self {
        let r = token.text_range();
        Self::new(u32::from(r.start()), u32::from(r.end()), replacement)
    }

    /// An edit replacing a node's full text range.
    #[must_use]
    pub fn for_node(node: &SyntaxNode, replacement: String) -> Self {
        let r = node.text_range();
        Self::new(u32::from(r.start()), u32::from(r.end()), replacement)
    }

    /// True when the edit adds text without removing any
    /// (`start == end`), i.e. it names a position, not a range.
    #[must_use]
    pub fn is_insert(&self) -> bool {
        self.start == self.end
    }

    /// True when `[start, end)` cannot coexist with this edit.
    ///
    /// Two replacements conflict as soon as they share a byte. An
    /// insertion owns no bytes, so it conflicts only when it lands
    /// *strictly inside* a replaced range: an insertion sitting at
    /// another edit's start or end is touching, not overlapping, and
    /// two insertions at the same offset never conflict. The relation
    /// is symmetric, so the verdict does not depend on which edit was
    /// pushed first.
    #[must_use]
    pub fn conflicts_with(&self, start: u32, end: u32) -> bool {
        match (self.is_insert(), start == end) {
            (true, true) => false,
            (true, false) => start < self.start && self.start < end,
            (false, true) => self.start < start && start < self.end,
            (false, false) => self.start.max(start) < self.end.min(end),
        }
    }
}

/// Why an attempted edit was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// `start > end`.
    InvalidRange { start: u32, end: u32 },
    /// `end` extends past the source length.
    OutOfBounds { end: u32, source_len: u32 },
    /// The new edit overlaps a previously-pushed edit. Adjacent
    /// (touching at one endpoint) is allowed.
    Overlap {
        existing: (u32, u32),
        incoming: (u32, u32),
    },
    /// An offset falls inside a multi-byte UTF-8 character, so the
    /// text around it cannot be sliced.
    NotCharBoundary { offset: u32 },
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::InvalidRange { start, end } => {
                write!(f, "invalid edit range {start}..{end}")
            }
            EditError::OutOfBounds { end, source_len } => {
                write!(f, "edit end {end} exceeds source length {source_len}")
            }
            EditError::Overlap { existing, incoming } => write!(
                f,
                "edit {}..{} overlaps existing edit {}..{}",
                incoming.0, incoming.1, existing.0, existing.1
            ),
            EditError::NotCharBoundary { offset } => {
                write!(f, "edit offset {offset} is not a character boundary")
            }
        }
    }
}

impl std::error::Error for EditError {}

impl EditError {
    /// Map a rejected edit to a rewrite diagnostic. `span` should
    /// cover the edit target in the source being modified.
    pub fn to_diagnostic(self, span: Span) -> leek_diagnostics::Diagnostic {
        use leek_diagnostics::{Diagnostic, codes};
        match self {
            EditError::InvalidRange { start, end } => Diagnostic::error(
                codes::EDIT_INVALID_RANGE,
                span,
                format!("invalid edit range {start}..{end}"),
            ),
            EditError::OutOfBounds { end, source_len } => Diagnostic::error(
                codes::EDIT_OUT_OF_BOUNDS,
                span,
                format!("edit end {end} exceeds source length {source_len}"),
            ),
            EditError::Overlap { existing, incoming } => Diagnostic::error(
                codes::EDIT_OVERLAP,
                span,
                format!(
                    "edit {}..{} overlaps existing edit {}..{}",
                    incoming.0, incoming.1, existing.0, existing.1
                ),
            ),
            EditError::NotCharBoundary { offset } => Diagnostic::error(
                codes::EDIT_NOT_CHAR_BOUNDARY,
                span,
                format!("edit offset {offset} is not a character boundary"),
            ),
        }
    }
}
