//! The parser ratchet's serialise/diff layer.
//!
//! Everything here is pure Rust: no upstream submodule, no parser, no
//! fixtures. The suite that uses it (`parser_fixtures.rs`) needs the
//! submodule checked out and only runs in `corpus.yml`, which is exactly
//! why the layer it leans on has to be tested on its own — a ratchet is
//! only worth committing if regenerating it on another machine produces
//! the same file, and strict enough that it cannot pass while measuring
//! nothing.
//!
//! The gate this layer implements is stricter than
//! `leek_test_corpus::fmt_ratchet`'s on purpose, and the tests below pin
//! the difference: a row that starts passing, and a row whose detail no
//! longer matches, both fail the build here. See the `parse_ratchet`
//! module docs for why.
//!
//! `corpus.yml` triggers on `crates/testing/leek-test-corpus/**`, so a
//! change to the ratchet runs these.

use leek_test_corpus::{ParseFailure, ParseRatchet, diff_parse_ratchet};

fn diag(code: &str, message: &str) -> (String, String) {
    (code.to_string(), message.to_string())
}

fn row(kind: &str, detail: &str) -> ParseFailure {
    ParseFailure {
        kind: kind.to_string(),
        detail: detail.to_string(),
    }
}

fn ratchet(total: usize, rows: &[(&str, ParseFailure)]) -> ParseRatchet {
    ParseRatchet::from_failures(
        total,
        rows.iter().map(|(id, r)| ((*id).to_string(), r.clone())),
    )
}

// ---------------------------------------------------------------------
// ParseFailure::from_diagnostics — what one row says
// ---------------------------------------------------------------------

/// The kind cell is a histogram in first-seen order, so a row groups by
/// defect and moves the moment the diagnostics do. The detail is the
/// *first* message only: a minified fixture produces sixteen of them,
/// and a row carrying all sixteen would be unreadable in a diff while
/// telling a reader nothing the count does not.
#[test]
fn a_row_counts_codes_in_first_seen_order_and_keeps_the_first_message() {
    let failure = ParseFailure::from_diagnostics(&[
        diag("E0100", "expected RParen, found StringLiteral"),
        diag("E0002", "numeric literal has a non-digit suffix"),
        diag("E0100", "unexpected token: RParen"),
        diag("E0100", "unexpected token: Comma"),
    ]);
    assert_eq!(failure.kind, "E0100 x3, E0002 x1");
    assert_eq!(failure.detail, "expected RParen, found StringLiteral");
}

/// A long message is clipped rather than wrapped into the file: the full
/// diagnostics are printed by the suite when it fails, and the file is
/// for diffing.
#[test]
fn a_long_message_is_clipped() {
    let long = "x".repeat(400);
    let failure = ParseFailure::from_diagnostics(&[diag("E0100", &long)]);
    assert!(
        failure.detail.chars().count() < long.chars().count(),
        "a 400-character message must not land in the file verbatim",
    );
    assert!(failure.detail.ends_with('…'));
}

/// A fixture with no diagnostics parses cleanly. Building a row for it
/// would put a passing fixture in the known-bad file, which is the one
/// thing the gate exists to stop.
#[test]
#[should_panic(expected = "parses cleanly")]
fn a_clean_fixture_cannot_have_a_row() {
    let _ = ParseFailure::from_diagnostics(&[]);
}

// ---------------------------------------------------------------------
// to_tsv / parse — the tracked file
// ---------------------------------------------------------------------

/// The file is a tracked artefact, so it has to be a *stable* diff:
/// sorted by id regardless of the order the sweep discovered failures
/// in, and byte-identical between two runs of the same fixture set.
#[test]
fn tsv_is_sorted_and_stable_regardless_of_discovery_order() {
    let discovered = ratchet(
        45,
        &[
            ("code/french.min.leek", row("E0100 x16", "expected RParen")),
            ("code/french.leek", row("W0005 x1", "block comment")),
        ],
    );
    let reordered = ratchet(
        45,
        &[
            ("code/french.leek", row("W0005 x1", "block comment")),
            ("code/french.min.leek", row("E0100 x16", "expected RParen")),
        ],
    );

    let tsv = discovered.to_tsv("cargo test");
    assert_eq!(tsv, reordered.to_tsv("cargo test"));
    let french = tsv.find("code/french.leek").expect("french row present");
    let min = tsv.find("code/french.min.leek").expect("min row present");
    assert!(french < min, "rows must be sorted by id");
}

/// Serialise, read back, get the same ratchet — including ids and
/// details holding the delimiters the file is made of.
#[test]
fn tsv_round_trips_including_tabs_and_newlines() {
    let original = ratchet(
        45,
        &[
            ("code/french.leek", row("W0005 x1", "block\tcomment\nopen")),
            ("odd\\path.leek", row("E0100 x1", "back\\slash")),
        ],
    );
    let parsed = ParseRatchet::parse(&original.to_tsv("cargo test")).expect("parse");
    assert_eq!(parsed.total, Some(45));
    assert_eq!(parsed.entries, original.entries);
}

