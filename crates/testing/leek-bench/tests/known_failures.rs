//! The fast rust-java sweep's failure ratchet (#107).
//!
//! Everything here is pure Rust: no JDK, no upstream classes, no sweep. The
//! sweep itself needs both and so cannot run in CI, which is exactly why the
//! serialise/diff layer has to be tested on its own — the ratchet is only
//! worth committing if it is stable enough that a developer regenerating it on
//! a different machine produces the same file.

use std::time::Duration;

use leek_bench::{
    FastOutcome, FastReport, KnownFailure, diff_known_failures, javac_signature,
    parse_known_failures,
};

fn report(failures: Vec<(&str, FastOutcome)>) -> FastReport {
    let total = 9519;
    FastReport {
        total,
        agree: total - failures.len(),
        failures: failures
            .into_iter()
            .map(|(id, o)| (id.to_string(), o))
            .collect(),
        elapsed: Duration::from_secs(85),
        javac_rounds: 27,
    }
}

fn compile_error(line: &str) -> FastOutcome {
    FastOutcome::CompileError {
        javac: line.to_string(),
    }
}

// ---------------------------------------------------------------------
// javac_signature — the grouping key
// ---------------------------------------------------------------------

/// The point of the signature: two cases broken by the *same* emitter defect
/// have to collapse onto one key. Raw `javac` lines never do — they carry a
/// per-pid temp dir, a line number that moves whenever emission shifts, and a
/// per-case `AI_<n>` / `u_<n>` name. Grouping on the raw line would report 49
/// distinct compile errors where there are three defects.
#[test]
fn javac_signature_groups_the_same_defect_across_cases() {
    let a = javac_signature(
        "/tmp/leek-fastjava-981/AI_7.java:12: error: variable __scrut is \
         already defined in method runIA()",
    );
    let b = javac_signature(
        "/tmp/leek-fastjava-4402/AI_318.java:97: error: variable __scrut is \
         already defined in method runIA()",
    );

    assert_eq!(a, "variable __scrut is already defined in method runIA()");
    assert_eq!(a, b);
}

/// Generated identifiers are normalised, real ones are not: `u_3` is the
/// emitter's name for a user variable and differs per case, but `menu_12` is a
/// name from the program under test and is part of the defect.
#[test]
fn javac_signature_normalises_generated_names_only() {
    assert_eq!(
        javac_signature("/tmp/x/AI_9.java:3: error: cannot find symbol: variable u_3 in AI_9"),
        "cannot find symbol: variable u_# in AI_#",
    );
    assert_eq!(
        javac_signature("/tmp/x/AI_9.java:3: error: cannot find symbol: variable menu_12"),
        "cannot find symbol: variable menu_12",
    );
}

/// A line with no `error:` marker still has to produce a key rather than an
/// empty string, or every unattributable failure collapses into one bucket
/// that says nothing.
#[test]
fn javac_signature_keeps_lines_without_an_error_marker() {
    assert_eq!(
        javac_signature("<unattributed javac failure>"),
        "<unattributed javac failure>",
    );
}

// ---------------------------------------------------------------------
// to_tsv / parse_known_failures
// ---------------------------------------------------------------------

/// The file is a tracked artefact, so it has to be a *stable* diff: sorted by
/// id regardless of the order the sweep discovered failures in (emit failures
/// come first, then compile, then run), and byte-identical between two runs.
#[test]
fn tsv_is_sorted_and_stable_regardless_of_discovery_order() {
    let discovered = report(vec![
        ("zeta/999", compile_error("/tmp/x/AI_1.java:1: error: boom")),
        (
            "alpha/001",
            FastOutcome::Disagree {
                got: "1".into(),
                expected: "2".into(),
            },
        ),
        ("mid/500", FastOutcome::RuntimeError("NullPointer".into())),
    ]);
    let reordered = report(vec![
        ("mid/500", FastOutcome::RuntimeError("NullPointer".into())),
        (
            "alpha/001",
            FastOutcome::Disagree {
                got: "1".into(),
                expected: "2".into(),
            },
        ),
        ("zeta/999", compile_error("/tmp/x/AI_1.java:1: error: boom")),
    ]);

    assert_eq!(discovered.to_tsv(), reordered.to_tsv());

    let tsv = discovered.to_tsv();
    let ids: Vec<String> = tsv
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').next().unwrap_or_default().to_string())
        .collect();
    assert_eq!(ids, ["alpha/001", "mid/500", "zeta/999"]);
}

