//! The order of the diagnostic stream.
//!
//! `diagnostics_without_lints` and `program_diagnostics` are the only
//! two places in the workspace that *state* the order the compiler's
//! complaints come out in. Everywhere else it was emergent: the recipe
//! planner sequences a step after the step whose artifact it requires,
//! and a `Run`'s diagnostics come out in the order the steps emitted
//! them. Emergent is fine until something depends on it —
//! `leek_lsp::handlers::code_action` matches diagnostics a client hands
//! back against the ones the server published, and that match degrades
//! when the order moves under it.
//!
//! So these tests pin the order twice over: against an explicit list of
//! codes, and against a pipeline the *recipe planner itself* laid out.
//! The second half is what makes this more than a restatement of the
//! query body — if the planner ever sequences `TypeCheck` before
//! `Resolve`, the query and the pipeline disagree and this file fails.
//!
//! ### The lint stage is not here, and cannot be
//!
//! The brief for this slice asked for one fixture carrying a lex error,
//! a parse error, a resolve error, a type error **and a lint finding**.
//! The first four are below; the fifth is impossible from this crate.
//! `leek-lint` is `crates/tools` (layer rank 6) and `leek-db` is
//! `crates/db` (rank 3), and `cargo xtask check-layers` allows a
//! dev-dependency to reach at most one rank up — so `leek-db` may not
//! even *test* against the linter. The lint half of the ordering rule
//! is pinned where the concatenation lives, in
//! `crates/tools/leek-lint/tests/diagnostics_query.rs`.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_db::queries::{diagnostics_without_lints, for_source, program_diagnostics};
use leek_diagnostics::Diagnostic;
use leek_pipeline::{Pipeline, RecipeParams, plan_for};
use leek_resolver::folder::MemFolder;
use leek_resolver::interner::PathInterner;
use leek_syntax::Version;
use support::{Fixture, vpath};

/// One file that breaks at every stage this crate can reach:
///
/// * `// @nope:1` — an unknown pragma (`W0010`), from `pragma_query`;
/// * `£` — not a character the lexer knows (`E0003`), from `lex_query`;
/// * `var = ;` — a declaration with no name (`E0100`), from
///   `parse_query`, which also reports the error token the lexer left
///   behind;
/// * `var a` twice — a redeclaration (`E0202`), from `resolve_query`;
/// * `[1: 2, 1: 3]` — a duplicated map key (`E0253`), from
///   `typecheck_query`.
///
/// The two broken lines are **last** on purpose. Parser error recovery
/// swallows the rest of the file after a stray token, and a fixture
/// that puts them first reports only the lex and parse errors —
/// looking like a five-stage test while exercising two.
const BROKEN: &str = concat!(
    "// @nope:1\n",
    "var a = 1;\n",
    "var a = 2;\n",
    "var m = [1: 2, 1: 3];\n",
    "var bad = £;\n",
    "var = ;\n",
);

fn codes(diagnostics: &[Diagnostic]) -> Vec<&'static str> {
    diagnostics.iter().map(|d| d.code.id()).collect()
}

// ---- One file ----

/// The order, spelled out. Each code names the stage that raised it, and
/// the stages appear in the sequence the query documents: pragma, lex,
/// parse, resolve, typecheck, HIR, MIR.
#[test]
fn one_files_diagnostics_come_out_stage_by_stage() {
    let fixture = Fixture::new(&[("main.leek", BROKEN)]);
    let diagnostics = diagnostics_without_lints(&fixture.db, fixture.file("main.leek"));
    // Grouped by the stage that raised them, in the order the query
    // documents. One flat list would say the same thing, but rustfmt
    // packs it onto shared lines and the stage each code belongs to
    // stops being legible.
    let expected: Vec<&str> = [
        &["W0010"][..],                   // pragma_query
        &["E0003"][..],                   // lex_query
        &["E0100", "E0100", "E0100"][..], // parse_query
        &["E0202"][..],                   // resolve_query
        &["E0253"][..],                   // typecheck_query
        &[][..],                          // lower_hir_query — nothing to say here
        &[][..],                          // lower_mir_query — nor here
    ]
    .concat();
    assert_eq!(codes(&diagnostics), expected, "{diagnostics:?}");
}

