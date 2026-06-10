//! Run extracted upstream test cases through the recipe pipeline and backends.

pub mod audit;
pub mod backends;
pub mod cases;
pub mod checks;
pub mod run;

pub use audit::audit_case;
pub use backends::{
    MultiDiff, MultiReport, SuiteBackend, detect_backends, run_case_backend,
    run_manifest as run_manifest_backends,
};
pub use cases::{CaseAudit, Expectation, Manifest, TestCase};
pub use checks::{CasePlan, CheckKind};
pub use run::{CaseOutcome, Diff, Report, Summary, run_manifest_all, run_one};
