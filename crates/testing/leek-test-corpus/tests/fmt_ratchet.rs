//! The formatter ratchet's serialise/diff layer (#197).
//!
//! Everything here is pure Rust: no upstream submodule, no formatter, no
//! corpus. The suite that uses it (`fmt_roundtrip.rs`) needs an 11k-case
//! checkout and only runs in `corpus.yml`, which is exactly why the layer it
//! leans on has to be tested on its own — a ratchet is only worth committing
//! if it is stable enough that regenerating it on another machine produces
//! the same file, and strict enough that it cannot pass while measuring
//! nothing.
//!
//! `corpus.yml` triggers on `crates/testing/leek-test-corpus/**`, so a change
//! to the ratchet runs these.

use leek_test_corpus::{
    FmtFailure, FmtRatchet, KIND_NOT_IDEMPOTENT, KIND_UNSAFE, KIND_UNSAFE_REFORMAT,
    diff_fmt_ratchet, first_difference,
};

fn row(kind: &str, detail: &str) -> FmtFailure {
    FmtFailure {
        kind: kind.to_string(),
        detail: detail.to_string(),
    }
}

fn ratchet(total: usize, rows: &[(&str, FmtFailure)]) -> FmtRatchet {
    FmtRatchet::from_failures(
        total,
        rows.iter().map(|(id, r)| ((*id).to_string(), r.clone())),
    )
}

// ---------------------------------------------------------------------
// to_tsv / parse — the tracked file
// ---------------------------------------------------------------------

/// The file is a tracked artefact, so it has to be a *stable* diff: sorted
/// by id regardless of the order the run discovered failures in, and
/// byte-identical between two runs of the same corpus.
#[test]
fn tsv_is_sorted_and_stable_regardless_of_discovery_order() {
    let discovered = ratchet(
        11_005,
        &[
            ("zeta.leek", row(KIND_UNSAFE, "token #1")),
            ("alpha.leek", row(KIND_NOT_IDEMPOTENT, "line 3: a => b")),
            ("mid.leek", row(KIND_UNSAFE_REFORMAT, "token #9")),
        ],
    );
    let reordered = ratchet(
        11_005,
        &[
            ("mid.leek", row(KIND_UNSAFE_REFORMAT, "token #9")),
            ("zeta.leek", row(KIND_UNSAFE, "token #1")),
            ("alpha.leek", row(KIND_NOT_IDEMPOTENT, "line 3: a => b")),
        ],
    );

    let tsv = discovered.to_tsv("the corpus", "make regen");
    assert_eq!(tsv, reordered.to_tsv("the corpus", "make regen"));

    let ids: Vec<&str> = tsv
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').next())
        .collect();
    assert_eq!(ids, ["alpha.leek", "mid.leek", "zeta.leek"]);
}

/// The header has to carry the regeneration command and say the rows are
/// bugs. Someone reading a red build finds this file first, and "entries are
/// accepted behaviour" is the reading that kills the ratchet.
#[test]
fn tsv_header_says_how_to_regenerate_and_that_rows_are_bugs() {
    let tsv = ratchet(11_005, &[("a.leek", row(KIND_UNSAFE, "x"))])
        .to_tsv("the upstream corpus", "cargo run --the-magic-words");

    assert!(tsv.contains("cargo run --the-magic-words"), "{tsv}");
    assert!(tsv.contains("BUG"), "{tsv}");
    assert!(tsv.contains("the upstream corpus"), "{tsv}");
    assert!(tsv.contains("# total=11005"), "{tsv}");
}

