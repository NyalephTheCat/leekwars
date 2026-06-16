//! Constructors for diagnostics raised at crate boundaries (lowering,
//! manifest parsing) where the producer has a finished message but no
//! richer error value to convert from. Error *values* should implement
//! [`IntoDiagnostic`](crate::IntoDiagnostic) instead.

use leek_span::Span;

use crate::{Diagnostic, codes};

/// Build a lowering diagnostic at `span`.
pub fn lowering_unsupported(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::at(codes::LOWERING_UNSUPPORTED, span, message)
}

/// Manifest parse failure (no source span).
pub fn manifest_error(message: impl Into<String>) -> Diagnostic {
    let sid = leek_span::SourceId::new(1).expect("non-zero source id");
    Diagnostic::at(codes::MANIFEST_PARSE_ERROR, Span::new(sid, 0, 0), message)
}
