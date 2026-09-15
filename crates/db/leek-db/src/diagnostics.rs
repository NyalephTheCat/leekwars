//! The diagnostic stream as tracked queries.
//!
//! Every other query in this crate answers one question about a file:
//! its tokens, its tree, its symbol table. A *diagnostic stream* is not
//! one of those — it is the concatenation of what each stage found, in
//! a fixed order, and that order used to exist nowhere. It was an
//! emergent property of the recipe planner that used to sequence the
//! passes: `Pragma` ran before `Lex` because `TokensArtifact` required
//! `PragmasArtifact`, `Parse` before `Resolve` because `ResolveArtifact`
//! required `AstArtifact`, and a run's diagnostics came out in the order
//! the steps emitted them.
//!
//! That order is load-bearing. `leek_lsp::handlers::code_action` matches
//! the diagnostics a client hands back against the ones the server
//! published (`client_shows`), and a client that truncates or renumbers
//! the list degrades the match when the order moves. So the two queries
//! here **pin** the order rather than reproduce it by accident:
//! [`diagnostics_without_lints`] for one file, [`program_diagnostics`]
//! for a whole include closure, each with a test that spells the
//! sequence out.
//!
//! ### Why lints are not in here
//!
//! `leek-lint` is a `crates/tools` crate, rank 6; this crate is
//! `crates/db`, rank 3, and `cargo xtask check-layers` rejects db →
//! tools. So the split runs the other way: this crate produces
//! everything up to and including MIR lowering, and
//! `leek_lint::diagnostics_with_lints` — *in the tool*, depending
//! **down** on this crate — appends `lint_query`'s findings to
//! [`diagnostics_without_lints`]. Every edge stays downward and the
//! ordering rule still lives in one place, because the tool's function
//! is the only thing that ever appends.
//!
//! ### Where the program stream's order comes from
//!
//! [`program_diagnostics`] is the caller
//! [`crate::program`]'s module docs describe: the whole-program queries
//! deliberately carry neither the include walk's own diagnostics nor
//! [`include_parse_failures`], so something has to concatenate them.
//! The order is the one the include-aware front end always emitted —
//! see [`program_diagnostics`] for the sequence, and for the one place
//! where a whole-closure query necessarily groups what the per-file walk
//! interleaved.

use std::sync::Arc;

use leek_diagnostics::Diagnostic;
use leek_query::OptLevel;
use leek_span::SourceId;
use leek_syntax::Version;

use crate::include::{include_graph, include_parse_failures, program_classes};
use crate::program::{lower_program, resolve_program, typecheck_program};
use crate::{Db, ProgramClasses, SourceFile, WorkspaceFiles};

/// Every diagnostic one file earns, from its pragmas down to its MIR.
///
/// The concatenation of [`pragma_query`](leek_syntax::pipeline::pragma_query),
/// [`lex_query`](leek_lexer::pipeline::lex_query),
/// [`parse_query`](leek_parser::pipeline::parse_query),
/// [`resolve_query`](leek_resolver::pipeline::resolve_query),
/// [`typecheck_query`](leek_types::pipeline::typecheck_query),
/// [`lower_hir_query`](leek_hir::pipeline::lower_hir_query) and
/// [`lower_mir_query`](leek_mir::pipeline::lower_mir_query), **in that
/// order** — which is the order the passes ran in when a planner
/// sequenced them, and the order a consumer still depends on.
/// `tests/diagnostics.rs` spells the sequence out as a list of codes.
///
/// This answers for **one file**, so it parses under the empty
/// [`ProgramClasses`] set exactly as `leek_lsp::analysis` does: a file
/// analyzed on its own has no cross-file classes. The include-aware
/// answer is [`program_diagnostics`].
///
/// Returned behind an [`Arc`] because the LSP asks for this set several
/// times per keystroke — publish, pull and `codeAction` all want the
/// same list — and a `Vec` return would deep-clone every diagnostic,
/// labels and suggestions included, on each one.
#[salsa::tracked]
pub fn diagnostics_without_lints(db: &dyn Db, file: SourceFile) -> Arc<Vec<Diagnostic>> {
    let mut out = file_diagnostics_upto(db, file, Stage::Hir).as_ref().clone();
    out.extend(leek_mir::pipeline::lower_mir_query(db, file).diagnostics);
    Arc::new(out)
}

