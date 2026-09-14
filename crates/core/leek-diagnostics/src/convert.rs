//! Constructors for diagnostics raised at crate boundaries (lowering) where
//! the producer has a finished message but no richer error value to convert
//! from. Error *values* should implement
//! [`IntoDiagnostic`](crate::IntoDiagnostic) instead — manifest errors did
//! live here, and the constructor had to invent a `SourceId`, which is
//! precisely the mistake the trait exists to prevent.

use leek_span::Span;

use crate::{Diagnostic, codes};

/// Build a lowering diagnostic at `span`.
pub fn lowering_unsupported(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::at(codes::LOWERING_UNSUPPORTED, span, message)
}

/// Build a malformed-MIR diagnostic at `span`. Raised by leek-mir's
/// verifier; `span` points at the offending function (or, where the
/// verifier can narrow it, the offending statement).
pub fn malformed_mir(span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::at(codes::MALFORMED_MIR, span, message)
}
