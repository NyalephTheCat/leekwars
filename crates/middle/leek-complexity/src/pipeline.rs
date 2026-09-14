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
    // NOTE(#428): unlike `LowerHir` / `TypeCheck`, this branch does not
    // check for an include-aware run, so a memoized `Target::Complexity` pipeline
    // built by `pipeline_with_includes` would measure complexity from the entry
    // file alone. Still dormant, but for a narrower reason than it used to be:
    // since #165 the LSP *does* ask for this target, from hover, code lens and
    // `leek.showComplexity` — only ever through `crate::pipeline::run` /
    // `run_on_file`, which build a plain `leek_recipes::pipeline`. Nothing
    // routes `Target::Complexity` through `pipeline_with_includes` yet.
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
    #[cfg(test)]
    salsa_probe::COMPLEXITY_QUERY_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hir = leek_hir::pipeline::lower_hir_query(db, file);
    ComplexityReport(Arc::new(analyze_file(hir.hir.as_ref())))
}

// The salsa cascade tests below are `#[cfg(feature = "salsa")]`, and a
// cfg'd-out test is an absent test, not a passing one. The self
// dev-dependency in `Cargo.toml` turns the feature on for test builds;
// refusing to build the test target without it makes losing that line a
// loud failure rather than the caching proof quietly disappearing. (The
// same trap leek-mir's `lower_mir_query` fell into — see
// `leek_mir::pipeline`.)
#[cfg(all(test, not(feature = "salsa")))]
compile_error!(
    "leek-complexity's test build needs the `salsa` feature — restore the self \
     dev-dependency in crates/middle/leek-complexity/Cargo.toml"
);

#[cfg(all(test, feature = "salsa"))]
mod salsa_probe {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    pub(super) static COMPLEXITY_QUERY_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub(super) static SERIAL: Mutex<()> = Mutex::new(());
}

#[cfg(all(test, feature = "salsa"))]
mod salsa_tests {
    use std::sync::atomic::Ordering;

    use leek_hir::pipeline::LowerHir;
    use leek_lexer::pipeline::Lex;
    use leek_parser::pipeline::Parse;
    use leek_pipeline::Pipeline;
    use leek_pipeline::salsa::{LeekDb, SourceFile};
    use leek_syntax::pipeline::Pragma;
    use salsa::Setter;

    use super::Analyze;
    use super::salsa_probe::{COMPLEXITY_QUERY_CALLS, SERIAL};

    fn source(db: &mut LeekDb, text: &str) -> SourceFile {
        SourceFile::new(db, 1, text.to_string(), 4, false, false, 0, Vec::new())
    }

    fn pipeline() -> Pipeline {
        Pipeline::new()
            .with(Pragma)
            .with(Lex)
            .with(Parse)
            .with(LowerHir::default())
            .with(Analyze)
    }

    #[test]
    fn full_cascade_caches_complexity() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(
            &mut db,
            "function sum(arr) { var t = 0 for (var x in arr) { t = t + x } return t }\n",
        );
        let pipeline = pipeline();

        let before = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let after_first = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let after_second = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);

        assert_eq!(
            after_first - before,
            1,
            "first run executes the analysis once"
        );
        assert_eq!(
            after_second - after_first,
            0,
            "a second identical run must hit the salsa cache — this is what \
             lets the LSP ask for the report on every hover and code lens"
        );
    }

    #[test]
    fn semantic_edit_reruns_complexity() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(&mut db, "function f() { return 1 }\n");
        let pipeline = pipeline();

        let before = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let after_first = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);

        file.set_text(&mut db)
            .to("function f(n) { for (var i = 0; i < n; i++) { } return 1 }\n".to_string());

        let _ = pipeline.run_memoized(&db, file);
        let after_second = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);

        assert_eq!(after_first - before, 1);
        assert_eq!(
            after_second - after_first,
            1,
            "a semantic change must re-execute the analysis"
        );
    }
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
