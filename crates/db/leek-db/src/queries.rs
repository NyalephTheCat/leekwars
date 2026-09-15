//! Every salsa-tracked query in the workspace, under one import path.
//!
//! Re-exports, with two groups of exceptions: the include-graph queries
//! in [`crate::include`] and the whole-program passes in
//! [`crate::program`] are defined here, because they span several files
//! and read the [`WorkspaceFiles`](crate::WorkspaceFiles) input this
//! crate owns. Each query still lives in the pass crate that computes
//! it and keeps its single memo table there — calling
//! `leek_db::queries::parse_query` and `leek_parser::query::parse_query`
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
//! computes each stage once. That cascade answers for **one file**;
//! `resolve_program`, `typecheck_program` and `lower_program` answer for
//! a whole include closure, sharing one `parse_query` per file keyed on
//! the closure's own `program_classes`. There is no second cascade for an indexed
//! on-disk file: it is the same input, carrying its canonical path, so it
//! shares every memo on this one.
//!
//! `leek-fmt`'s `format_query` is deliberately absent. It is a tools-layer
//! query and `crates/db` may not depend on `crates/tools`; it stays in
//! `leek-fmt`, which depends down on this crate.

/// Pragma preprocessing (`// @version:`, experimental opt-ins).
pub use leek_syntax::query::{PragmaResult, pragma_query};

/// Lexing.
pub use leek_lexer::LexResult;
pub use leek_lexer::query::lex_query;

/// Parsing — one query for every file, buffer or on-disk alike.
pub use leek_parser::query::{ParseQueryResult, parse_query};

/// Name resolution.
pub use leek_resolver::query::{ResolveArtifact, resolve_query};

/// Type checking.
pub use leek_types::query::{TypeCheckArtifact, typecheck_query};

/// HIR lowering.
pub use leek_hir::query::{LowerHirResult, lower_hir_query};

/// MIR lowering (at `O0` — a codegen driver optimizes its own copy).
pub use leek_mir::query::{LowerMirQueryResult, lower_mir_query};

/// Complexity / big-O analysis.
pub use leek_complexity::query::{ComplexityReport, complexity_query};

/// The assembled diagnostic streams, and the per-file filter over them
/// — see [`crate::diagnostics`]. Lint findings are deliberately absent:
/// they are appended by `leek_lint::diagnostics_with_lints`, in the
/// tool, because db may not depend on tools.
pub use crate::diagnostics::{
    Stage, diagnostics_without_lints, file_diagnostics_upto, for_source, program_diagnostics,
    program_diagnostics_upto,
};
/// The include closure: one file's include sites, one include name
/// resolved against the workspace, the whole graph an entry file
/// reaches, and the program-wide class set every parse in it is keyed
/// on. Owned by this crate — see [`crate::include`].
pub use crate::include::{
    IncludeGraph, IncludeGraphFile, IncludeRef, class_names, include_edges, include_graph,
    include_parse_failures, program_classes, resolve_include,
};
pub use crate::program::{
    lower_program, lower_program_mir, program_complexity, resolve_program, typecheck_program,
};
/// The whole-program passes over that closure — see [`crate::program`].
/// The optimization level a whole-program lowering is keyed on. Owned by
/// the database substrate, re-exported here so a caller of
/// [`lower_program`] needs no other import.
pub use leek_query::OptLevel;
/// The per-file include scan's result type, from the pure scan the
/// graph and the folder-backed walk share.
pub use leek_resolver::include_graph::{IncludeCall, IncludeEdges, IncludeSite};
