//! Run the frontend on a case and record compile-time outcomes.

use leek_diagnostics::Severity;
use leek_project::Input;
use leek_span::SourceId;

use crate::cases::{CaseAudit, TestCase};

/// Lower a case to HIR and snapshot its diagnostics.
///
/// Through the tracked queries: `diagnostics_without_lints` is the
/// permissive frontend stream this used to read off a `Target::Hir` run,
/// and it composes the same passes in the same order.
///
/// A case is one self-contained snippet with no `include`, so the per-file
/// queries answer it — no workspace file set, no closure.
pub fn audit_case(case: &TestCase, source: SourceId) -> CaseAudit {
    let input = Input {
        source,
        text: case.code.clone().into(),
        version_byte: case.version,
        strict: case.strict,
        flags: leek_span::FeatureFlags::from_env(),
    };

    let db = leek_db::LeekDb::default();
    let file = leek_db::input_file(&db, String::new(), &input);

    let mut audit = CaseAudit::default();
    for d in leek_db::queries::diagnostics_without_lints(&db, file).iter() {
        match d.severity {
            Severity::Error => audit.compile_errors += 1,
            Severity::Warning => audit.compile_warnings += 1,
            _ => {}
        }
    }
    // Faithfully `true`, which is what it always was here rather than a
    // simplification introduced by the move. The old check asked whether
    // the run inserted `HirArtifact`; `Parse` always inserts an
    // `AstArtifact` (its root cast cannot fail), `LowerHir` skips only
    // when that is absent, and the permissive params this used never
    // aborted the run — so on this path the artifact was always there.
    //
    // Left as a field rather than deleted because `CaseAudit` is a
    // serialized record shared with the corpus manifests. Tempting to
    // redefine it as "lowered something non-empty", but that would change
    // what a recorded audit means without anything asking for it.
    audit.hir_built = true;
    audit
}
