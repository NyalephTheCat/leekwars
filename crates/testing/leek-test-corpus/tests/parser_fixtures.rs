//! Parser-level round-trip and shape checks against upstream fixtures.
//!
//! Two properties, over the `.leek` files the upstream submodule ships —
//! and they have deliberately **different scopes**:
//!
//! 1. **Lossless round-trip**, over *every* fixture — the green tree
//!    reconstructs the source byte for byte. This is the foundation
//!    invariant the formatter, the LSP and incremental reparsing all
//!    stand on, and it must hold for malformed input too: a parse error
//!    becomes an `ErrorNode` holding the tokens verbatim, never a
//!    dropped byte. Nothing below narrows it.
//! 2. **Clean parse** — no lexer, pragma or parser diagnostics — over
//!    the fixtures the upstream JUnit suite itself *runs*. Unlike (1),
//!    this one is about the parser understanding the language, so it can
//!    only be asked about programs that are in the language.
//!
//! `tests/fmt_roundtrip.rs` sweeps the same files, but it cannot stand in
//! for (2): `format_source_checked` *discards* parse diagnostics and
//! compares tokens and comments, so a file full of `ErrorNode`s formats
//! "safely".
//!
//! # Why (2) is scoped, and why the scope is derived
//!
//! The fixture tree comes from the standalone **`leekscript` language**
//! submodule, not from the Leek Wars generator's own AI corpus, and a
//! large part of it is written in language that the Leek Wars dialect
//! this toolchain implements does not have and is not going to grow:
//!
//! - **bignum literals** — the `1m` / `5m` suffix in `code/pow5.leek`,
//!   `code/fact1000.leek`, `code/primes_gmp.leek`,
//!   `code/product_n*.leek` and `code/product_coproduct.leek`, which
//!   this lexer reports as `E0002` "numeric literal has a non-digit
//!   suffix";
//! - **`match` with a wildcard arm** — `code/match.leek` is
//!   `let v = "oui"` followed by `match v { "oui": … ..: … }`;
//! - **`let`** as a declaration keyword (`code/array.leek`,
//!   `code/fibonacci_v12.leek`), the `$`-prefixed **dynamic operators**
//!   of `code/dynamic_operators.leek`, and the `{key: value}` object
//!   literals of `code/fold_left_2.leek` / `code/fold_right_2.leek`.
//!
//! "Close the parser gap" is not a coherent goal for any of those, and a
//! gate that demands it is a gate that is red for reasons nobody intends
//! to act on — which is how this test came to report 38 failures on
//! `main` while signalling nothing.
//!
//! Upstream has already made the call, file by file, in
//! `src/test/java/test/Test*.java`: `primes_gmp`, `break_and_continue`,
//! `quine`, `quine_zwik`, `dynamic_operators`, `euler1`, `text_analysis`,
//! `divisors`, `match`, `product_n*` and a dozen more are commented out
//! or spelled `DISABLED_file(…)`, while the ones it genuinely runs are
//! the live `file(…)` / `file_v1(…)` / `file_v2_(…)` / `file_v3(…)` /
//! `file_v4_(…)` call sites. [`upstream_enabled_fixtures`] extracts
//! exactly those (`src/extract.rs`, at build time), so the scope tracks
//! a submodule bump instead of rotting in a hand-written list here.
//!
//! **Do not "fix" this by deleting the scoping.** Widening (2) back to
//! every fixture does not close a gap; it re-reds the gate on a language
//! this compiler does not target. A fixture outside the enabled set
//! still has to round-trip — that is (1), and it is not negotiable.
//!
//! # There is no allow-list
//!
//! There was one: `data/parse-known-failures.tsv`, a ratchet holding the
//! two fixtures upstream enabled that this parser could not read —
//! `code/french.leek` (a deliberate unterminated `/*` at EOF, which
//! upstream's lexer accepts silently) and `code/french.min.leek`
//! (minified source with the commas left out between call arguments and
//! between array elements, which upstream's element loops also accept).
//! Both were real divergences from the reference implementation, both
//! are closed (#351), and a ratchet with nothing in it is not a passing
//! gate but a missing one — so the file went and (2) is a plain
//! assertion again. Should a submodule bump break a fixture, this fails
//! loudly, which is the point. The ratchet machinery itself stays in
//! `leek_test_corpus::parse_ratchet`, with its own tests, for the next
//! parser gap too large to close in the change that finds it; wire it
//! back up only alongside a gap worth tracking.
//!
//! Runs under `.github/workflows/corpus.yml`, which checks out the
//! upstream submodule; `cargo test --workspace` in ci.yml excludes this
//! crate.

use std::collections::BTreeSet;
use std::path::Path;

use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, parse_pragmas};
use leek_test_corpus::{
    fixture_id, leek_files, upstream_enabled_fixtures, upstream_fixture,
    upstream_fixtures_available, upstream_fixtures_dir,
};