// ---------------------------------------------------------------------
// The two honesty checks
// ---------------------------------------------------------------------

/// An empty file and a deleted file look identical to a diff that only
/// reports new ids, so an empty one is an error with an explicit way
/// out, not a silent pass.
#[test]
fn an_empty_file_is_not_a_passing_gate() {
    let empty = ParseRatchet::parse("# total=45\n").expect("parse");
    let err = empty
        .check_gateable(std::path::Path::new("data/parse-known-failures.tsv"))
        .expect_err("an empty ratchet must be rejected");
    assert!(err.to_string().contains("not a passing gate"));
}

/// A sweep that covered a different number of fixtures cannot be diffed
/// against this file: every unrun fixture would read as "now clean".
/// Unlike the corpus-sized ratchets there is no percentage slack here —
/// the enabled set is small enough that one fixture is real movement.
#[test]
fn a_differently_sized_run_is_refused() {
    let committed = ratchet(
        45,
        &[("code/french.leek", row("W0005 x1", "block comment"))],
    );
    committed.check_comparable(45).expect("same size is fine");
    let err = committed
        .check_comparable(44)
        .expect_err("one fixture fewer must be refused");
    assert!(err.to_string().contains("44"));
    assert!(err.to_string().contains("45"));
    committed
        .check_comparable(46)
        .expect_err("one fixture more must be refused too");
}

/// A file written before the header existed carries no `total`, and
/// there is nothing to compare against — that is not a reason to fail.
#[test]
fn a_file_without_a_total_header_is_comparable_to_anything() {
    let committed =
        ParseRatchet::parse("code/french.leek\tW0005 x1\tblock comment\n").expect("parse");
    assert_eq!(committed.total, None);
    committed.check_comparable(45).expect("nothing to compare");
}

// ---------------------------------------------------------------------
// The diff — all three buckets gate
// ---------------------------------------------------------------------

/// The ratchet's whole reason to exist: a fixture that fails and is not
/// listed is a parser regression.
#[test]
fn a_newly_failing_fixture_is_a_regression() {
    let committed = ratchet(
        45,
        &[("code/french.leek", row("W0005 x1", "block comment"))],
    );
    let current = ratchet(
        45,
        &[
            ("code/french.leek", row("W0005 x1", "block comment")),
            ("code/gcd.leek", row("E0100 x1", "unexpected token: KwLet")),
        ],
    );

    let diff = diff_parse_ratchet(&current, &committed);
    assert_eq!(diff.new_ids.len(), 1);
    assert_eq!(diff.new_ids[0].0, "code/gcd.leek");
    assert!(diff.fixed_ids.is_empty());
    assert!(diff.is_regression());
}

/// A closed gap whose row was left behind fails the build too — this is
/// where the parser ratchet is deliberately stricter than the
/// formatter's, which only *reports* it. The file has two rows; a stale
/// one is a tracked file lying about what is broken, and the list is
/// supposed to be able only to shrink.
#[test]
fn a_row_that_starts_parsing_cleanly_fails_the_build() {
    let committed = ratchet(
        45,
        &[
            ("code/french.leek", row("W0005 x1", "block comment")),
            ("code/french.min.leek", row("E0100 x16", "expected RParen")),
        ],
    );
    let current = ratchet(
        45,
        &[("code/french.leek", row("W0005 x1", "block comment"))],
    );

    let diff = diff_parse_ratchet(&current, &committed);
    assert_eq!(diff.fixed_ids, ["code/french.min.leek"]);
    assert!(diff.new_ids.is_empty());
    assert!(
        diff.is_regression(),
        "a closed gap with its row still in the file must fail, or nobody ever deletes the row",
    );
}

/// Same fixture, different diagnostics: still broken, but the row no
/// longer describes it. The details here are this toolchain's *own*
/// messages, so keeping them true costs one command — unlike the
/// formatter ratchet, where a JDK bump could reword hundreds at once.
#[test]
fn a_row_whose_diagnostics_moved_fails_the_build() {
    let committed = ratchet(
        45,
        &[("code/french.min.leek", row("E0100 x16", "expected RParen"))],
    );
    let current = ratchet(
        45,
        &[("code/french.min.leek", row("E0100 x4", "expected RParen"))],
    );

    let diff = diff_parse_ratchet(&current, &committed);
    assert!(diff.new_ids.is_empty());
    assert!(diff.fixed_ids.is_empty());
    assert_eq!(diff.changed_detail.len(), 1);
    assert_eq!(diff.changed_detail[0].0, "code/french.min.leek");
    assert!(diff.is_regression());
}

/// The steady state: the same fixtures failing the same way is the only
/// thing that passes.
#[test]
fn an_unchanged_run_is_not_a_regression() {
    let rows: &[(&str, ParseFailure)] = &[
        ("code/french.leek", row("W0005 x1", "block comment")),
        ("code/french.min.leek", row("E0100 x16", "expected RParen")),
    ];
    let diff = diff_parse_ratchet(&ratchet(45, rows), &ratchet(45, rows));
    assert!(!diff.is_regression());
    assert!(diff.changed_detail.is_empty());
}
