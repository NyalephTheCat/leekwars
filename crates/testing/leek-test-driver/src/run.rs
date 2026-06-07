//! Run extracted upstream cases through the recipe pipeline and
//! classify outcomes against upstream expectations.

use std::collections::BTreeMap;
use leek_parser::ast::SourceFile;
use leek_span::SourceId;
use serde::{Deserialize, Serialize};

use crate::cases::{Manifest, TestCase};

/// Per-case outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CaseOutcome {
    /// Pipeline succeeded and the upstream expects success → pass.
    Pass,

    /// Pipeline emitted exactly the kind of error the upstream wanted
    /// (today: any parse-time error matches any expected compile error,
    /// since our error codes don't line up yet).
    PassExpectedError,

    /// Upstream expects success, our pipeline emitted errors. A real
    /// failure or a "feature not yet implemented" gap.
    FailParseError,

    /// Upstream expects an error, our pipeline didn't emit one. Either
    /// our parser is too lenient or the error is supposed to come from
    /// a later stage we haven't built.
    FailMissingError,

    /// Upstream expected a specific runtime value (`.almost(X)` or
    /// `.ops(N)`) and our interpreter produced something different.
    FailWrongValue,

    /// Upstream marked this `DISABLED_…` — we skip and don't count
    /// against pass/fail.
    SkippedDisabled,

    /// We couldn't recover a useful expectation from the upstream call
    /// (parser of Java sources fell back to Unknown).
    SkippedUnknown,
}

impl CaseOutcome {
    pub fn is_pass(self) -> bool {
        matches!(self, Self::Pass | Self::PassExpectedError)
    }
    pub fn is_skip(self) -> bool {
        matches!(self, Self::SkippedDisabled | Self::SkippedUnknown)
    }
    pub fn is_fail(self) -> bool {
        !self.is_pass() && !self.is_skip()
    }
}

/// Aggregate report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    /// Per-case outcomes, keyed by `TestCase::id`.
    pub outcomes: BTreeMap<String, CaseOutcome>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub total: u32,
    pub pass: u32,
    pub pass_expected_error: u32,
    pub fail_parse_error: u32,
    pub fail_missing_error: u32,
    pub fail_wrong_value: u32,
    pub skipped_disabled: u32,
    pub skipped_unknown: u32,
}

impl Summary {
    pub(crate) fn record(&mut self, o: CaseOutcome) {
        self.total += 1;
        match o {
            CaseOutcome::Pass => self.pass += 1,
            CaseOutcome::PassExpectedError => self.pass_expected_error += 1,
            CaseOutcome::FailParseError => self.fail_parse_error += 1,
            CaseOutcome::FailMissingError => self.fail_missing_error += 1,
            CaseOutcome::FailWrongValue => self.fail_wrong_value += 1,
            CaseOutcome::SkippedDisabled => self.skipped_disabled += 1,
            CaseOutcome::SkippedUnknown => self.skipped_unknown += 1,
        }
    }
    pub fn pass_total(&self) -> u32 {
        self.pass + self.pass_expected_error
    }
    pub fn fail_total(&self) -> u32 {
        self.fail_parse_error + self.fail_missing_error + self.fail_wrong_value
    }
    pub fn skip_total(&self) -> u32 {
        self.skipped_unknown
    }

    /// Cases that participate in pass/fail rates (excludes upstream DISABLED).
    pub fn active_total(&self) -> u32 {
        self.total.saturating_sub(self.skipped_disabled)
    }
}

/// Run every case on all linked backends (see [`crate::backends`]).
pub fn run_manifest_all(manifest: &Manifest) -> crate::backends::MultiReport {
    let backends = crate::backends::detect_backends(None);
    crate::backends::run_manifest(manifest, &backends)
}

/// Classify a single case on the pipeline backend.
pub fn run_one(case: &TestCase, source: SourceId) -> CaseOutcome {
    crate::backends::run_case_backend(case, source, crate::backends::SuiteBackend::Pipeline)
}

