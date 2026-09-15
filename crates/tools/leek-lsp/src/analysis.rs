//! Typed accessors over the memoized frontend.
//!
//! Every handler in this crate asks the same six questions of a file —
//! what is its green tree, its syntax root, its resolve table, its type
//! table, its HIR, its complexity report — and today each one asks them
//! by planning a [`Pipeline`](leek_pipeline::Pipeline) for a
//! [`Target`](leek_session::Target), running it, and fishing the answer
//! out of the resulting [`Run`](leek_pipeline::Run) by artifact type.
//! That is three steps of ceremony around one salsa query, and it puts
//! the recipe catalogue between a handler and the cache it actually
//! wants.
//!
//! This module is the direct route: one function per question, each a
//! single [`leek_db::queries`] call. `leek-db` is the façade that knows
//! which pass crate owns which query, so a handler that moves onto these
//! accessors stops naming `leek_parser::pipeline`, `leek_resolver::pipeline`
//! and friends entirely.
//!
//! **Same cache, same answers.** These are not a second path to the same
//! work: the memoized pipeline's steps *already* dispatch into these
//! queries (see `run_parse`, `run_resolve`, `run_lower`, `run_analyze` in
//! the pass crates), so an accessor and the equivalent `pipeline::run`
//! read one memo entry. The tests at the bottom of this file pin that —
//! every accessor is compared against the artifact the pipeline produces
//! for the same file, so the handler rewrites that follow are
//! provably behaviour-preserving rather than hopefully so.
//!
//! **What is deliberately missing.** There is no `lints` accessor and no
//! `formatted` accessor, and neither is an oversight. `leek-lint` ships
//! no tracked query at all, so there is nothing to wrap. `leek-fmt`'s
//! [`format_query`](leek_fmt::format_query) does exist, but it is keyed
//! on the source file alone and formats with `FormatOptions::default()`,
//! where [`crate::pipeline::run_formatted`] formats with the options the
//! editor pushed — an accessor over it would quietly ignore the user's
//! settings. Formatting and linting therefore stay on the pipeline until
//! an options-keyed `format_query` and a `lint_query` exist.
//!
//! # Includes
//!
//! These accessors answer for *one* file, exactly like
//! [`crate::pipeline::run_on_file`]. The include-aware path
//! ([`crate::pipeline::run_on_file_with_includes`]) assembles a graph
//! outside salsa and deliberately bypasses the single-file queries, so
//! diagnostics — the one consumer that needs the compiler's shared
//! program scope — keeps using the pipeline.

use leek_db::queries;
use leek_db::{Db, SourceFile};
use leek_syntax::SyntaxNode;
use leek_syntax::language::GreenNode;

/// The file's green tree.
///
/// Replaces `run.get::<leek_parser::pipeline::GreenTreeArtifact>()?.0`.
pub fn green_tree(db: &dyn Db, file: SourceFile) -> GreenNode {
    queries::parse_query(db, file).green
}

/// The file's syntax tree, as an owned rowan red-tree root.
///
/// The one accessor worth having for its own sake: thirty call sites in
/// this crate independently spell out
/// `SyntaxNode::new_root(green.0.clone())` after digging a
/// `GreenTreeArtifact` out of a `Run`, and every one of them wants this.
/// The root is owned, so it outlives the borrow the artifact version was
/// tied to — which `handlers::completion::file_root` currently works
/// around by hand.
pub fn syntax_root(db: &dyn Db, file: SourceFile) -> SyntaxNode {
    SyntaxNode::new_root(green_tree(db, file))
}

/// Name resolution: the symbol/reference table plus the resolver's own
/// diagnostics.
///
/// Replaces `run.get::<leek_resolver::pipeline::ResolveArtifact>()`.
pub fn resolved(db: &dyn Db, file: SourceFile) -> queries::ResolveArtifact {
    queries::resolve_query(db, file)
}

/// Type checking: the type table, the inferred signatures, and the
/// checker's own diagnostics.
///
/// Replaces `run.get::<leek_types::pipeline::TypeCheckArtifact>()`.
pub fn typed(db: &dyn Db, file: SourceFile) -> queries::TypeCheckArtifact {
    queries::typecheck_query(db, file)
}

/// Lowered HIR (unoptimized), plus the lowering pass's diagnostics.
///
/// Replaces `run.get::<leek_hir::pipeline::HirArtifact>()`, whose payload
/// is this result's `hir` field. The query is keyed on the file alone and
/// so always lowers at `OptLevel::O0` — which is what the LSP asks for
/// anyway ([`leek_session::lsp_params`] is `RecipeParams::permissive`).
pub fn hir(db: &dyn Db, file: SourceFile) -> queries::LowerHirResult {
    queries::lower_hir_query(db, file)
}

/// Per-function / per-method complexity estimates.
///
/// Replaces `run.get::<leek_complexity::pipeline::ComplexityArtifact>()`,
/// whose payload is this report's field.
pub fn complexity(db: &dyn Db, file: SourceFile) -> queries::ComplexityReport {
    queries::complexity_query(db, file)
}

/// Each accessor must return exactly what the handler it is meant to
/// replace gets from [`crate::pipeline::run`] today. These tests are the
/// reason the handler rewrites that follow can be called
/// behaviour-preserving: an accessor nobody checked against the existing
/// path is a second opinion, not a refactor.
#[cfg(test)]
mod tests {
    use leek_session::Target;
    use leek_syntax::SyntaxNode;
    use tower_lsp::lsp_types::Url;

    use super::{complexity, green_tree, hir, resolved, syntax_root, typed};
    use crate::workspace::Workspace;

