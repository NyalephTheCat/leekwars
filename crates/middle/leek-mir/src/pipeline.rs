//! Pipeline integration: HIR→MIR lowering as a [`Step`].

use std::sync::Arc;

use leek_hir::pipeline::HirArtifact;
use leek_pipeline::{Artifact, Context, OptLevel, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};

use crate::MirProgram;
use crate::lower::lower_file;

/// Lowered MIR program. Held by [`Arc`] so the salsa cache hit stays
/// pointer-cheap.
#[derive(Debug, Clone)]
pub struct MirArtifact(pub Arc<MirProgram>);
impl Artifact for MirArtifact {}

/// HIR → MIR lowering. Requires a prior [`leek_hir::pipeline::LowerHir`].
/// Lowering diagnostics are emitted into the pipeline context (and returned
/// from [`lower_mir_query`] on the salsa path).
///
/// `opt` controls whether the backend-agnostic MIR passes
/// ([`crate::optimize_program`]) run after lowering — codegen drivers request
/// [`OptLevel::O1`]; analysis drivers keep the IR shape unchanged.
pub struct LowerMir {
    opt: OptLevel,
}

impl LowerMir {
    /// A lowering step at the given [`OptLevel`]. For manual `.with(...)`
    /// composition; recipes build it from [`RecipeParams::opt`].
    #[must_use]
    pub fn new(opt: OptLevel) -> Self {
        Self { opt }
    }
}

impl Default for LowerMir {
    fn default() -> Self {
        Self { opt: OptLevel::O0 }
    }
}

impl Step for LowerMir {
    fn name(&self) -> &'static str {
        "lower-mir"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        if let Some(out) = run_lower_mir(cx, self.opt) {
            cx.insert(MirArtifact(out));
        }
        Ok(())
    }
}

impl RecipeStep for LowerMir {
    fn build(params: &RecipeParams) -> Box<dyn leek_pipeline::Step> {
        Box::new(LowerMir { opt: params.opt })
    }
}

impl RecipeArtifact for MirArtifact {
    type Producer = LowerMir;
    type Requires = (HirArtifact,);
    type Produces = (MirArtifact,);
}

fn run_lower_mir(cx: &mut Context<'_>, opt: OptLevel) -> Option<Arc<MirProgram>> {
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        let out = lower_mir_query(db, file);
        cx.emit_all(out.diagnostics.iter().cloned());
        // The salsa query is keyed only on the source file, so it caches
        // unoptimized MIR. Optimize outside the cache when a codegen driver
        // asked for it.
        if opt.optimizes() {
            let mut program = (*out.program).clone();
            crate::optimize_program(&mut program);
            return Some(Arc::new(program));
        }
        return Some(out.program);
    }
    let hir = cx.get::<HirArtifact>()?;
    let (mut program, diags) = lower_file(hir.0.as_ref());
    cx.emit_all(diags);
    if opt.optimizes() {
        crate::optimize_program(&mut program);
    }
    Some(Arc::new(program))
}

/// Tracked return: MIR program plus lowering diagnostics.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct LowerMirQueryResult {
    pub program: Arc<MirProgram>,
    pub diagnostics: Vec<leek_diagnostics::Diagnostic>,
}

/// Tracked return: `Arc<MirProgram>` newtype, salsa-friendly.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct LoweredMir(pub Arc<MirProgram>);

/// Salsa-tracked entry point. Re-runs only when
/// [`lower_hir_query`](leek_hir::pipeline::lower_hir_query)'s HIR
/// changes.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn lower_mir_query<'db>(
    db: &'db dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> LowerMirQueryResult {
    #[cfg(test)]
    salsa_probe::LOWER_MIR_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hir = leek_hir::pipeline::lower_hir_query(db, file);
    let (program, diagnostics) = lower_file(hir.hir.as_ref());
    LowerMirQueryResult {
        program: Arc::new(program),
        diagnostics,
    }
}

#[cfg(all(test, feature = "salsa"))]
mod salsa_probe {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    pub(super) static LOWER_MIR_CALLS: AtomicUsize = AtomicUsize::new(0);
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

    use super::LowerMir;
    use super::salsa_probe::{LOWER_MIR_CALLS, SERIAL};

    fn source(db: &mut LeekDb, text: &str) -> SourceFile {
        SourceFile::new(db, 1, text.to_string(), 4, false, 0, Vec::new())
    }

    #[test]
    fn full_cascade_caches_mir() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(
            &mut db,
            "function add(a, b) { return a + b; }\nvar x = add(1, 2);\n",
        );
        let pipeline = Pipeline::new()
            .with(Pragma)
            .with(Lex)
            .with(Parse)
            .with(LowerHir::default())
            .with(LowerMir::default());

        let before = LOWER_MIR_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let after_first = LOWER_MIR_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let after_second = LOWER_MIR_CALLS.load(Ordering::Relaxed);

        assert_eq!(after_first - before, 1, "first run executes lower_mir once");
        assert_eq!(
            after_second - after_first,
            0,
            "second identical run must hit the salsa cache all the way through"
        );
    }

    #[test]
    fn semantic_edit_reruns_mir() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(&mut db, "var x = 5;");
        let pipeline = Pipeline::new()
            .with(Pragma)
            .with(Lex)
            .with(Parse)
            .with(LowerHir::default())
            .with(LowerMir::default());

        let before = LOWER_MIR_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let after_first = LOWER_MIR_CALLS.load(Ordering::Relaxed);

        file.set_text(&mut db).to("var y = 6;".to_string());

        let _ = pipeline.run_memoized(&db, file);
        let after_second = LOWER_MIR_CALLS.load(Ordering::Relaxed);

        assert_eq!(after_first - before, 1);
        assert_eq!(
            after_second - after_first,
            1,
            "semantic change must re-execute"
        );
    }
}
