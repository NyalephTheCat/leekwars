//! Doc-comment extraction and `@<backend>-backend:` directives.
//!
//! The implementation now lives in [`leek_syntax::doc`] so the HIR
//! lowerer and backends can share it; this module re-exports it for the
//! IDE layer's existing call sites.

pub use leek_syntax::doc::{
    BackendDirectives, directives_enabled, doc_and_directives_before, doc_comment_before,
    extract_backend_directives, substitute,
};
