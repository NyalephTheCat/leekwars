//! The include-aware half of the diagnostic-stream ordering rule.
//!
//! `diagnostics_query.rs` pins the one-file stream. This one pins the
//! program stream — the closure's frontend complaints, then its lints —
//! and, more importantly, pins *which* lints a closure produces.
//!
//! That second claim is the reason this file exists. The pipeline's
//! `Lint` step reads one `HirArtifact`, which on the include-aware path
//! is the whole closure's **merged** HIR, and one `GreenTreeArtifact`,
//! which is the **entry's** tree alone. So a program is linted once, not
//! once per file, and `@allow` is read from one tree. Anything that
//! moves the LSP's diagnostics off the pipeline has to keep both, and a
//! refactor that quietly linted per file would look like a cleanup while
//! changing what users see.

use std::collections::BTreeMap;

use leek_db::{LeekDb, SourceFile, WorkspaceFiles};
use leek_diagnostics::Diagnostic;
use leek_lint::LintGroups;
use leek_lint::query::{program_diagnostics_with_lints, program_lint_query};
use leek_span::SourceId;
use leek_syntax::Version;

const ROOT: &str = "/program-lint-tests";

struct Fixture {
    db: LeekDb,
    files: WorkspaceFiles,
    inputs: BTreeMap<String, SourceFile>,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let mut db = LeekDb::default();
        let workspace = WorkspaceFiles::empty(&db);
        let mut inputs = BTreeMap::new();
        let mut map = BTreeMap::new();
        for (index, (name, text)) in files.iter().enumerate() {
            let path = format!("{ROOT}/{name}");
            let file = SourceFile::new(
                &db,
                path.clone(),
                u32::try_from(index + 1).expect("fixture is small"),
                (*text).into(),
                u8::from(Version::V4),
                false,
                false,
                0,
            );
            inputs.insert(path.clone(), file);
            map.insert(path, file);
        }
        workspace.set_all(&mut db, map);
        Self {
            db,
            files: workspace,
            inputs,
        }
    }

    fn file(&self, name: &str) -> SourceFile {
        self.inputs[&format!("{ROOT}/{name}")]
    }

    fn source(&self, name: &str) -> SourceId {
        self.file(name).source(&self.db)
    }

    fn lints(&self, entry: &str) -> Vec<Diagnostic> {
        program_lint_query(
            &self.db,
            self.files,
            self.file(entry),
            Version::V4,
            LintGroups::default(),
        )
        .as_ref()
        .clone()
    }

    fn stream(&self, entry: &str) -> Vec<Diagnostic> {
        program_diagnostics_with_lints(
            &self.db,
            self.files,
            self.file(entry),
            Version::V4,
            LintGroups::default(),
        )
        .as_ref()
        .clone()
    }
}

fn codes(diagnostics: &[Diagnostic]) -> Vec<&'static str> {
    diagnostics.iter().map(|d| d.code.id()).collect()
}

/// `1 / 0` is the lint (`L0016`) every fixture here leans on: no
/// compiler stage objects to it, so a finding in the stream can only
/// have come from the linter.
const DIVIDE_BY_ZERO: &str = "var z = 1 / 0;\n";

/// The composition rule: the program's frontend stream, unchanged and in
/// order, then the lints.
#[test]
fn lints_are_appended_to_the_programs_frontend_stream_unchanged() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"util\")\nreturn helper();\n"),
        (
            "util.leek",
            &format!("function helper() {{\n\t{DIVIDE_BY_ZERO}\treturn z;\n}}\n"),
        ),
    ]);

    let frontend = leek_db::queries::program_diagnostics(
        &fixture.db,
        fixture.files,
        fixture.file("main.leek"),
        Version::V4,
    );
    let lints = fixture.lints("main.leek");
    let whole = fixture.stream("main.leek");

    assert_eq!(
        whole.len(),
        frontend.len() + lints.len(),
        "the stream is exactly the two halves"
    );
    assert_eq!(
        codes(&whole[..frontend.len()]),
        codes(&frontend),
        "the frontend's half is unchanged and first"
    );
    assert_eq!(
        codes(&whole[frontend.len()..]),
        codes(&lints),
        "the lints come last"
    );
}

/// A lint inside an included file is reported, once, against *that*
/// file — the closure is linted through its merged HIR, so a construct
/// the entry never mentions is still seen.
#[test]
fn a_lint_in_an_included_file_is_reported_once_against_that_file() {
    let fixture = Fixture::new(&[
        ("main.leek", "include(\"util\")\nreturn helper();\n"),
        (
            "util.leek",
            &format!("function helper() {{\n\t{DIVIDE_BY_ZERO}\treturn z;\n}}\n"),
        ),
    ]);

    let lints = fixture.lints("main.leek");
    let divides: Vec<&Diagnostic> = lints.iter().filter(|d| d.code.id() == "L0016").collect();

    assert_eq!(
        divides.len(),
        1,
        "linted once, not once per file: {lints:?}"
    );
    assert_eq!(
        divides[0].span.source,
        fixture.source("util.leek"),
        "raised against the file that holds the construct"
    );
}

/// Two entries that include the same leaf each report it. The finding
/// belongs to the *program*, so it is not deduplicated across programs —
/// each entry's stream is answered on its own.
#[test]
fn two_entries_over_one_leaf_each_report_it() {
    let fixture = Fixture::new(&[
        ("one.leek", "include(\"util\")\nreturn helper();\n"),
        ("two.leek", "include(\"util\")\nreturn helper();\n"),
        (
            "util.leek",
            &format!("function helper() {{\n\t{DIVIDE_BY_ZERO}\treturn z;\n}}\n"),
        ),
    ]);

    for entry in ["one.leek", "two.leek"] {
        assert!(
            codes(&fixture.lints(entry)).contains(&"L0016"),
            "{entry} reports the leaf's lint"
        );
    }
}

/// `@allow` is read from the entry's tree, because that is the only tree
/// the `Lint` step is handed. An annotation sitting in an *included*
/// file therefore does not suppress — pinned here not because it is
/// desirable but because it is what the pipeline does, and a
/// replacement that silently started honouring it would be changing
/// behaviour under cover of a refactor.
#[test]
fn an_allow_inside_an_included_file_does_not_suppress() {
    let annotated = Fixture::new(&[
        ("main.leek", "include(\"util\")\nreturn helper();\n"),
        (
            "util.leek",
            &format!(
                "function helper() {{\n\t// @allow(L0016)\n\t{DIVIDE_BY_ZERO}\treturn z;\n}}\n"
            ),
        ),
    ]);

    assert!(
        codes(&annotated.lints("main.leek")).contains(&"L0016"),
        "an include's own @allow does not reach the entry's tree"
    );
}

/// The same annotation in the entry does suppress, which is what makes
/// the previous test a statement about *which tree* rather than about
/// `@allow` being broken.
#[test]
fn an_allow_in_the_entry_suppresses_the_entrys_own_finding() {
    let bare = Fixture::new(&[("main.leek", DIVIDE_BY_ZERO)]);
    assert!(
        codes(&bare.lints("main.leek")).contains(&"L0016"),
        "baseline: the finding is there without an annotation"
    );

    let annotated = Fixture::new(&[("main.leek", &format!("// @allow(L0016)\n{DIVIDE_BY_ZERO}"))]);
    assert!(
        !codes(&annotated.lints("main.leek")).contains(&"L0016"),
        "the entry's own annotation suppresses"
    );
}
