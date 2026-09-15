//! Every salsa-tracked query in the workspace, under one import path.
//!
//! Re-exports, with one group of exceptions: the include-graph queries
//! in [`crate::include`] are defined here, because they span several
//! files and read the [`WorkspaceFiles`](crate::WorkspaceFiles) input
//! this crate owns. Each query still lives in the pass crate that computes
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
//! computes each stage once. There is no second cascade for an indexed
//! on-disk file: it is the same input, carrying its canonical path, so it
//! shares every memo on this one.
//!
//! `leek-fmt`'s `format_query` is deliberately absent. It is a tools-layer
//! query and `crates/db` may not depend on `crates/tools`; it stays in
//! `leek-fmt`, which depends down on this crate.

/// Pragma preprocessing (`// @version:`, experimental opt-ins).
pub use leek_syntax::pipeline::{PragmaResult, pragma_query};

/// Lexing.
pub use leek_lexer::LexResult;
pub use leek_lexer::pipeline::lex_query;

/// Parsing — one query for every file, buffer or on-disk alike.
pub use leek_parser::pipeline::{ParseQueryResult, parse_query};

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

/// The include closure: one file's include sites, one include name
/// resolved against the workspace, and the whole graph an entry file
/// reaches. Owned by this crate — see [`crate::include`].
pub use crate::include::{
    IncludeGraph, IncludeGraphFile, IncludeRef, include_edges, include_graph,
    include_parse_failures, resolve_include,
};
/// The per-file include scan's result type, from the pure scan the
/// graph and the folder-backed walk share.
pub use leek_resolver::include_graph::{IncludeCall, IncludeEdges, IncludeSite};
