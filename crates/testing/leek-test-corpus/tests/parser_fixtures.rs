//! Parser-level round-trip and shape checks against upstream fixtures.
//!
//! Two properties, over every `.leek` file the upstream submodule ships:
//!
//! 1. **Lossless round-trip** — the green tree reconstructs the source
//!    byte for byte. This is the foundation invariant the formatter, the
//!    LSP and incremental reparsing all stand on, and it must hold for
//!    malformed input too: a parse error becomes an `ErrorNode` holding
//!    the tokens verbatim, never a dropped byte.
//! 2. **Clean parse** — no lexer, pragma or parser diagnostics. Unlike
//!    (1), this one really is about the parser understanding the
//!    language, and it is the property that used to be checked on a
//!    single fixture.
//!
//! `tests/fmt_roundtrip.rs` sweeps the same files, but it cannot stand in
//! for (2): `format_source_checked` *discards* parse diagnostics and
//! compares tokens and comments, so a file full of `ErrorNode`s formats
//! "safely".
//!
//! Runs under `.github/workflows/corpus.yml`, which checks out the
//! upstream submodule; `cargo test --workspace` in ci.yml excludes this
//! crate.

use std::path::Path;

use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, parse_pragmas};
use leek_test_corpus::{
    fixture_id, leek_files, upstream_fixture, upstream_fixtures_available, upstream_fixtures_dir,
};

/// How many failing fixtures to name before summarizing.
const REPORTED: usize = 20;

/// What parsing `text` produced: the reconstructed text, and every
/// diagnostic (pragma + parse) rendered for reporting.
fn parse_report(text: &str) -> (String, Vec<String>) {
    let src = SourceId::new(1).expect("1 is a valid source id");
    let (pragmas, pragma_diags) = parse_pragmas(text, src);
    let result = parse(text, src, pragmas.version);
    let node = SyntaxNode::new_root(result.green);
    let diags = pragma_diags
        .iter()
        .chain(result.diagnostics.iter())
        .map(|d| format!("{d:?}"))
        .collect();
    (node.text().to_string(), diags)
}

/// Parse fixture by relative path; assert round-trip and absence of
/// diagnostics.
fn assert_parses(rel: &str) {
    let text = upstream_fixture(rel);
    let (round_tripped, diags) = parse_report(&text);
    assert_eq!(round_tripped, text, "round-trip mismatch in {rel}");
    assert!(
        diags.is_empty(),
        "unexpected diagnostics in {rel}: {diags:?}"
    );
}

/// The fixtures to sweep, or `None` when the upstream submodule is not
/// checked out — a non-recursive clone must skip, not fail.
fn fixtures() -> Option<Vec<std::path::PathBuf>> {
    if !upstream_fixtures_available() {
        eprintln!("skipping: the upstream submodule is not checked out");
        return None;
    }
    let files = leek_files(&upstream_fixtures_dir());
    assert!(
        !files.is_empty(),
        "the fixtures directory holds no .leek files; an empty sweep gates on nothing",
    );
    Some(files)
}

/// Join at most [`REPORTED`] items, then say how many were elided.
fn summarize(items: &[String]) -> String {
    let shown = items
        .iter()
        .take(REPORTED)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");
    if items.len() > REPORTED {
        format!("{shown}\n\n… and {} more", items.len() - REPORTED)
    } else {
        shown
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every upstream fixture reconstructs byte for byte from its green
/// tree. Holds regardless of whether the file parses cleanly — losing a
/// byte on malformed input would silently corrupt any buffer the
/// formatter or the LSP wrote back.
#[test]
fn every_upstream_fixture_round_trips() {
    let Some(files) = fixtures() else { return };
    let failures: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let text = read(path);
            let (round_tripped, _) = parse_report(&text);
            (round_tripped != text).then(|| fixture_id(path))
        })
        .collect();

    assert!(
        failures.is_empty(),
        "{} of {} fixture(s) do not round-trip through the parser:\n\n{}",
        failures.len(),
        files.len(),
        summarize(&failures),
    );
}

/// Every upstream fixture parses with no diagnostics at all.
///
/// There is deliberately no allow-list: a fixture that stops parsing
/// cleanly is a parser gap to close, not a row to add.
#[test]
fn every_upstream_fixture_parses_without_diagnostics() {
    let Some(files) = fixtures() else { return };
    let failures: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let text = read(path);
            let (_, diags) = parse_report(&text);
            (!diags.is_empty()).then(|| format!("{}: {}", fixture_id(path), diags.join(", ")))
        })
        .collect();

    assert!(
        failures.is_empty(),
        "{} of {} fixture(s) produced diagnostics:\n\n{}",
        failures.len(),
        files.len(),
        summarize(&failures),
    );
}

/// `return 'bonjour';` — the simplest possible program, kept as a named
/// smoke case so a total parser failure reports as one obvious line
/// rather than as every fixture at once.
#[test]
fn round_trip_bonjour() {
    if !upstream_fixtures_available() {
        eprintln!("skipping: the upstream submodule is not checked out");
        return;
    }
    assert_parses("bonjour.leek");
}
