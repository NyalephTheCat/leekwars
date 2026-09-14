//! The include-aware front-end, driven end to end.
//!
//! `leek_recipes::pipeline_with_includes` is the path `leekc`, `miku`,
//! the LSP and the DAP take for a multi-file project, and it is the only
//! one that plans the `resolve` and `type-check` steps with an
//! [`IncludeGraphArtifact`] in the context. Nothing executed it before
//! (#194): `multi_file_pipeline.rs` builds the HIR-only recipe, whose
//! pushed `LowerHir` step never expands `HirArtifact::Requires`, and the
//! recipe-shape tests in `leek-pipeline` stand a no-op `Tap` in for the
//! includes step and assert step *names* only.

use std::path::PathBuf;
use std::sync::Arc;

use leek_diagnostics::{Diagnostic, codes};
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target, pipeline_with_includes, plan_with_includes};
use leek_resolver::folder::MemFolder;
use leek_resolver::interner::PathInterner;
use leek_resolver::pipeline::{IncludeGraphArtifact, ResolveIncludes};
use leek_span::SourceId;
use leek_types::pipeline::TypeCheckArtifact;

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
    let mut folder = MemFolder::new();
    for (p, t) in files {
        folder.insert(*p, *t);
    }
    let entry_text = files
        .iter()
        .find(|(p, _)| *p == entry_path)
        .map(|(_, t)| (*t).to_string())
        .expect("entry exists in fixture");

    let input = Input {
        source: SourceId::new(1).unwrap(),
        text: entry_text.into(),
        version_byte: 4,
        strict: true,
        flags: leek_pipeline::FeatureFlags::none(),
    };
    // The walker interns the entry first, so the counter starts at the
    // entry's own id — exactly what `leek_driver::includes_step` does.
    // Seeding it past the entry would hand the entry a second `SourceId`
    // and make any span-source assertion here meaningless.
    let includes = ResolveIncludes::new(
        Arc::new(folder),
        PathBuf::from(entry_path),
        Arc::new(PathInterner::starting_at(1)),
    );
    let params = RecipeParams::permissive();
    let pipeline =
        pipeline_with_includes(Target::TypeChecked, Box::new(includes), &params).expect("recipe");
    let run = pipeline.run(input);

    assert!(
        run.get::<TypeCheckArtifact>().is_some(),
        "the type-check step ran: {:?}",
        run.errors()
    );
    let include_sources = run
        .get::<IncludeGraphArtifact>()
        .map(|g| {
            g.includes
                .iter()
                .map(|f| (f.path.clone(), f.source))
                .collect()
        })
        .unwrap_or_default();
    Run {
        diagnostics: run.diagnostics().to_vec(),
        include_sources,
    }
}

#[test]
fn the_typechecked_target_plans_both_resolve_and_type_check_with_includes() {
    // The guarantee the rest of this file relies on: a no-op `Tap`
    // recipe-shape test can't tell whether these steps really execute,
    // but if they were not even planned nothing below would run.
    let includes = ResolveIncludes::new(
        Arc::new(MemFolder::new()),
        PathBuf::from("/main.leek"),
        Arc::new(PathInterner::starting_at(2)),
    );
    let names = plan_with_includes(
        Target::TypeChecked,
        Box::new(includes),
        &RecipeParams::permissive(),
    )
    .expect("plan")
    .step_names();
    assert!(names.contains(&"resolve"), "{names:?}");
    assert!(names.contains(&"type-check"), "{names:?}");
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