    /// Exercises every stage: a function with a loop (so the complexity
    /// report is non-trivial), a local and a global (so the resolve table
    /// has both kinds), and a call (so the type checker has something to
    /// infer).
    const SRC: &str = "function sum(arr) {\n\
                       \tvar total = 0\n\
                       \tfor (var x in arr) {\n\
                       \t\ttotal = total + x\n\
                       \t}\n\
                       \treturn total\n\
                       }\n\
                       global answer = sum([1, 2, 3])\n";

    /// A file the parser only gets through by recovering. The accessors
    /// and the pipeline have to agree on the *broken* tree too — that is
    /// the state an editor spends most of its time in.
    const BROKEN_SRC: &str = "function f(\nvar = ;\nreturn oops\n";

    fn fixture(text: &str) -> (Workspace, Url) {
        let mut ws = Workspace::default();
        let uri = Url::parse("untitled:analysis.leek").expect("uri");
        ws.open(uri.clone(), text.to_string());
        (ws, uri)
    }

    /// The open buffer's salsa input — the same one the handlers pass to
    /// [`crate::pipeline::run_on_file`].
    fn source_file(ws: &Workspace, uri: &Url) -> leek_db::SourceFile {
        ws.doc(uri).expect("open doc").source_file
    }

    #[test]
    fn green_tree_matches_the_pipelines_green_tree_artifact() {
        for text in [SRC, BROKEN_SRC] {
            let (ws, uri) = fixture(text);
            let run = crate::pipeline::run(&ws, &uri, Target::Parsed).expect("parsed run");
            let artifact = run
                .get::<leek_parser::pipeline::GreenTreeArtifact>()
                .expect("green tree artifact");
            assert_eq!(green_tree(&ws.db, source_file(&ws, &uri)), artifact.0);
        }
    }

    /// The payoff accessor. Thirty sites in this crate spell out
    /// `SyntaxNode::new_root(green.0.clone())`; this is that expression,
    /// and it has to build the identical tree.
    #[test]
    fn syntax_root_matches_the_root_handlers_build_by_hand() {
        for text in [SRC, BROKEN_SRC] {
            let (ws, uri) = fixture(text);
            let run = crate::pipeline::run(&ws, &uri, Target::Parsed).expect("parsed run");
            let artifact = run
                .get::<leek_parser::pipeline::GreenTreeArtifact>()
                .expect("green tree artifact");
            let by_hand = SyntaxNode::new_root(artifact.0.clone());
            let accessor = syntax_root(&ws.db, source_file(&ws, &uri));

            assert_eq!(accessor.green().into_owned(), by_hand.green().into_owned());
            assert_eq!(accessor.kind(), by_hand.kind());
            assert_eq!(accessor.text_range(), by_hand.text_range());
            assert_eq!(accessor.to_string(), by_hand.to_string());
            assert_eq!(accessor.to_string(), text, "the root covers the whole file");
        }
    }

    #[test]
    fn resolved_matches_the_pipelines_resolve_artifact() {
        for text in [SRC, BROKEN_SRC] {
            let (ws, uri) = fixture(text);
            let run = crate::pipeline::run(&ws, &uri, Target::Resolved).expect("resolved run");
            let artifact = run
                .get::<leek_resolver::pipeline::ResolveArtifact>()
                .expect("resolve artifact");
            assert_eq!(&resolved(&ws.db, source_file(&ws, &uri)), artifact);
        }
        // Guard against a fixture that resolves to nothing: two empty
        // tables compare equal without proving anything.
        let (ws, uri) = fixture(SRC);
        assert!(
            !resolved(&ws.db, source_file(&ws, &uri))
                .table
                .symbols
                .is_empty(),
            "the fixture must declare symbols for the comparison to bite"
        );
    }

    #[test]
    fn typed_matches_the_pipelines_type_check_artifact() {
        for text in [SRC, BROKEN_SRC] {
            let (ws, uri) = fixture(text);
            let run = crate::pipeline::run(&ws, &uri, Target::TypeChecked).expect("typed run");
            let artifact = run
                .get::<leek_types::pipeline::TypeCheckArtifact>()
                .expect("type check artifact");
            assert_eq!(&typed(&ws.db, source_file(&ws, &uri)), artifact);
        }
    }

    #[test]
    fn hir_matches_the_pipelines_hir_artifact() {
        for text in [SRC, BROKEN_SRC] {
            let (ws, uri) = fixture(text);
            let run = crate::pipeline::run(&ws, &uri, Target::Hir).expect("hir run");
            let artifact = run
                .get::<leek_hir::pipeline::HirArtifact>()
                .expect("hir artifact");
            let accessor = hir(&ws.db, source_file(&ws, &uri));
            assert_eq!(accessor.hir, artifact.0);
            // Both sides clone the same memoized `Arc`, so this is one
            // lowering rather than two that happen to agree.
            assert!(std::sync::Arc::ptr_eq(&accessor.hir, &artifact.0));
        }
    }

    #[test]
    fn complexity_matches_the_pipelines_complexity_artifact() {
        for text in [SRC, BROKEN_SRC] {
            let (ws, uri) = fixture(text);
            let run = crate::pipeline::run(&ws, &uri, Target::Complexity).expect("complexity run");
            let artifact = run
                .get::<leek_complexity::pipeline::ComplexityArtifact>()
                .expect("complexity artifact");
            assert_eq!(complexity(&ws.db, source_file(&ws, &uri)).0, artifact.0);
        }
        let (ws, uri) = fixture(SRC);
        assert!(
            !complexity(&ws.db, source_file(&ws, &uri)).0.is_empty(),
            "the fixture must have a function to measure"
        );
    }
}
