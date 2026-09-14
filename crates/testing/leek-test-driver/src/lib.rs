//! Run extracted upstream test cases through the recipe pipeline and backends.

pub mod audit;
pub mod backends;
pub mod checks;
pub mod run;

/// The case data model, which now lives in its own dependency-free crate
/// so a build script can use it without the runner (#150). Re-exported
/// under the old name: `leek_test_driver::cases::TestCase` still resolves.
pub use leek_test_cases as cases;

pub use audit::audit_case;
pub use backends::{
    MultiDiff, MultiReport, SuiteBackend, detect_backends, run_case_backend,
    run_manifest as run_manifest_backends,
};
pub use cases::{CaseAudit, Expectation, Manifest, TestCase};
pub use checks::{CaseChecks, CasePlan, CheckKind};
pub use run::{CaseOutcome, Diff, Report, Summary, run_manifest_all, run_one};
