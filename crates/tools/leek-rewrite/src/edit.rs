//! Single edit and validation errors.

use leek_span::Span;

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
        }
    }
}
