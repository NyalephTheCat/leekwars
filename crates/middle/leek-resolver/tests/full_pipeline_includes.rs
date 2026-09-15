//! The include-aware front end, driven end to end.
//!
//! `program_diagnostics_upto(..., Stage::TypeChecked)` is what every
//! front-end asks for a multi-file project, and it is the only stream
//! that runs `resolve_program` and `typecheck_program` over the whole
//! include closure. Nothing executed that path before (#194):
//! `multi_file_pipeline.rs` only lowers, and the per-query tests in the
//! pass crates each answer for one file.
//!
//! The stage is the guarantee an earlier version of this file spent a
//! test on: `Stage::TypeChecked` *is* "resolve and type-check ran", so a
//! diagnostic below arriving from an included file cannot be produced any
//! other way.

use std::path::PathBuf;

use leek_db::queries::{Stage, include_graph, program_diagnostics_upto};
use leek_diagnostics::{Diagnostic, codes};
use leek_span::SourceId;

struct Run {
    diagnostics: Vec<Diagnostic>,
    /// Included files in the graph, by source id, so a test can say
    /// which file a diagnostic came from.
    include_sources: Vec<(PathBuf, SourceId)>,
}

impl Run {
    fn source_of(&self, path: &str) -> SourceId {
        match self
            .include_sources
            .iter()
            .find(|(p, _)| p == std::path::Path::new(path))
        {
            Some((_, source)) => *source,
            None => panic!(
                "{path} is an included file; have {:?}",
                self.include_sources
            ),
        }
    }

    fn with_code(&self, code: leek_diagnostics::Code) -> Vec<&Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.code.name() == code.name())
            .collect()
    }
}

fn run_to_typecheck(entry_path: &str, files: &[(&str, &str)]) -> Run {
    let mut db = leek_db::LeekDb::default();
    // The entry keeps `SourceId(1)` and the includes take 2, 3, … — the
    // numbering a `Session` hands out. Giving the entry a second id would
    // make every span-source assertion here meaningless.
    let (set, entry) = leek_db::testing::workspace(
        &mut db,
        entry_path,
        files,
        (4, true),
        leek_span::FeatureFlags::none().to_bits(),
    );
    let version = leek_syntax::Version::V4;
    let diagnostics = program_diagnostics_upto(&db, set, entry, version, Stage::TypeChecked)
        .as_ref()
        .clone();
    let include_sources = include_graph(&db, set, entry, version)
        .includes()
        .map(|f| (f.path.clone(), f.source))
        .collect();
    Run {
        diagnostics,
        include_sources,
    }
}

#[test]
fn a_resolver_diagnostic_from_an_included_file_reaches_the_run() {
    let run = run_to_typecheck(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\n"),
            ("/a.leek", "var d = 1\nvar d = 2\n"),
        ],
    );
    let diags = run.with_code(codes::REDECLARED_SYMBOL);
    assert_eq!(diags.len(), 1, "{:?}", run.diagnostics);
    assert_eq!(diags[0].span.source, run.source_of("/a.leek"));
}

#[test]
fn a_type_diagnostic_from_an_included_file_reaches_the_run() {
    let run = run_to_typecheck(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\n"),
            ("/a.leek", "function f() -> integer { return \"s\" }\n"),
        ],
    );
    let diags = run.with_code(codes::INCOMPATIBLE_TYPE);
    assert_eq!(diags.len(), 1, "{:?}", run.diagnostics);
    assert_eq!(diags[0].span.source, run.source_of("/a.leek"));
}

#[test]
fn the_whole_front_end_agrees_on_include_site_order() {
    // #118 end to end: `cfg` is declared above the include site, so
    // /a.leek's read of it resolves — no UNKNOWN_VARIABLE — and the
    // statements after the site are not dead code.
    let run = run_to_typecheck(
        "/main.leek",
        &[
            ("/main.leek", "var cfg = 3\ninclude(\"a\")\nvar after = 1\n"),
            ("/a.leek", "var got = cfg\n"),
        ],
    );
    assert!(
        run.with_code(codes::UNKNOWN_VARIABLE).is_empty(),
        "{:?}",
        run.diagnostics
    );
    assert!(
        run.with_code(codes::CANT_ADD_INSTRUCTION_AFTER_BREAK)
            .is_empty(),
        "{:?}",
        run.diagnostics
    );
}

#[test]
fn a_terminator_in_an_included_file_reaches_the_entrys_trailing_statement() {
    let run = run_to_typecheck(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\nvar after = 1\n"),
            ("/a.leek", "return 1\n"),
        ],
    );
    let diags = run.with_code(codes::CANT_ADD_INSTRUCTION_AFTER_BREAK);
    assert_eq!(diags.len(), 1, "{:?}", run.diagnostics);
    assert_eq!(
        diags[0].span.source,
        SourceId::new(1).unwrap(),
        "the dead statement is the entry's"
    );
}

#[test]
fn a_broken_include_is_reported_at_the_include_site() {
    // #290: the parse of an included file always yields a `SourceFile`
    // root (recovery builds `ErrorNode`s inside it), so the old
    // `ast.is_none()` test could never fire and E0274 had no emitter.
    // The trigger is now "the included file's own parse produced an
    // error", and the label lands on the entry's `include(...)`.
    const ENTRY: &str = "include(\"a\")\nvar x = 1\n";
    let run = run_to_typecheck(
        "/main.leek",
        &[("/main.leek", ENTRY), ("/a.leek", "function f( {\n")],
    );
    let diags = run.with_code(codes::INCLUDE_PARSE_FAILED);
    assert_eq!(diags.len(), 1, "{:?}", run.diagnostics);
    assert_eq!(
        diags[0].span.source,
        SourceId::new(1).unwrap(),
        "the label is at the include site in the entry, not inside /a.leek"
    );
    let site = &ENTRY[diags[0].span.start as usize..diags[0].span.end as usize];
    assert_eq!(
        site, "\"a\"",
        "the span is the include argument in the entry"
    );
}

#[test]
fn a_well_formed_include_reports_no_parse_failure() {
    // Errors only — a file that parses cleanly must not mark its include
    // site, even when a later pass complains about it.
    let run = run_to_typecheck(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\n"),
            ("/a.leek", "var d = 1\nvar d = 2\n"),
        ],
    );
    assert!(
        run.with_code(codes::INCLUDE_PARSE_FAILED).is_empty(),
        "{:?}",
        run.diagnostics
    );
}
