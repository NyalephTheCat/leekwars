//! Pipeline integration: complexity analysis as a [`Step`].
//!
//! Exposes the analysis through `leek-pipeline` so tools plan it via
//! recipes (and reuse salsa-cached HIR) instead of calling
//! [`analyze_file`](crate::analyze_file) directly. The step reads the
//! canonical [`HirArtifact`] and contributes a [`ComplexityArtifact`].

use std::sync::Arc;

use leek_hir::pipeline::HirArtifact;
use leek_pipeline::{Artifact, Context, RecipeArtifact, RecipeParams, RecipeStep, Step, StepError};

use crate::Complexity;
use crate::analyze::analyze_file;

/// Per-function / per-method complexity report for one file.
///
/// Held by `Arc` so the salsa cache hit stays pointer-cheap and
/// downstream consumers can clone the handle freely.
#[derive(Debug, Clone)]
pub struct ComplexityArtifact(pub Arc<Vec<Complexity>>);
impl Artifact for ComplexityArtifact {}

/// Run static complexity analysis over the lowered HIR.
#[derive(Default)]
pub struct Analyze;

impl Step for Analyze {
    fn name(&self) -> &'static str {
        "complexity"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        if let Some(report) = run_analyze(cx) {
            cx.insert(ComplexityArtifact(report));
        }
        Ok(())
    }
}

impl RecipeStep for Analyze {
    fn build(_params: &RecipeParams) -> Box<dyn Step> {
        Box::new(Analyze)
    }
}

impl RecipeArtifact for ComplexityArtifact {
    type Producer = Analyze;
    type Requires = (HirArtifact,);
    type Produces = (ComplexityArtifact,);
}

fn run_analyze(cx: &Context<'_>) -> Option<Arc<Vec<Complexity>>> {
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        return Some(complexity_query(db, file).0);
    }
    let hir = cx.get::<HirArtifact>()?;
    Some(Arc::new(analyze_file(hir.0.as_ref())))
}

/// Tracked return type — newtype over `Arc<Vec<Complexity>>` so the
/// salsa query has a single `Update`-able return.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct ComplexityReport(pub Arc<Vec<Complexity>>);

/// Salsa-tracked entry point. Re-runs only when
/// [`lower_hir_query`](leek_hir::pipeline::lower_hir_query)'s HIR
/// changes.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn complexity_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> ComplexityReport {
    let hir = leek_hir::pipeline::lower_hir_query(db, file);
    ComplexityReport(Arc::new(analyze_file(hir.hir.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::ComplexityArtifact;
    use leek_hir::pipeline::LowerHir;
    use leek_lexer::pipeline::Lex;
    use leek_parser::pipeline::Parse;
    use leek_pipeline::{FeatureFlags, Input, Pipeline};
    use leek_span::SourceId;
    use leek_syntax::pipeline::Pragma;

    fn input(text: &str) -> Input {
        Input {
            source: SourceId::new(1).unwrap(),
            text: text.into(),
            version_byte: 4,
            strict: false,
            flags: FeatureFlags::none(),
        }
    }

    #[test]
    fn analyze_step_contributes_a_complexity_artifact() {
        // The non-salsa Step path: a plain pipeline lowers to HIR and
        // the `Analyze` step turns it into a `ComplexityArtifact`.
        let pipeline = Pipeline::new()
            .with(Pragma)
            .with(Lex)
            .with(Parse)
            .with(LowerHir::default())
            .with(super::Analyze);
        let run = pipeline.run(input(
            "function sum(arr) {\n  var t = 0\n  for (var x in arr) { t = t + x }\n  return t\n}\n",
        ));
        let report = run
            .get::<ComplexityArtifact>()
            .expect("complexity artifact present");
        let sum = report
            .0
            .iter()
            .find(|c| c.name == "sum")
            .expect("sum analysed");
        assert!(
            matches!(sum.big_o, crate::BigO::Linear(ref v) if v.name == "arr"),
            "got {:?}",
            sum.big_o
        );
    }
}
