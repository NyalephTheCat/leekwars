//! Typed accessors over the memoized frontend.
//!
//! Every handler in this crate asks the same six questions of a file —
//! what is its green tree, its syntax root, its resolve table, its type
//! table, its HIR, its complexity report — and each one used to ask them
//! by planning a pipeline for a [`Target`](leek_session::Target),
//! running it, and fishing the answer out of the resulting run by
//! artifact type. That was three steps of ceremony around one salsa
//! query, with a recipe catalogue between a handler and the cache it
//! actually wanted.
//!
//! No handler goes that way any more, and there is no longer a way to.
//! Every one of the six questions is asked here, by [`green_tree`],
//! [`syntax_root`], [`resolved`], [`typed`], [`hir`] and [`complexity`].
//!
//! **Ask for each answer where it is needed.** A run planned for a late
//! target executed every earlier step whether or not the handler read
//! it, so a hover that landed on a local still lowered HIR and measured
//! complexity, and a completion request still type-checked to answer a
//! global-name query. An accessor costs nothing until it is called, so
//! the handlers call each one inside the branch that reads it: hover
//! asks for [`complexity`] only on a top-level function,
//! `implementation` for [`hir`] only on a method, completion for
//! [`typed`] only in member mode and [`resolved`] only in global mode.
//! The saving is per keystroke, and it multiplies by the program scope
//! in the handlers that fan out over every file.
//!
//! This module is the direct route: one function per question, each a
//! single [`leek_db::queries`] call. `leek-db` is the façade that knows
//! which pass crate owns which query, so a handler on these accessors
//! never names `leek_parser::pipeline`, `leek_resolver::pipeline` and
//! friends at all.
//!
//! **Nothing is deliberately missing.** Formatting and linting were the
//! two holdouts, for different reasons, and both are resolved:
//! `format_query` interns its [`FormatOptions`](leek_fmt::FormatOptions)
//! into the key instead of formatting with defaults, so the editor's own
//! settings go through the cache rather than around it; and
//! [`crate::diagnostics`] moved onto `program_diagnostics_with_lints`
//! once `Workspace::resync` began closing its file set under includes,
//! which is what made the query layer's closure match the include
//! folder's.
//!
//! # Includes
//!
//! These accessors answer for *one* file. The consumer that needs the
//! compiler's shared program scope — [`crate::diagnostics`] — does not
//! come through here at all: it asks the whole-program queries, over an
//! include closure `Workspace::resync` keeps registered as inputs.
//!
//! That is also the whole scope of the program-wide `class` set here.
//! Each of these parses with no cross-file classes at all, because a
//! file analyzed on its own has none: the workspace-wide union the
//! server used to push into every file's salsa input made a class typed
//! into one AI change how an unrelated AI parsed (#163), and it is gone.
//! A closure's classes reach the passes through
//! `leek_db::queries::program_classes`, which keys them per program.

use std::sync::Arc;

use leek_db::queries;
use leek_db::{Db, ProgramClasses, SourceFile};
use leek_syntax::SyntaxNode;
use leek_syntax::language::GreenNode;

/// The file's green tree.
///
/// Parsed under the empty [`ProgramClasses`] set, like every other
/// accessor here and like the single-file queries they wrap: these
/// answer for one file, so the only classes in scope are the ones the
/// parser's own token pre-scan finds in it. A program's class set comes
/// from `leek_db::queries::program_classes` and belongs to the
/// include-aware path — see the [module docs](self).
pub fn green_tree(db: &dyn Db, file: SourceFile) -> GreenNode {
    queries::parse_query(db, file, ProgramClasses::none(db)).green
}

/// The file's syntax tree, as an owned rowan red-tree root.
///
/// The one accessor worth having for its own sake: the handlers used to
/// spell out `SyntaxNode::new_root(green.0.clone())` at twenty-nine
/// sites, each after digging a `GreenTreeArtifact` out of a `Run`, and
/// every one of them wanted this. Twenty-eight now call here; the
/// twenty-ninth (`handlers::on_type_formatting`) needs the green node
/// itself for `leek_fmt::format_range` and so takes [`green_tree`]. The
/// root is owned, so it outlives the borrow the artifact version was
/// tied to — which `handlers::completion::file_root` used to work around
/// by hand, and which is why that helper no longer exists.
pub fn syntax_root(db: &dyn Db, file: SourceFile) -> SyntaxNode {
    SyntaxNode::new_root(green_tree(db, file))
}

/// Name resolution: the symbol/reference table plus the resolver's own
/// diagnostics.
///
/// The one whose fan-out paid for the accessors. The helpers behind
/// references, rename, document highlight, the two hierarchies,
/// `implementation`, workspace symbols and cross-file completion ask
/// this once per file in the program scope, and each of those calls
/// used to plan a whole pipeline for one table.
///
/// [`resolve_query`](queries::resolve_query) reads its AST from
/// `parse_query(db, file, ProgramClasses::none(db))`, which is
/// [`green_tree`] exactly. A handler that takes its root from
/// [`syntax_root`] and its table from here therefore reads both against
/// one parse by construction, rather than against two that happen to
/// agree.
pub fn resolved(db: &dyn Db, file: SourceFile) -> queries::ResolveArtifact {
    queries::resolve_query(db, file)
}