/// The header records how many cases the writing sweep covered, so a later
/// `--check` can refuse to compare against a truncated run.
#[test]
fn tsv_records_the_case_count_it_was_written_from() {
    let tsv = report(vec![("a", FastOutcome::Timeout)]).to_tsv();

    assert!(tsv.contains("# total=9519"), "{tsv}");
    assert_eq!(parse_known_failures(&tsv).unwrap().total, Some(9519));
}

/// Corpus values are arbitrary Leekscript exports and routinely contain tabs
/// and newlines (string cases, multi-line arrays). The file is tab- and
/// line-delimited, so an unescaped value would silently shift every later
/// column and corrupt the ids the gate compares.
#[test]
fn tsv_round_trips_values_containing_tabs_and_newlines() {
    let nasty = FastOutcome::Disagree {
        got: "a\tb\nc\\d".into(),
        expected: "x".into(),
    };
    let tsv = report(vec![("case\twith\ttabs", nasty)]).to_tsv();

    assert_eq!(
        tsv.lines().filter(|l| !l.starts_with('#')).count(),
        1,
        "an embedded newline split the row:\n{tsv}"
    );

    let parsed = parse_known_failures(&tsv).unwrap();
    let row = parsed.entries.get("case\twith\ttabs").expect("id key");
    assert_eq!(row.kind, "disagree");
    assert_eq!(row.detail, "a\tb\nc\\d => x");
}

/// Writing then reading has to be a fixed point, or `--check` reports the file
/// it just wrote as a wall of changes.
#[test]
fn tsv_round_trips_every_outcome_kind() {
    let r = report(vec![
        (
            "d",
            FastOutcome::Disagree {
                got: "1".into(),
                expected: "2".into(),
            },
        ),
        (
            "c",
            compile_error("/tmp/x/AI_4.java:9: error: cannot find symbol"),
        ),
        ("e", FastOutcome::EmitError("unsupported node".into())),
        ("r", FastOutcome::RuntimeError("ArithmeticException".into())),
        ("t", FastOutcome::Timeout),
        ("n", FastOutcome::NoResult),
    ]);

    let parsed = parse_known_failures(&r.to_tsv()).unwrap();

    assert_eq!(parsed.entries, r.to_known_failures().entries);
    let kinds: Vec<&str> = parsed.entries.values().map(|v| v.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "compile-error",
            "disagree",
            "emit-error",
            "no-result",
            "runtime-error",
            "timeout"
        ],
    );
}

/// Comparing a `--limit`-truncated sweep against the full-corpus file would
/// report every unrun case as "fixed" and let the gate pass while running 20
/// cases. The header check is the backstop for that.
#[test]
fn refuses_to_compare_against_a_sweep_of_a_different_size() {
    let committed = parse_known_failures("# total=9519\na\tdisagree\t1 => 2\n").unwrap();

    assert!(committed.check_comparable(20).is_err());
    // A handful of cases added or removed upstream is fine.
    assert!(committed.check_comparable(9_520).is_ok());
}

// ---------------------------------------------------------------------
// diff_known_failures — the gate
// ---------------------------------------------------------------------

#[test]
fn a_newly_broken_case_is_a_regression() {
    let committed = parse_known_failures("# total=9519\nold\tdisagree\t1 => 2\n").unwrap();
    let current = report(vec![
        (
            "old",
            FastOutcome::Disagree {
                got: "1".into(),
                expected: "2".into(),
            },
        ),
        (
            "new",
            FastOutcome::Disagree {
                got: "7".into(),
                expected: "8".into(),
            },
        ),
    ])
    .to_known_failures();

    let diff = diff_known_failures(&current, &committed);

    assert_eq!(diff.new_ids.len(), 1);
    assert_eq!(diff.new_ids[0].0, "new");
    assert!(diff.is_regression());
}