/// The claim that makes the list above a *pin* rather than a copy of the
/// query body: the same stream, in the same order, as the pipeline the
/// recipe planner builds for the deepest single-file target.
///
/// `plan_for` is the real planner — the one `leek_session::plan` calls —
/// so this compares against the artifact-dependency graph itself and not
/// against a hand-written step list. Planned `permissive`, because the
/// default `stop_on_diagnostics` halts the run at the first error and a
/// truncated stream would make the comparison vacuous.
#[test]
fn diagnostics_without_lints_is_the_recipes_order() {
    let fixture = Fixture::new(&[("main.leek", BROKEN)]);
    let file = fixture.file("main.leek");

    let plan = plan_for::<leek_mir::pipeline::MirArtifact>(&RecipeParams::permissive())
        .expect("the MIR recipe plans");
    let run = plan.build().run_memoized(&fixture.db, file);

    assert_eq!(
        codes(run.diagnostics()),
        codes(&diagnostics_without_lints(&fixture.db, file)),
        "the query and the recipe-planned pipeline disagree about the order"
    );
    assert_eq!(
        run.diagnostics(),
        diagnostics_without_lints(&fixture.db, file).as_slice(),
        "…and about the diagnostics themselves"
    );
}

/// A clean file earns nothing, and asking twice is one answer.
#[test]
fn a_clean_file_has_an_empty_stream() {
    let fixture = Fixture::new(&[("main.leek", "var x = 1;\nreturn x;\n")]);
    let file = fixture.file("main.leek");
    assert!(diagnostics_without_lints(&fixture.db, file).is_empty());
    assert!(Arc::ptr_eq(
        &diagnostics_without_lints(&fixture.db, file),
        &diagnostics_without_lints(&fixture.db, file),
    ));
}

// ---- A whole program ----

/// A three-file program in which every file has something wrong, plus a
/// dangling `include("ghost")` so the graph itself complains.
fn broken_program() -> Fixture {
    Fixture::new(&[
        (
            "main.leek",
            "// @nope:1\ninclude(\"a\")\ninclude(\"ghost\")\nvar m = [1: 2, 1: 3];\nreturn m;\n",
        ),
        ("a.leek", "include(\"b\")\nvar bad = £;\nvar = ;\n"),
        ("b.leek", "var b = 1;\nvar b = 2;\n"),
    ])
}

/// The include-aware pipeline, as `leek_session::plan_with_includes`
/// shapes it: lex the entry, walk its includes, parse, resolve, type
/// check, lower. Its diagnostic stream is what `program_diagnostics`
/// has to reproduce.
fn include_aware_run(fixture: &Fixture, entry: &str, names: &[&str]) -> Vec<Diagnostic> {
    let mut folder = MemFolder::new();
    let interner = PathInterner::new();
    for name in names {
        folder.insert(PathBuf::from(vpath(name)), fixture.text(name));
        interner.assign(
            Path::new(&vpath(name)),
            fixture.file(name).source(&fixture.db),
        );
    }
    let pipeline = Pipeline::new()
        .with(leek_syntax::pipeline::Pragma)
        .with(leek_lexer::pipeline::Lex)
        .with(leek_resolver::pipeline::ResolveIncludes::new(
            Arc::new(folder),
            PathBuf::from(vpath(entry)),
            Arc::new(interner),
        ))
        .with(leek_parser::pipeline::Parse)
        .with(leek_resolver::pipeline::Resolve)
        .with(leek_types::pipeline::TypeCheck)
        .with(leek_hir::pipeline::LowerHir::new(
            leek_pipeline::OptLevel::O0,
        ));
    pipeline
        .run_memoized(&fixture.db, fixture.file(entry))
        .diagnostics()
        .to_vec()
}