/// How many failing fixtures to name before summarizing.
const REPORTED: usize = 20;

/// A parse result: the reconstructed text plus every diagnostic (pragma
/// + parse) in its full `Debug` rendering for the failure report.
struct Parsed {
    round_tripped: String,
    rendered: Vec<String>,
}

fn parse_report(text: &str) -> Parsed {
    let src = SourceId::new(1).expect("1 is a valid source id");
    let (pragmas, pragma_diags) = parse_pragmas(text, src);
    let result = parse(text, src, pragmas.version);
    let node = SyntaxNode::new_root(result.green);
    let all: Vec<_> = pragma_diags
        .iter()
        .chain(result.diagnostics.iter())
        .collect();
    Parsed {
        round_tripped: node.text().to_string(),
        rendered: all.iter().map(|d| format!("{d:?}")).collect(),
    }
}

/// Parse fixture by relative path; assert round-trip and absence of
/// diagnostics.
fn assert_parses(rel: &str) {
    let text = upstream_fixture(rel);
    let parsed = parse_report(&text);
    assert_eq!(parsed.round_tripped, text, "round-trip mismatch in {rel}");
    assert!(
        parsed.rendered.is_empty(),
        "unexpected diagnostics in {rel}: {:?}",
        parsed.rendered
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
    if items.is_empty() {
        return "  (none)".to_string();
    }
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
///
/// Unlike the clean-parse property below, this one is **not** scoped to
/// what upstream enables, and must not become so: the formatter and the
/// LSP see whatever a user opens, dialect or not.
#[test]
fn every_upstream_fixture_round_trips() {
    let Some(files) = fixtures() else { return };
    let failures: Vec<String> = files
        .iter()
        .filter_map(|path| {
            let text = read(path);
            (parse_report(&text).round_tripped != text).then(|| fixture_id(path))
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

/// The scope of the clean-parse gate is real, and big enough to be a
/// gate.
///
/// [`upstream_enabled_fixtures`] is embedded from `OUT_DIR` at build
/// time, so a build that could not see the Java sources — an
/// uninitialised submodule, a target directory shared with a checkout
/// that had none — embeds an *empty* set. That would leave the sweep
/// below covering nothing while still reporting green, which is the
/// failure mode this whole arrangement exists to remove. So the set is
/// checked against the fixtures actually on disk: every enabled id must
/// name a real file, and the set must be a substantial share of them.
#[test]
fn the_enabled_fixture_set_is_derived_and_plausible() {
    let Some(files) = fixtures() else { return };
    let enabled = upstream_enabled_fixtures();
    let on_disk: BTreeSet<String> = files.iter().map(|p| fixture_id(p)).collect();

    let missing: Vec<&String> = enabled.difference(&on_disk).collect();
    assert!(
        missing.is_empty(),
        "upstream enables fixture(s) that are not in the fixture tree: {missing:?}. The \
         extraction in src/extract.rs and the submodule have drifted apart, and a scope \
         naming files that do not exist gates on less than it claims.",
    );

    // A quarter of the tree is well under today's 45 of 101 and well
    // over anything a broken extraction would leave behind.
    let floor = on_disk.len() / 4;
    assert!(
        enabled.len() > floor,
        "only {} of {} fixture(s) came back as upstream-enabled (expected more than {floor}). \
         Either the build could not read the upstream Java sources — check that \
         official-generator/leek-wars-generator/leekscript is checked out, then rebuild — or \
         the `file(…)` scan in src/extract.rs has stopped matching.",
        enabled.len(),
        on_disk.len(),
    );
}

/// Every fixture the upstream suite actually runs parses with no lexer,
/// pragma or parser diagnostic at all. No exceptions and no allow-list:
/// the last two both came from this parser disagreeing with the
/// reference implementation, and both are closed (#351).
///
/// The scope — see this file's module docs — is upstream's own
/// enablement, extracted from its Java sources, never a list written
/// here. A failure is therefore a parser gap against a program upstream
/// compiles, and the fix is in the parser, not in a row added here.
#[test]
fn every_upstream_enabled_fixture_parses_without_diagnostics() {
    let Some(files) = fixtures() else { return };
    let enabled = upstream_enabled_fixtures();

    let mut covered = 0usize;
    let mut rendered: Vec<String> = Vec::new();
    for path in &files {
        let id = fixture_id(path);
        if !enabled.contains(&id) {
            continue;
        }
        covered += 1;
        let parsed = parse_report(&read(path));
        if !parsed.rendered.is_empty() {
            rendered.push(format!("{id}: {}", parsed.rendered.join(", ")));
        }
    }

    assert!(
        rendered.is_empty(),
        "{} of the {covered} upstream-enabled fixture(s) do not parse cleanly. Each one is a \
         program the reference compiler accepts, so each is a parser gap to close (epic R7, \
         #351) — not a row to add to an allow-list:\n\n{}",
        rendered.len(),
        summarize(&rendered),
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
