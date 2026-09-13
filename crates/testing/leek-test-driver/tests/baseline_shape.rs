//! Pins the on-disk baseline shape: `MultiReport::save` stores only the
//! *non-passing* outcomes (the corpus is ~11k mostly-passing cases per
//! backend, and a full pass map costs megabytes of tracked data per
//! refresh), and `diff_against` reads an id the baseline omits as a pass —
//! so dropping the passes keeps the regression check exact and fails
//! closed for ids the baseline predates.

use std::collections::BTreeMap;

use leek_test_driver::backends::MultiReport;
use leek_test_driver::run::{CaseOutcome, Report};

fn report(outcomes: &[(&str, CaseOutcome)]) -> Report {
    let mut r = Report::default();
    for &(id, outcome) in outcomes {
        r.outcomes.insert(id.to_string(), outcome);
        r.summary.total += 1;
        if outcome.is_pass() {
            r.summary.pass += 1;
        }
    }
    r
}

fn multi(report: Report) -> MultiReport {
    let mut backends = BTreeMap::new();
    backends.insert("pipeline".to_string(), report);
    MultiReport {
        schema_version: MultiReport::SCHEMA_VERSION,
        backends,
    }
}

fn tmp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("leek-{name}-{}.toml", std::process::id()))
}

#[test]
fn saved_baseline_keeps_only_non_passing_outcomes() {
    let full = report(&[
        ("pass@v4", CaseOutcome::Pass),
        ("expected-error@v4", CaseOutcome::PassExpectedError),
        ("disabled@v4", CaseOutcome::SkippedDisabled),
        ("unknown@v4", CaseOutcome::SkippedUnknown),
        ("wrong-value@v4", CaseOutcome::FailWrongValue),
    ]);

    let path = tmp_path("baseline-shape");
    multi(full).save(&path).expect("save baseline");
    let loaded = MultiReport::load(&path).expect("load baseline");
    let _ = std::fs::remove_file(&path);

    let stored = &loaded.backends["pipeline"];
    assert_eq!(
        stored.outcomes.keys().collect::<Vec<_>>(),
        ["disabled@v4", "unknown@v4", "wrong-value@v4"],
        "baselines must store the non-passing outcomes only",
    );
    // The summary still describes the whole run, passes included.
    assert_eq!(stored.summary.total, 5);
    assert_eq!(stored.summary.pass_total(), 2);
}

#[test]
fn omitted_baseline_id_counts_as_a_pass() {
    let baseline = report(&[("was-failing@v4", CaseOutcome::FailWrongValue)]).failures_only();

    // Absent from the baseline and still passing → nothing to report.
    let steady = report(&[("pass@v4", CaseOutcome::Pass)]);
    let diff = steady.diff_against(&baseline);
    assert!(diff.regressions.is_empty());
    assert!(diff.improvements.is_empty());

    // Absent from the baseline and now failing → a regression, not a
    // silently-accepted new case.
    let regressed = report(&[("pass@v4", CaseOutcome::FailParseError)]);
    let diff = regressed.diff_against(&baseline);
    assert_eq!(diff.regressions.len(), 1);
    assert_eq!(diff.regressions[0].id, "pass@v4");
    assert_eq!(diff.regressions[0].before, CaseOutcome::Pass);

    // Recorded as failing and now passing → an improvement.
    let fixed = report(&[("was-failing@v4", CaseOutcome::Pass)]);
    let diff = fixed.diff_against(&baseline);
    assert_eq!(diff.improvements.len(), 1);
    assert_eq!(diff.improvements[0].id, "was-failing@v4");
}

#[test]
fn dense_legacy_baselines_still_diff_the_same() {
    let dense = report(&[
        ("a@v4", CaseOutcome::Pass),
        ("b@v4", CaseOutcome::FailWrongValue),
    ]);
    let sparse = dense.failures_only();
    let now = report(&[
        ("a@v4", CaseOutcome::FailParseError),
        ("b@v4", CaseOutcome::Pass),
    ]);

    for baseline in [&dense, &sparse] {
        let diff = now.diff_against(baseline);
        assert_eq!(diff.regressions.len(), 1, "a@v4 regressed");
        assert_eq!(diff.improvements.len(), 1, "b@v4 improved");
    }
}