/// Writing then reading has to be a fixed point, or the first check after a
/// regeneration reports the file it just wrote as a wall of changes.
#[test]
fn tsv_round_trips_every_kind() {
    let r = ratchet(
        101,
        &[
            (
                "u.leek",
                row(KIND_UNSAFE, "formatting would change token #3"),
            ),
            (
                "r.leek",
                row(KIND_UNSAFE_REFORMAT, "would drop 1 comment(s)"),
            ),
            (
                "i.leek",
                row(KIND_NOT_IDEMPOTENT, "line 2: @pure => @pure "),
            ),
        ],
    );

    let parsed = FmtRatchet::parse(&r.to_tsv("s", "c")).expect("round trip");

    assert_eq!(parsed.total, Some(101));
    assert_eq!(parsed.entries, r.entries);
}

/// Corpus ids come from Java test methods and details from formatter
/// diagnostics; both can contain a tab or a newline. The file is tab- and
/// line-delimited, so an unescaped value would shift every later column and
/// corrupt the ids the gate compares.
#[test]
fn tsv_round_trips_values_containing_tabs_and_newlines() {
    let r = ratchet(
        9,
        &[(
            "Test.java::weird\tname::0@v4",
            row(KIND_NOT_IDEMPOTENT, "line 1: a\tb => c\nd\\e"),
        )],
    );
    let tsv = r.to_tsv("s", "c");

    assert_eq!(
        tsv.lines().filter(|l| !l.starts_with('#')).count(),
        1,
        "an embedded newline split the row:\n{tsv}"
    );

    let parsed = FmtRatchet::parse(&tsv).expect("round trip");
    let got = parsed
        .entries
        .get("Test.java::weird\tname::0@v4")
        .expect("id key survived escaping");
    assert_eq!(got.detail, "line 1: a\tb => c\nd\\e");
}

// ---------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------

/// The whole point: a case that breaks and is not listed fails the build.
#[test]
fn a_newly_broken_case_is_a_regression() {
    let committed = FmtRatchet::parse("# total=100\nold.leek\tunsafe\ttoken #1\n").unwrap();
    let current = ratchet(
        100,
        &[
            ("old.leek", row(KIND_UNSAFE, "token #1")),
            ("new.leek", row(KIND_UNSAFE, "token #7")),
        ],
    );

    let diff = diff_fmt_ratchet(&current, &committed);

    assert!(diff.is_regression());
    assert_eq!(diff.new_ids.len(), 1);
    assert_eq!(diff.new_ids[0].0, "new.leek");
}

/// The other half of a ratchet: a case that stopped failing is reported so
/// the list shrinks, but it is not a failure — otherwise fixing a formatter
/// bug turns the build red.
#[test]
fn a_fixed_case_is_reported_and_is_not_a_regression() {
    let committed = FmtRatchet::parse("# total=100\ngone.leek\tunsafe\ttoken #1\n").unwrap();
    let current = ratchet(100, &[]);

    let diff = diff_fmt_ratchet(&current, &committed);

    assert_eq!(diff.fixed_ids, ["gone.leek"]);
    assert!(!diff.is_regression());
}

/// A formatter change can reword a diagnostic or move a token index without
/// fixing or breaking anything. The case is still broken and still listed,
/// so that must be information, not a red gate — a red gate here teaches
/// people to regenerate the file, and regenerating is how a real regression
/// gets baked in.
#[test]
fn a_reworded_detail_is_informational_not_a_regression() {
    let committed = FmtRatchet::parse("# total=100\nsame.leek\tunsafe\ttoken #4\n").unwrap();
    let current = ratchet(100, &[("same.leek", row(KIND_UNSAFE, "token #5"))]);

    let diff = diff_fmt_ratchet(&current, &committed);

    assert!(!diff.is_regression());
    assert!(diff.fixed_ids.is_empty());
    assert_eq!(diff.changed_detail.len(), 1);
    assert_eq!(
        diff.changed_detail[0],
        (
            "same.leek".to_string(),
            "unsafe token #4".to_string(),
            "unsafe token #5".to_string(),
        )
    );
}

