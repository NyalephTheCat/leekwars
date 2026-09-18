//! The lint half of the diagnostic-stream ordering rule.
//!
//! `leek-db` produces the compiler's own complaints —
//! `diagnostics_without_lints`, pinned stage by stage in
//! `crates/db/leek-db/tests/diagnostics.rs` — and stops there, because
//! it may not depend on this crate: `leek-db` is `crates/db` (layer
//! rank 3), `leek-lint` is `crates/tools` (rank 6), and
//! `cargo xtask check-layers` rejects db → tools with no allowlist
//! entry for it. The append therefore lives here, in the tool, which
//! depends **down** on `leek-db`.
//!
//! So this file owns the two claims the layer boundary split off:
//! lint findings come **last**, and everything before them is exactly
//! what `leek-db` produced — byte for byte, not merely "the same set".
//! The same boundary is why the brief's "one fixture with all five
//! stages plus a lint" is two fixtures in two crates: `leek-db` cannot
//! dev-depend on the linter to see the fifth.

use leek_db::queries::diagnostics_without_lints;
use leek_db::{LeekDb, SourceFile};
use leek_diagnostics::Diagnostic;
use leek_lint::query::{diagnostics_with_lints, lint_query};
use leek_lint::{LintGroups, LintOptions};

/// Everything the one-file stream can carry: an unknown pragma
/// (`W0010`), a stray character (`E0003`), a nameless declaration
/// (`E0100`), a redeclaration (`E0202`), a duplicated map key
/// (`E0253`) — the same five stages `leek-db`'s own test pins — plus a
/// division by zero, which no compiler stage objects to and the
/// `L0016` lint does.
///
/// The two broken lines come last because parser error recovery
/// swallows what follows a stray token, and a fixture that leads with
/// them exercises two stages while looking like it exercises six.
const BROKEN: &str = concat!(
    "// @nope:1\n",
    "var a = 1;\n",
    "var a = 2;\n",
    "var m = [1: 2, 1: 3];\n",
    "var z = 1 / 0;\n",
    "var bad = £;\n",
    "var = ;\n",
);

fn db_with(src: &str) -> (LeekDb, SourceFile) {
    let db = LeekDb::default();
    let file = SourceFile::new(
        &db,
        "/lint-query-tests/main.leek".to_string(),
        1,
        src.into(),
        4,
        false,
        false,
        0,
    );
    (db, file)
}

fn codes(diagnostics: &[Diagnostic]) -> Vec<&'static str> {
    diagnostics.iter().map(|d| d.code.id()).collect()
}

/// The whole rule in one assertion: the frontend's stream, unchanged
/// and in order, then the lints.
#[test]
fn lints_are_appended_to_the_frontends_stream_unchanged() {
    let (db, file) = db_with(BROKEN);
    let groups = LintGroups::default();

    let front = diagnostics_without_lints(&db, file);
    let lints = lint_query(&db, file, groups);
    let full = diagnostics_with_lints(&db, file, groups);

    assert_eq!(
        full.as_slice(),
        [front.as_slice(), lints.as_slice()].concat().as_slice(),
        "the composed stream is the concatenation, in that order"
    );
    assert_eq!(
        &full[..front.len()],
        front.as_slice(),
        "the frontend's prefix is untouched"
    );
    assert!(
        full[front.len()..]
            .iter()
            .all(|d| d.code.id().starts_with('L')),
        "…and everything after it is a lint: {:?}",
        codes(&full[front.len()..])
    );
}

/// All six stages in one file, so the ordering claim is about a stream
/// that actually has something from each.
#[test]
fn every_stage_is_represented_and_in_order() {
    let (db, file) = db_with(BROKEN);
    let full = diagnostics_with_lints(&db, file, LintGroups::default());
    let all = codes(&full);

    let at = |code: &str| {
        all.iter()
            .position(|c| *c == code)
            .unwrap_or_else(|| panic!("{code} missing from {all:?}"))
    };
    let pragma = at("W0010");
    let lex = at("E0003");
    let parse = at("E0100");
    let resolve = at("E0202");
    let types = at("E0253");
    let lint = at("L0016");
    assert!(
        pragma < lex && lex < parse && parse < resolve && resolve < types && types < lint,
        "pragma, lex, parse, resolve, typecheck, lint: {all:?}"
    );
}