/// [`diagnostics_without_lints`], stopped after `stage`.
///
/// The single-file counterpart of [`program_diagnostics_upto`], for a
/// driver whose target reaches only part of the front end: `leekc --emit
/// cst` plans `pragma, lex, parse` and nothing more, so reporting what a
/// resolve or a type-check found would be reporting a pass it never ran.
///
/// Parses under the empty [`ProgramClasses`] set, which is what makes this
/// the *file* answer rather than the program one: `include(…)` is left
/// unresolved, so a class an included file declares is not in scope. That
/// is deliberate for the textual views — they describe the bytes in front
/// of them — and wrong for everything else, which wants
/// [`program_diagnostics_upto`].
///
/// [`Stage::Tokens`] stops before the parse, so at that stage the two
/// queries answer identically: neither reads an include, because resolving
/// one means lexing it.
#[salsa::tracked]
pub fn file_diagnostics_upto(db: &dyn Db, file: SourceFile, stage: Stage) -> Arc<Vec<Diagnostic>> {
    let mut out = Vec::new();
    out.extend(leek_syntax::pipeline::pragma_query(db, file).diagnostics);
    out.extend(leek_lexer::pipeline::lex_query(db, file).diagnostics);
    if stage == Stage::Tokens {
        return Arc::new(out);
    }

    out.extend(leek_parser::pipeline::parse_query(db, file, ProgramClasses::none(db)).diagnostics);
    if stage == Stage::Parsed {
        return Arc::new(out);
    }

    out.extend(leek_resolver::pipeline::resolve_query(db, file).diagnostics);
    if stage == Stage::Resolved {
        return Arc::new(out);
    }

    out.extend(leek_types::pipeline::typecheck_query(db, file).diagnostics);
    if stage == Stage::TypeChecked {
        return Arc::new(out);
    }

    out.extend(leek_hir::pipeline::lower_hir_query(db, file).diagnostics);
    Arc::new(out)
}

/// Every diagnostic an entry file's whole include closure earns.
///
/// The order, spelled out as a list of codes in `tests/diagnostics.rs`:
///
/// 1. the entry's `pragma_query`,
/// 2. the entry's `lex_query`,
/// 3. [`include_graph`]'s own `diagnostics` — unresolved include names
///    and cycles,
/// 4. [`include_parse_failures`] — one `INCLUDE_PARSE_FAILED` per
///    `include("…")` site that reaches a file whose parse failed,
/// 5. per **included** file, in the graph's dependency order, its
///    `lex_query` then its `parse_query`,
/// 6. the entry's own `parse_query`,
/// 7. [`resolve_program`], [`typecheck_program`], [`lower_program`].
///
/// Steps 1–2 and 6–7 are the ones [`crate::program`]'s module docs said
/// a caller has to supply itself: the whole-program queries carry the
/// passes' diagnostics and nothing upstream of them, so a stream
/// without 1, 2, 5 and 6 would silently drop every lex and syntax error
/// in the program. That is why this query is wider than "graph +
/// failures + program".
///
/// ### The one divergence from the per-file walk, and why it is not one
///
/// [`resolve_include_closure`](leek_resolver::closure::resolve_include_closure)
/// *interleaves* 4 and 5: for each included file it emits that file's
/// `INCLUDE_PARSE_FAILED`s and then that file's own lex/parse
/// diagnostics. Here they are two blocks, because
/// [`include_parse_failures`] is one memoized query over the whole
/// closure and splitting it per file would throw that memo away.
///
/// The two orders are indistinguishable to every consumer, because
/// consumers filter by [`for_source`] first: an
/// `INCLUDE_PARSE_FAILED` for file *x* is anchored at an `include("…")`
/// site inside a file that includes *x*, and an includer always comes
/// **after** *x* in dependency order. So within any one source id, both
/// orders yield "the failures for the files this one includes, then
/// this one's own lex and parse diagnostics".
/// `the_include_failures_land_on_the_includer` pins exactly that.
///
/// `lower_program` is asked at [`OptLevel::O0`]: an optimized tree is a
/// different tree, not a different set of complaints, and `O0` is what
/// the LSP's parameters ask for.
#[salsa::tracked]
pub fn program_diagnostics(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
) -> Arc<Vec<Diagnostic>> {
    program_diagnostics_upto(db, files, entry, entry_version, Stage::Hir)
}

