//! Which axes each upstream case should be checked on (per backend).

use serde::{Deserialize, Serialize};

use crate::cases::{Expectation, TestCase};

/// Pipeline / backend axis to verify for a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Parse,
    Resolve,
    Typecheck,
    Hir,
    InterpRun,
    InterpValue,
    InterpOps,
    JavaEmit,
    JavaCompile,
    /// Native (Cranelift) JIT value check.
    NativeRun,
}

impl CheckKind {
    const fn order(self) -> u8 {
        match self {
            Self::Parse => 0,
            Self::Resolve => 1,
            Self::Typecheck => 2,
            Self::Hir => 3,
            Self::InterpRun => 4,
            Self::InterpValue => 5,
            Self::InterpOps => 6,
            Self::JavaEmit => 7,
            Self::JavaCompile => 8,
            Self::NativeRun => 9,
        }
    }
}

/// Check plan derived from upstream `Expectation` and case metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasePlan {
    pub kinds: Vec<CheckKind>,
}

/// Check-plan policy for an upstream case.
///
/// An extension trait rather than an inherent `impl`: [`TestCase`] lives
/// in the dependency-free `leek-test-cases` crate (#150), while *which*
/// backends a case exercises is this crate's policy and depends on the
/// backends it runs. Bring it into scope to call [`Self::check_plan`].
pub trait CaseChecks {
    /// Backends and pipeline stages that should be exercised for this case.
    fn check_plan(&self) -> CasePlan;

    /// Whether the interpreter must compare a value for this case.
    fn needs_interp_value_check(&self) -> bool;

    /// Whether the interpreter must compare an operation count.
    fn needs_interp_ops_check(&self) -> bool;
}

impl CaseChecks for TestCase {
    fn check_plan(&self) -> CasePlan {
        if !self.enabled {
            return CasePlan { kinds: vec![] };
        }
        let mut kinds = vec![
            CheckKind::Parse,
            CheckKind::Resolve,
            CheckKind::Typecheck,
            CheckKind::Hir,
        ];
        match &self.expected {
            Expectation::Equals { .. } | Expectation::Almost { .. } => {
                kinds.extend([
                    CheckKind::InterpRun,
                    CheckKind::InterpValue,
                    CheckKind::JavaEmit,
                    CheckKind::NativeRun,
                ]);
            }
            Expectation::EqualsOps { .. } => {
                kinds.extend([
                    CheckKind::InterpRun,
                    CheckKind::InterpValue,
                    CheckKind::InterpOps,
                    CheckKind::JavaEmit,
                    // `equalsOps` carries a value the native backend can verify
                    // (it just can't count operations).
                    CheckKind::NativeRun,
                ]);
            }
            Expectation::Ops { .. } => {
                kinds.extend([
                    CheckKind::InterpRun,
                    CheckKind::InterpValue,
                    CheckKind::InterpOps,
                    CheckKind::JavaEmit,
                    // Native charges ops at the same MIR sites as the interp,
                    // so it can verify the operation count too.
                    CheckKind::NativeRun,
                ]);
            }
            Expectation::Error { code } if code == "NONE" => {
                // No error expected → native must also compile + run it
                // cleanly (it shares the frontend that produces errors).
                kinds.extend([
                    CheckKind::InterpRun,
                    CheckKind::JavaEmit,
                    CheckKind::NativeRun,
                ]);
            }
            Expectation::Error { .. }
            | Expectation::AnyError
            | Expectation::Warning { .. }
            | Expectation::NoWarning => {
                // Native participates in structural (error/warning) cases too:
                // compile errors are produced by the shared frontend native
                // depends on, and warning/clean cases must run cleanly on it.
                kinds.extend([
                    CheckKind::InterpRun,
                    CheckKind::JavaEmit,
                    CheckKind::NativeRun,
                ]);
            }
            // Legacy manifest rows store `.equalsOps(...)` as `unknown`; they
            // still carry a verifiable value, so let native check it.
            Expectation::Unknown { detail } if detail == "equalsOps" => {
                kinds.push(CheckKind::NativeRun);
            }
            Expectation::Unknown { .. } => {}
        }
        kinds.sort_by_key(|k| k.order());
        CasePlan { kinds }
    }

    fn needs_interp_value_check(&self) -> bool {
        self.enabled
            && matches!(
                self.expected,
                Expectation::Equals { .. } | Expectation::Almost { .. }
            )
    }

    fn needs_interp_ops_check(&self) -> bool {
        self.enabled && matches!(self.expected, Expectation::Ops { .. })
    }
}