/// The order `program_diagnostics` documents, per file.
///
/// Compared *per source id* rather than end to end, which is the
/// comparison every consumer actually makes: a diagnostic raised inside
/// an include belongs to that include's document, so the LSP filters
/// before it publishes. The query and the pipeline concatenate the
/// include-parse failures differently — one block versus interleaved per
/// file — and this is the assertion that says the difference cannot be
/// observed.
#[test]
fn program_diagnostics_matches_the_include_aware_pipeline_per_source() {
    let fixture = broken_program();
    let entry = fixture.file("main.leek");
    let names = ["main.leek", "a.leek", "b.leek"];

    let from_pipeline = include_aware_run(&fixture, "main.leek", &names);
    let from_query = program_diagnostics(&fixture.db, fixture.files, entry, Version::V4);

    for name in names {
        let source = fixture.file(name).source(&fixture.db);
        assert_eq!(
            codes(&for_source(&from_query, source)),
            codes(&for_source(&from_pipeline, source)),
            "{name}: query {:?} vs pipeline {:?}",
            for_source(&from_query, source),
            for_source(&from_pipeline, source),
        );
    }
}

/// The include walk's own complaints come first, then the per-site
/// parse failures, then the program's own passes — the order
/// `crates/db/leek-db/src/program.rs` says a caller has to assemble, now
/// assembled in exactly one place.
#[test]
fn the_program_stream_opens_with_the_graphs_own_diagnostics() {
    let fixture = broken_program();
    let entry = fixture.file("main.leek");
    let diagnostics = program_diagnostics(&fixture.db, fixture.files, entry, Version::V4);
    let all = codes(&diagnostics);

    assert_eq!(all[0], "W0010", "the entry's pragma, first: {all:?}");
    let unresolved = all
        .iter()
        .position(|c| *c == "E0272")
        .expect("`include(\"ghost\")` resolves to nothing: {all:?}");
    let failed = all
        .iter()
        .position(|c| *c == "E0274")
        .expect("`a.leek` does not parse, so its include site is flagged");
    let typed = all
        .iter()
        .position(|c| *c == "E0253")
        .expect("the entry's duplicated map key is a type error");
    assert!(
        unresolved < failed && failed < typed,
        "graph, then include-parse failures, then the passes: {all:?}"
    );
}

/// Two broken leaves, so the two orders genuinely differ end to end: the
/// query emits both `E0274`s together, the pipeline emits each one just
/// before its own file's parse errors. Per source they are the same
/// list, which is the whole justification for keeping
/// `include_parse_failures` a single query.
#[test]
fn interleaving_the_include_failures_is_invisible_per_source() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"a\")\ninclude(\"b\")\nreturn 1;\n"),
        ("a.leek", "var = ;\n"),
        ("b.leek", "var = ;\n"),
    ]);
    let names = ["main.leek", "a.leek", "b.leek"];
    let entry = fixture.file("main.leek");

    let from_pipeline = include_aware_run(&fixture, "main.leek", &names);
    let from_query = program_diagnostics(&fixture.db, fixture.files, entry, Version::V4);

    // Both leaves failed, and both failures are reported at their
    // `include("…")` sites in the entry.
    let entry_source = entry.source(&fixture.db);
    assert_eq!(
        codes(&for_source(&from_query, entry_source))
            .iter()
            .filter(|c| **c == "E0274")
            .count(),
        2,
        "{from_query:?}"
    );
    for name in names {
        let source = fixture.file(name).source(&fixture.db);
        assert_eq!(
            codes(&for_source(&from_query, source)),
            codes(&for_source(&from_pipeline, source)),
            "{name}"
        );
    }
}

// ---- What actually re-runs ----

/// The claim the promoted `leek_db::testing::EventDb` exists to make
/// falsifiable, in its simplest form: asking the same question twice
/// executes nothing the second time.
///
/// `diagnostics_without_lints` is the query the LSP will ask several
/// times per keystroke — publish, pull, and `codeAction` all want one
/// file's set — so "the second ask is free" is the whole reason it is a
/// query rather than a helper that concatenates seven results at each
/// call site.
#[test]
fn asking_twice_executes_nothing_the_second_time() {
    let fixture = Fixture::new(&[("main.leek", BROKEN)]);
    let file = fixture.file("main.leek");

    let _ = diagnostics_without_lints(&fixture.db, file);
    assert!(
        support::ran(&fixture.db.drain(), "diagnostics_without_lints") > 0,
        "the first ask has to execute, or the second proves nothing"
    );

    let _ = diagnostics_without_lints(&fixture.db, file);
    let events = fixture.db.drain();
    assert!(events.is_empty(), "a repeat ask ran: {events:?}");
}

