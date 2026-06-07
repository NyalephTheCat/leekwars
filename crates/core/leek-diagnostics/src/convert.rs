//! Convert boundary error types into [`Diagnostic`].

use leek_span::Span;

use crate::{Diagnostic, codes};

/// Build a lowering diagnostic at `span`.
pub fn lowering_unsupported(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::LOWERING_UNSUPPORTED, span, message)
}

/// Manifest parse failure (no source span).
pub fn manifest_error(message: impl Into<String>) -> Diagnostic {
    let sid = leek_span::SourceId::new(1).expect("non-zero source id");
    Diagnostic::error(codes::MANIFEST_PARSE_ERROR, Span::new(sid, 0, 0), message)
}