/// How far down the frontend a diagnostic stream reaches.
///
/// A `Run`'s diagnostics are whatever the steps it *ran* reported, so the
/// stream a `leek_session::Target` produces grows with the target: a
/// `Target::Parsed` run says nothing about types, and a `Target::Hir` one
/// says nothing about lints. [`program_diagnostics`] has no such notion —
/// it reports the whole frontend — which makes it a drop-in replacement
/// for a `Run`'s stream at exactly one target and a behaviour change at
/// every other.
///
/// This is the key that closes that gap. The variants are the stages that
/// actually contribute diagnostics, in the order they contribute them;
/// `leek_session` maps its `Target` onto one.
///
/// Lints are deliberately absent, and so is MIR. Lints live in
/// `leek_lint::program_diagnostics_with_lints`, because `crates/db` may
/// not depend on `crates/tools`. MIR is absent because there is no
/// *program-level* MIR lowering to ask — `lower_mir_query` answers for one
/// file — so a `Target::Mir` stream cannot be assembled here yet.
#[derive(salsa::Update, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// The entry's pragmas and tokens. No include is read: resolving one
    /// means lexing it, which is already past this stage.
    Tokens,
    /// Adds the include graph, the closure's lexes and parses, and the
    /// entry's own parse.
    Parsed,
    /// Adds whole-program name resolution.
    Resolved,
    /// Adds whole-program type checking.
    TypeChecked,
    /// Adds whole-program HIR lowering. The widest stream assembled here,
    /// and what [`program_diagnostics`] reports.
    Hir,
}

/// [`program_diagnostics`], stopped after `stage`.
///
/// The order is [`program_diagnostics`]'s, and each stage is a prefix of
/// the next, so a consumer that widens its target only ever *gains*
/// diagnostics — which is the property that makes swapping a `Run`'s
/// stream for this one safe at a given target.
#[salsa::tracked]
pub fn program_diagnostics_upto(
    db: &dyn Db,
    files: WorkspaceFiles,
    entry: SourceFile,
    entry_version: Version,
    stage: Stage,
) -> Arc<Vec<Diagnostic>> {
    let mut out = Vec::new();
    out.extend(leek_syntax::pipeline::pragma_query(db, entry).diagnostics);
    out.extend(leek_lexer::pipeline::lex_query(db, entry).diagnostics);
    if stage == Stage::Tokens {
        return Arc::new(out);
    }

    let graph = include_graph(db, files, entry, entry_version);
    // The program's own parse key, so every parse read below is the memo
    // the whole-program passes filled rather than a second parse under
    // an empty class set.
    let classes = program_classes(db, files, entry, entry_version);
    out.extend(graph.diagnostics.iter().cloned());
    out.extend(include_parse_failures(db, files, entry, entry_version));
    for file in graph.includes() {
        out.extend(leek_lexer::pipeline::lex_query(db, file.file).diagnostics);
        out.extend(leek_parser::pipeline::parse_query(db, file.file, classes).diagnostics);
    }
    out.extend(leek_parser::pipeline::parse_query(db, entry, classes).diagnostics);
    if stage == Stage::Parsed {
        return Arc::new(out);
    }

    out.extend(resolve_program(db, files, entry, entry_version).diagnostics);
    if stage == Stage::Resolved {
        return Arc::new(out);
    }

    out.extend(typecheck_program(db, files, entry, entry_version).diagnostics);
    if stage == Stage::TypeChecked {
        return Arc::new(out);
    }

    out.extend(lower_program(db, files, entry, entry_version, OptLevel::O0).diagnostics);
    Arc::new(out)
}

/// The slice of a program's diagnostics that belongs to one file.
///
/// A whole-program stream reports the whole program: a type error
/// inside an included file is raised against *that* file's
/// [`SourceId`], and an editor showing it on the entry document would
/// point at a line number from the wrong file. Every consumer of
/// [`program_diagnostics`] therefore wants one file's slice, which is
/// the filter `leek_lsp::diagnostics::file_diagnostics` already spells
/// out by hand over a `Run`.
///
/// Order-preserving: the surviving diagnostics keep the relative order
/// [`program_diagnostics`] put them in, which is the property
/// `client_shows` matching depends on.
#[must_use]
pub fn for_source(diagnostics: &[Diagnostic], source: SourceId) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .filter(|d| d.span.source == source)
        .cloned()
        .collect()
}
