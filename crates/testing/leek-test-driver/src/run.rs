//! Run extracted upstream cases through the recipe pipeline and
//! classify outcomes against upstream expectations.

use leek_span::SourceId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