/// The query has to agree with the [`Lint`](leek_lint::Lint) step it
/// memoizes, or the LSP and `miku lint` would report different things
/// about the same file.
#[test]
fn the_query_agrees_with_the_pipeline_step() {
    let (db, file) = db_with(BROKEN);
    let opts = LintOptions::from_groups(LintGroups::default(), 4);

    let hir = leek_db::queries::lower_hir_query(&db, file);
    let root = leek_syntax::SyntaxNode::new_root(
        leek_db::queries::parse_query(&db, file, leek_db::ProgramClasses::none(&db)).green,
    );
    let direct = leek_lint::lint_file(&hir.hir, Some(&root), file.source(&db), &opts);

    assert_eq!(
        lint_query(&db, file, LintGroups::default()).as_slice(),
        direct
    );
}

/// `@allow` lives in comment trivia the HIR does not carry, which is
/// why the query reads the green tree as well as the HIR. Without that
/// second read the suppression would silently stop working the moment
/// the query replaced the step.
#[test]
fn the_query_honours_allow_annotations() {
    let src = "function f() {\n// @allow(L0001)\nvar dead = 1\nreturn 0\n}\n";
    let (db, file) = db_with(src);
    assert!(
        !codes(&lint_query(&db, file, LintGroups::default())).contains(&"L0001"),
        "the annotation suppresses the unused-variable finding"
    );

    let (db, file) = db_with("function f() {\nvar dead = 1\nreturn 0\n}\n");
    assert!(
        codes(&lint_query(&db, file, LintGroups::default())).contains(&"L0001"),
        "…and without it the finding is there, so the test is not vacuous"
    );
}

/// The opt-in groups are a **key**, not a field: asking for pedantic
/// lints is a different question, with its own memo, and the default
/// answer is still cached beside it.
#[test]
fn the_opt_in_groups_are_part_of_the_key() {
    // `f` takes more parameters than the pedantic `L0025` allows, and
    // nothing else objects to it.
    let src = "function f(a, b, c, d, e, g, h) {\n\
               return a + b + c + d + e + g + h\n\
               }\nreturn f(1, 2, 3, 4, 5, 6, 7)\n";
    let (db, file) = db_with(src);

    let default = lint_query(&db, file, LintGroups::default());
    let pedantic = lint_query(
        &db,
        file,
        LintGroups {
            pedantic: true,
            nursery: false,
        },
    );

    assert!(!codes(&default).contains(&"L0025"));
    assert!(
        codes(&pedantic).contains(&"L0025"),
        "{:?}",
        codes(&pedantic)
    );
}

/// The version the lints run at comes off the file input, so it cannot
/// disagree with the version the HIR was lowered at. That is the whole
/// reason the key is [`LintGroups`] and not a `LintOptions` carrying a
/// version of its own.
#[test]
fn the_version_comes_from_the_file_not_the_key() {
    // `L0037` (nursery) suggests v4 intervals, so it is version-gated:
    // the same source lints differently at v1 and at v4, and the only
    // thing that can say which is the input.
    let src = "for (var i = 0; i < 10; i++) {\nvar u = i\n}\nreturn 0\n";
    let groups = LintGroups {
        pedantic: false,
        nursery: true,
    };

    let (db, v4) = db_with(src);
    let v4_codes = codes(&lint_query(&db, v4, groups));

    let v1 = SourceFile::new(
        &db,
        "/lint-query-tests/v1.leek".to_string(),
        2,
        src.into(),
        1,
        false,
        false,
        0,
    );
    let v1_codes = codes(&lint_query(&db, v1, groups));

    assert!(v4_codes.contains(&"L0037"), "{v4_codes:?}");
    assert!(!v1_codes.contains(&"L0037"), "{v1_codes:?}");
}

/// A clean file earns nothing from either half.
#[test]
fn a_clean_file_has_an_empty_stream() {
    let (db, file) = db_with("function f(a) {\nreturn a + 1\n}\nreturn f(1)\n");
    assert!(
        diagnostics_with_lints(&db, file, LintGroups::default()).is_empty(),
        "{:?}",
        diagnostics_with_lints(&db, file, LintGroups::default())
    );
}