/// Type checking: the type table, the inferred signatures, and the
/// checker's own diagnostics.
///
/// Always answers, where the artifact it replaced was an `Option`: the
/// pipeline's type-check step inserted unconditionally, so `None` never
/// meant "no table" — it meant the run had not reached the step.
pub fn typed(db: &dyn Db, file: SourceFile) -> queries::TypeCheckArtifact {
    queries::typecheck_query(db, file)
}

/// Lowered HIR (unoptimized), plus the lowering pass's diagnostics.
///
/// Keyed on the file alone, so it always lowers at `OptLevel::O0` —
/// which is what the LSP asks for anyway
/// ([`leek_session::lsp_params`] is `CompileParams::permissive`).
pub fn hir(db: &dyn Db, file: SourceFile) -> queries::LowerHirResult {
    queries::lower_hir_query(db, file)
}

/// Per-function / per-method complexity estimates.
///
/// Always answers, with an empty report where the artifact it replaced
/// could simply be absent. Every caller already treated the two the same
/// way: neither finds a row for the function it asked about.
pub fn complexity(db: &dyn Db, file: SourceFile) -> queries::ComplexityReport {
    queries::complexity_query(db, file)
}

/// The formatted text for a file, under `opts`.
///
/// The settings are interned into the query key rather than defaulted, so the
/// editor's own `[format]` table goes through the cache instead of around
/// it — formatting on a keystroke used to be the one path that could
/// never hit a memo, because any project with a `[format]` table had
/// non-default options and the query only ever formatted with defaults.
///
/// **Unverified**: the caller must run
/// [`leek_fmt::check_equivalence`] before putting this in a buffer.
pub fn formatted(db: &dyn Db, file: SourceFile, opts: &leek_fmt::FormatOptions) -> Arc<String> {
    leek_fmt::query::format_query(
        db,
        file,
        leek_fmt::query::FormatConfig::new(db, opts.clone()),
    )
    .text
}

/// The accessors used to be checked against the pipeline run each one
/// replaced — the comparison that made those handler rewrites a refactor
/// rather than a second opinion. There is no run left to compare
/// against, so what remains is the part that is still falsifiable: the
/// accessors answer, they agree with each other where they are two views
/// of one thing, and they answer for a file the parser only got through
/// by recovering, which is the state an editor spends most of its time
/// in.
#[cfg(test)]
mod tests {
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

    /// A file the parser only gets through by recovering.
    const BROKEN_SRC: &str = "function f(\nvar = ;\nreturn oops\n";

    fn fixture(text: &str) -> (Workspace, Url) {
        let mut ws = Workspace::default();
        let uri = Url::parse("untitled:analysis.leek").expect("uri");
        ws.open(uri.clone(), text.to_string());
        (ws, uri)
    }

    /// The open buffer's salsa input — the same one every handler passes
    /// to these accessors.
    fn source_file(ws: &Workspace, uri: &Url) -> leek_db::SourceFile {
        ws.doc(uri).expect("open doc").source_file
    }

    /// The payoff accessor. The handler sites that used to spell out
    /// `SyntaxNode::new_root(green.0.clone())` call this instead; it is
    /// that expression, over the same memoized tree.
    #[test]
    fn syntax_root_is_the_green_tree_rooted() {
        for text in [SRC, BROKEN_SRC] {
            let (ws, uri) = fixture(text);
            let file = source_file(&ws, &uri);
            let by_hand = SyntaxNode::new_root(green_tree(&ws.db, file));
            let accessor = syntax_root(&ws.db, file);

            assert_eq!(accessor.green().into_owned(), by_hand.green().into_owned());
            assert_eq!(accessor.kind(), by_hand.kind());
            assert_eq!(accessor.text_range(), by_hand.text_range());
            assert_eq!(accessor.to_string(), text, "the root covers the whole file");
        }
    }

    /// Every accessor answers for a broken buffer rather than going dark:
    /// the LSP's parameters are permissive precisely so hover, completion
    /// and go-to-definition keep working mid-edit.
    #[test]
    fn every_accessor_answers_for_a_file_that_only_parsed_by_recovering() {
        let (ws, uri) = fixture(BROKEN_SRC);
        let file = source_file(&ws, &uri);
        let _ = green_tree(&ws.db, file);
        let _ = resolved(&ws.db, file);
        let _ = typed(&ws.db, file);
        let _ = hir(&ws.db, file);
        let _ = complexity(&ws.db, file);
    }

    /// Guard against a fixture that analyses to nothing: empty tables
    /// compare equal to each other without proving anything, so the
    /// assertions above are only worth something if this holds.
    #[test]
    fn the_fixture_gives_every_accessor_something_to_report() {
        let (ws, uri) = fixture(SRC);
        let file = source_file(&ws, &uri);
        assert!(
            !resolved(&ws.db, file).table.symbols.is_empty(),
            "the fixture must declare symbols"
        );
        assert!(
            !typed(&ws.db, file).table.exprs.is_empty(),
            "the fixture must infer types"
        );
        assert!(
            !hir(&ws.db, file).hir.defs.is_empty(),
            "the fixture must lower a definition"
        );
        assert!(
            !complexity(&ws.db, file).0.is_empty(),
            "the fixture must have a function to measure"
        );
    }
}