/// A file with no rows is the shape a ratchet takes when someone empties it
/// to get a green build. It has to be an error: an empty allow-list and a
/// deleted gate are indistinguishable from the diff's point of view, and the
/// honest way to zero is to delete the gate with the last bug.
#[test]
fn an_empty_list_is_refused_rather_than_treated_as_a_clean_run() {
    let empty = FmtRatchet::parse("# total=100\n").unwrap();
    let path = std::path::Path::new("data/fmt-known-failures-corpus.tsv");

    let err = empty
        .check_gateable(path)
        .expect_err("empty list must fail");

    let msg = format!("{err:#}");
    assert!(msg.contains("not a passing gate"), "{msg}");
    assert!(msg.contains("fmt-known-failures-corpus.tsv"), "{msg}");

    assert!(
        FmtRatchet::parse("# total=100\na.leek\tunsafe\tx\n")
            .unwrap()
            .check_gateable(path)
            .is_ok()
    );
}

/// Comparing a filtered run (`--test-threads`, a `--exact` filter, a
/// truncated manifest) against the full-corpus file would report every unrun
/// id as "fixed" and let the gate pass while checking a handful of cases.
#[test]
fn refuses_to_compare_against_a_run_of_a_different_size() {
    let committed = FmtRatchet::parse("# total=11005\na.leek\tunsafe\tx\n").unwrap();

    assert!(committed.check_comparable(20).is_err());
    // A handful of cases added or removed upstream is fine.
    assert!(committed.check_comparable(11_010).is_ok());
}

/// A file with no header cannot say how big its run was, so it cannot refuse
/// a truncated comparison — but it must not *panic* either; the size check
/// is a backstop, not a schema requirement.
#[test]
fn a_headerless_file_still_parses_and_gates() {
    let committed = FmtRatchet::parse("a.leek\tunsafe\tx\n").unwrap();

    assert_eq!(committed.total, None);
    assert!(committed.check_comparable(3).is_ok());
    assert!(diff_fmt_ratchet(&ratchet(3, &[]), &committed).fixed_ids == ["a.leek"]);
}

// ---------------------------------------------------------------------
// first_difference — the non-idempotent signature
// ---------------------------------------------------------------------

/// Storing both whole outputs would put megabytes of LeekScript in a tracked
/// file and turn any reflow into a thousand-line diff. The signature has to
/// be the one line that identifies the defect.
#[test]
fn first_difference_names_the_line_that_moved() {
    let once = "function f() {\n@pure  @unused\nreturn 1;\n}\n";
    let twice = "function f() {\n@pure @unused\nreturn 1;\n}\n";

    assert_eq!(
        first_difference(once, twice),
        "line 2: @pure  @unused => @pure @unused",
    );
}

/// A second pass that only adds or drops trailing lines still has to produce
/// a signature rather than an empty cell.
#[test]
fn first_difference_handles_a_truncated_or_extended_second_pass() {
    assert_eq!(first_difference("a\nb\n", "a\n"), "line 2: b => <eof>");
    assert_eq!(first_difference("a\n", "a\nb\n"), "line 2: <eof> => b");
    assert_eq!(
        first_difference("a\n", "a"),
        "trailing whitespace only",
        "`lines()` hides a missing final newline; say so instead of lying about a line",
    );
}

/// Details land in a tab-delimited file a human reads in a diff, so a long
/// line is clipped rather than pasted whole.
#[test]
fn first_difference_clips_a_very_long_line() {
    let long = "x".repeat(500);
    let sig = first_difference(&format!("{long}\n"), "y\n");

    assert!(sig.ends_with("=> y"), "{sig}");
    assert!(sig.contains('…'), "{sig}");
    assert!(
        sig.len() < 200,
        "{} chars is not a one-line detail",
        sig.len()
    );
}

/// Sanity: a row is a plain value type the suite can build and compare.
#[test]
fn rows_compare_by_value_and_describe_themselves() {
    let a = row(KIND_UNSAFE, "token #1");
    assert_eq!(a.clone(), a);
    assert_eq!(a.describe(), "unsafe token #1");
    assert_eq!(row(KIND_UNSAFE, "").describe(), "unsafe");
}