/// Evaluate the expected `.almost(X)` side and compare with the program's
/// runtime result. The expected side may be a plain float (`0.42`/`1.5e3`),
/// a `value, delta` pair, or a small Java-math expression (`Math.PI / 2`,
/// `Math.sqrt(2)`); evaluation is shared with the native backend via
/// [`crate::backends::eval_java_almost_expected`]. Tolerance is
/// `1e-9 * max(|expected|, 1)` unless an explicit delta is given — matching
/// upstream's loose float comparison.
///
/// Returns `Some(true)`/`Some(false)` for a real verdict, or `None` when the
/// expected side can't be evaluated here — the caller skips those rather than
/// false-passing (the previous behaviour silently returned `true`).
pub(crate) fn check_almost(
    file: &SourceFile,
    source: SourceId,
    case: &TestCase,
    expected_str: &str,
) -> Option<bool> {
    // Fail-closed: if we can't evaluate the expectation, skip — never pass.
    let (expected, explicit_delta) = crate::backends::eval_java_almost_expected(expected_str)?;
    let (hir, _) = leek_hir::lower_file_versioned(file, source, case.version);
    leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(case.version));
    let run = leek_backend_interp::run_with_limit_version(&hir, 100_000_000, case.version);
    if run.error.is_some() {
        return Some(false);
    }
    // v1 formats reals with comma decimal — normalize.
    let normalized = run.value.to_string().replace(',', ".");
    let Ok(got) = normalized.parse::<f64>() else {
        return Some(false);
    };
    let tol = explicit_delta.unwrap_or_else(|| 1e-9_f64.max(expected.abs() * 1e-9));
    Some((got - expected).abs() <= tol)
}

/// Upstream `.equals("value")` — run the program on the interpreter and
/// compare the produced value's display form. Mirrors the `Equals` arm of
/// [`crate::backends::run_interp`] so the pipeline backend value-checks
/// `Equals` cases instead of passing on clean compilation alone.
pub(crate) fn check_equals(
    file: &SourceFile,
    source: SourceId,
    case: &TestCase,
    expected_value: &str,
) -> bool {
    let (hir, _) = leek_hir::lower_file_versioned(file, source, case.version);
    leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(case.version));
    let run = leek_backend_interp::run_with_limit_version_strict(
        &hir,
        100_000_000,
        case.version,
        case.strict,
    );
    run.error.is_none() && run.value.to_string() == expected_value
}

/// Run the program and verify the op count matches upstream's
/// `.ops(N)` expectation. Our op-cost model is approximate, so
/// we only accept *exact* matches; mismatches are recorded as
/// wrong-value (not false-pass).
pub(crate) fn check_ops(file: &SourceFile, source: SourceId, case: &TestCase, expected: u64) -> bool {
    let (hir, _) = leek_hir::lower_file_versioned(file, source, case.version);
    let (_result, used) = leek_backend_interp::run_with_ops_used(&hir, 100_000_000, case.version);
    used == expected
}

/// Upstream `.equalsOps("value", N)` — value and op count must both match.
pub(crate) fn check_equals_ops(
    file: &SourceFile,
    source: SourceId,
    case: &TestCase,
    expected_value: &str,
    expected_ops: u64,
) -> bool {
    let (hir, _) = leek_hir::lower_file_versioned(file, source, case.version);
    leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(case.version));
    let (result, used) = leek_backend_interp::run_with_ops_used(&hir, 100_000_000, case.version);
    result.error.is_none() && used == expected_ops && result.value.to_string() == expected_value
}

impl Report {
    /// Compare this report against a stored baseline. Returns
    /// regressions (ids that were passing but now fail) and
    /// improvements (ids that were failing but now pass). Cases not
    /// in the baselizne are reported separately.
    pub fn diff_against(&self, baseline: &Report) -> Diff {
        let mut diff = Diff::default();
        for (id, &now) in &self.outcomes {
            match baseline.outcomes.get(id) {
                None => diff.added.push((id.clone(), now)),
                Some(&before) if before == now => {}
                Some(&before) => {
                    if before.is_pass() && !now.is_pass() {
                        diff.regressions.push(Change {
                            id: id.clone(),
                            before,
                            after: now,
                        });
                    } else if !before.is_pass() && now.is_pass() {
                        diff.improvements.push(Change {
                            id: id.clone(),
                            before,
                            after: now,
                        });
                    } else {
                        diff.churn.push(Change {
                            id: id.clone(),
                            before,
                            after: now,
                        });
                    }
                }
            }
        }
        for id in baseline.outcomes.keys() {
            if !self.outcomes.contains_key(id) {
                diff.removed.push(id.clone());
            }
        }
        diff
    }
}

#[derive(Debug, Default)]
pub struct Diff {
    pub regressions: Vec<Change>,
    pub improvements: Vec<Change>,
    pub churn: Vec<Change>,
    pub added: Vec<(String, CaseOutcome)>,
    pub removed: Vec<String>,
}

#[derive(Debug)]
pub struct Change {
    pub id: String,
    pub before: CaseOutcome,
    pub after: CaseOutcome,
}