/// The incremental claim for the program stream: editing one leaf
/// re-lexes and re-parses that leaf and leaves its siblings' parses
/// alone, even though the stream that reports them is one value.
#[test]
fn editing_one_leaf_reparses_only_that_leaf() {
    let mut fixture = Fixture::new(&[
        ("main.leek", "include(\"a\")\ninclude(\"b\")\nreturn 1;\n"),
        ("a.leek", "var a = 1;\n"),
        ("b.leek", "var b = 1;\n"),
    ]);
    let entry = fixture.file("main.leek");
    let _ = program_diagnostics(&fixture.db, fixture.files, entry, Version::V4);
    let _ = fixture.db.drain();

    fixture.edit("b.leek", "var b = 2;\n");
    let _ = program_diagnostics(&fixture.db, fixture.files, entry, Version::V4);
    let events = fixture.db.drain();

    assert_eq!(
        support::ran(&events, "parse_query"),
        1,
        "only the edited leaf: {events:?}"
    );
    assert_eq!(
        support::ran(&events, "program_diagnostics"),
        1,
        "the stream itself does re-run — it read the leaf that changed: {events:?}"
    );
}

/// A file that declares a class is parsed **twice** when it is asked
/// both questions — once under the empty class set for the single-file
/// stream, once under its own class set for the program stream — and
/// the two parses produce the same tree.
///
/// Not the same defect as #522 (which is two parses inside *one*
/// include-aware run), but the same shape, and worth recording because
/// it bounds what `program_diagnostics` can share with
/// `diagnostics_without_lints`: nothing, for such a file. The cause is
/// that `program_classes` folds in the **entry's own** `class` names,
/// so a zero-include entry's program key is not `ProgramClasses::none`
/// even though the parser's token pre-scan already finds those names by
/// itself. Cheap to fix (drop the entry's own names from the fold, or
/// route the single-file stream through the program key) and neither is
/// this slice's to make — a caller is routed onto the program queries
/// by R1-24.
#[test]
fn a_file_declaring_a_class_is_parsed_once_per_stream() {
    let fixture = Fixture::new(&[("main.leek", "class c {}\nvar x = new c();\nreturn x;\n")]);
    let file = fixture.file("main.leek");

    let _ = diagnostics_without_lints(&fixture.db, file);
    let _ = fixture.db.drain();

    let _ = program_diagnostics(&fixture.db, fixture.files, file, Version::V4);
    let events = fixture.db.drain();
    assert_eq!(
        support::ran(&events, "parse_query"),
        1,
        "the program stream parsed the file again under its own class set: {events:?}"
    );

    let bare = leek_db::queries::parse_query(
        &fixture.db,
        file,
        leek_db::ProgramClasses::none(&fixture.db),
    );
    let program = leek_db::queries::parse_query(
        &fixture.db,
        file,
        leek_db::queries::program_classes(&fixture.db, fixture.files, file, Version::V4),
    );
    assert_eq!(
        bare.green, program.green,
        "…for the same tree: the parser's own pre-scan already knew about `class c`"
    );
}

/// `for_source` keeps the relative order of what survives — the
/// property `client_shows` matching in the LSP depends on.
#[test]
fn for_source_is_a_filter_not_a_sort() {
    let fixture = broken_program();
    let entry = fixture.file("main.leek");
    let all = program_diagnostics(&fixture.db, fixture.files, entry, Version::V4);
    let mine = for_source(&all, entry.source(&fixture.db));

    assert!(!mine.is_empty());
    let mut expected = all
        .iter()
        .filter(|d| d.span.source == entry.source(&fixture.db));
    for got in &mine {
        assert_eq!(Some(got), expected.next());
    }
}