/// The other half of a ratchet: a case that stopped failing is reported so the
/// file can be tightened, but it is not a failure.
#[test]
fn a_fixed_case_is_reported_and_is_not_a_regression() {
    let committed =
        parse_known_failures("# total=9519\ngone\tcompile-error\tcannot find symbol\n").unwrap();
    let current = report(vec![]).to_known_failures();

    let diff = diff_known_failures(&current, &committed);

    assert_eq!(diff.fixed_ids, ["gone"]);
    assert!(!diff.is_regression());
}

/// A JDK bump can reword hundreds of `javac` messages at once without any
/// compiler change. That must show up as information, never as a red gate —
/// otherwise the first person to upgrade their JDK has to regenerate the file
/// to get a green build, and regenerating is how a real regression gets
/// baked in.
#[test]
fn a_reworded_message_is_informational_not_a_regression() {
    let committed =
        parse_known_failures("# total=9519\nsame\tcompile-error\tcannot find symbol\n").unwrap();
    let current = report(vec![(
        "same",
        compile_error("/tmp/x/AI_2.java:5: error: cannot resolve symbol"),
    )])
    .to_known_failures();

    let diff = diff_known_failures(&current, &committed);

    assert!(diff.new_ids.is_empty());
    assert!(diff.fixed_ids.is_empty());
    assert_eq!(diff.changed_detail.len(), 1);
    assert_eq!(diff.changed_detail[0].0, "same");
    assert!(!diff.is_regression());
}

/// `BatchRunner`'s timeout only interrupts, and a CPU-bound Leekscript loop
/// never checks interrupts (#297), so a runaway case keeps a core and pushes
/// later *correct* cases past the budget. Which cases land in `timeout` /
/// `no-result` therefore depends on case order and machine speed. Gating on
/// them would make the ratchet flaky, so they are reported separately.
#[test]
fn newly_timing_out_cases_are_reported_but_do_not_gate() {
    let committed = parse_known_failures("# total=9519\n").unwrap();
    let current = report(vec![
        ("slow", FastOutcome::Timeout),
        ("silent", FastOutcome::NoResult),
    ])
    .to_known_failures();

    let diff = diff_known_failures(&current, &committed);

    assert!(diff.new_ids.is_empty());
    assert_eq!(diff.flaky_new_ids.len(), 2);
    assert!(!diff.is_regression());
}

// ---------------------------------------------------------------------
// signature_histogram
// ---------------------------------------------------------------------

/// "49 compile errors" is a number; "38 × variable __scrut is already
/// defined" is a bug report. The histogram is the prioritisation half of the
/// recommendation, and it has to be deterministic (count desc, then
/// signature) so two runs print the same thing.
#[test]
fn histogram_groups_by_signature_commonest_first() {
    let r = report(vec![
        (
            "a",
            compile_error("/tmp/x/AI_1.java:3: error: variable __scrut is already defined"),
        ),
        (
            "b",
            compile_error("/tmp/y/AI_88.java:41: error: variable __scrut is already defined"),
        ),
        (
            "c",
            compile_error("/tmp/y/AI_2.java:7: error: incompatible types"),
        ),
        (
            "d",
            FastOutcome::Disagree {
                got: "1".into(),
                expected: "2".into(),
            },
        ),
    ]);

    assert_eq!(
        r.signature_histogram(),
        vec![
            (
                "compile-error variable __scrut is already defined".to_string(),
                2
            ),
            ("compile-error incompatible types".to_string(), 1),
            ("disagree 1 => 2".to_string(), 1),
        ],
    );
}

/// A row with no detail still needs a printable signature.
#[test]
fn histogram_handles_detail_free_kinds() {
    let r = report(vec![("t", FastOutcome::Timeout)]);
    assert_eq!(r.signature_histogram(), vec![("timeout".to_string(), 1)]);
}

/// Sanity: `KnownFailure` is a plain value type the CLI can build and compare.
#[test]
fn known_failure_rows_compare_by_value() {
    let a = KnownFailure {
        kind: "timeout".into(),
        detail: String::new(),
    };
    assert_eq!(a.clone(), a);
}
