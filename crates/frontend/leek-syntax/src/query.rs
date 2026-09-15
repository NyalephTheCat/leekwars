//! Pragma preprocessing as a tracked query.
//!
//! [`version_from_byte`], which decodes a [`SourceFile`]'s `version_byte`
//! into a [`Version`](crate::version::Version), lives in
//! [`crate::version`] and is re-exported here for the passes that reach
//! for it through this module.
//!
//! [`SourceFile`]: leek_query::salsa::SourceFile

use leek_diagnostics::Diagnostic;

use crate::pragma::{Pragmas, parse_pragmas};

pub use crate::version::version_from_byte;

/// Tracked return value: pragmas + their parse-time diagnostics.
/// Single-struct return so the salsa-tracked query is well-formed.
#[derive(salsa::Update, Debug, Clone, PartialEq, Eq)]
pub struct PragmaResult {
    pub pragmas: Pragmas,
    pub diagnostics: Vec<Diagnostic>,
}

/// Salsa-tracked entry point for pragma preprocessing. Re-runs only
/// when the input [`SourceFile`](leek_query::salsa::SourceFile)'s
/// text changes.
///
/// A file's `version_byte` and `strict` are *not* derived here: a driver
/// settles them once at the input boundary with
/// `leek_span::pragma::LanguageSettings::resolve` (override > pragma >
/// default) and every pass reads them back off the input. This query
/// contributes the parsed pragmas — the experimental opt-ins — plus any
/// pragma diagnostics.
#[salsa::tracked]
pub fn pragma_query(
    db: &dyn leek_query::salsa::Db,
    file: leek_query::salsa::SourceFile,
) -> PragmaResult {
    let (pragmas, diagnostics) = parse_pragmas(file.text(db), file.source(db));
    PragmaResult {
        pragmas,
        diagnostics,
    }
}
