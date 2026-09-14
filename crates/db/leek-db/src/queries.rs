//! Every salsa-tracked query in the workspace, under one import path.
//!
//! Pure re-exports. Each query still lives in the pass crate that computes
//! it and keeps its single memo table there — calling
//! `leek_db::queries::parse_query` and `leek_parser::pipeline::parse_query`
//! hits the same cache entry, because they are the same function. Nothing
//! here wraps, forwards or re-declares a query; a wrapper would be a second
//! memo table over the same work.
//!
//! Each query's result type is re-exported beside it, so a consumer that
//! names a return value does not need the pass crate in its own
//! `Cargo.toml` either.
//!
//! The queries form one cascade over a
//! [`SourceFile`](crate::SourceFile): `complexity_query` → `lower_hir_query`
//! → `parse_query` → `lex_query`, with `typecheck_query`, `resolve_query`
//! and `lower_mir_query` hanging off it, so asking for the deepest one
//! computes each stage once. [`parse_project_file_query`] is the exception:
//! it is keyed on a [`ProjectFile`](crate::ProjectFile) — an indexed on-disk
//! file rather than an editor buffer — and re-lexes internally instead of
//! sharing `lex_query`'s cache.
//!
//! `leek-fmt`'s `format_query` is deliberately absent. It is a tools-layer
//! query and `crates/db` may not depend on `crates/tools`; it stays in
//! `leek-fmt`, which depends down on this crate.

/// Pragma preprocessing (`// @version:`, experimental opt-ins).
pub use leek_syntax::pipeline::{PragmaResult, pragma_query};

/// Lexing.
pub use leek_lexer::LexResult;
pub use leek_lexer::pipeline::lex_query;

/// Parsing: the editor-buffer query and the indexed-project-file query,
/// which share a result type but not a cache key.
pub use leek_parser::pipeline::{ParseQueryResult, parse_project_file_query, parse_query};

/// Name resolution.
pub use leek_resolver::pipeline::{ResolveArtifact, resolve_query};

/// Type checking.
pub use leek_types::pipeline::{TypeCheckArtifact, typecheck_query};

/// HIR lowering.
pub use leek_hir::pipeline::{LowerHirResult, lower_hir_query};

/// MIR lowering (at `O0` — a codegen driver optimizes its own copy).
pub use leek_mir::pipeline::{LowerMirQueryResult, lower_mir_query};

/// Complexity / big-O analysis.
pub use leek_complexity::pipeline::{ComplexityReport, complexity_query};
