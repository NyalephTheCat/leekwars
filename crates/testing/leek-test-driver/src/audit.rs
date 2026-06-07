//! Run the recipe pipeline on a case and record compile-time outcomes.

use leek_diagnostics::Severity;
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target};
use leek_span::SourceId;

use crate::cases::{CaseAudit, TestCase};

/// Run the standard permissive pipeline to HIR and snapshot diagnostics.
pub fn audit_case(case: &TestCase, source: SourceId) -> CaseAudit {
    let input = Input {
        source,
        text: case.code.clone().into(),
        version_byte: case.version,
        strict: case.strict,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let Ok(pipeline) = leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive()) else {
        return CaseAudit::default();
    };
    let run = pipeline.run(input);
    let mut audit = CaseAudit::default();
    for d in run.diagnostics() {
        match d.severity {
            Severity::Error => audit.compile_errors += 1,
            Severity::Warning => audit.compile_warnings += 1,
            _ => {}
        }
    }
    audit.hir_built = run.get::<HirArtifact>().is_some();
    audit
}
