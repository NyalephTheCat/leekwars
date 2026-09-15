//! Pipeline integration: HIR→MIR lowering as a [`Step`].

use std::sync::Arc;

use leek_hir::pipeline::HirArtifact;
use leek_pipeline::{Artifact, Context, OptLevel, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};

use crate::MirProgram;
use crate::lower::lower_and_optimize;

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
///
/// The work itself is [`lower_and_optimize`]; this step only moves
/// artifacts and diagnostics in and out of the [`Context`].
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
    // NOTE(#428): unlike `LowerHir` / `TypeCheck`, this branch does not
    // check for an include-aware run, so a memoized `Target::Mir` pipeline
    // built by `pipeline_with_includes` would lower MIR from the entry
    // file alone. Dormant: the only `run_memoized` caller is the LSP, which
    // never asks for this target.
    //
    // The answer this *should* be asking for is
    // `leek_db::queries::lower_program_mir`, which lowers the closure's
    // merged HIR. It cannot be called from here: a tracked whole-program
    // query needs `WorkspaceFiles`, and a `Context` carries only a
    // `SourceFile`. Guarding on `IncludeGraphArtifact` the way `LowerHir`
    // does would fix it within this model, at the cost of a `leek-resolver`
    // dependency added to code epic #345 deletes. So it stays dormant, and
    // goes when this step does.
    if let Some((db, file)) = cx.salsa() {
        // The MIR query is keyed only on the source file, so it caches
        // *unoptimized* MIR: a codegen driver's program would have to be
        // cloned out of the cached `Arc` before the passes could run on it.
        // Lower it from the (still memoized) HIR instead — same work as the
        // clone-and-optimize it replaces, minus the clone.
        if opt.optimizes() {
            let hir = leek_hir::pipeline::lower_hir_query(db, file);
            let (program, diags) = lower_and_optimize(hir.hir.as_ref(), opt);
            cx.emit_all(diags);
            return Some(Arc::new(program));
        }
        let out = lower_mir_query(db, file);
        cx.emit_all(out.diagnostics.iter().cloned());
        return Some(out.program);
    }
    let hir = cx.get::<HirArtifact>()?;
    let (program, diags) = lower_and_optimize(hir.0.as_ref(), opt);
    cx.emit_all(diags);
    Some(Arc::new(program))
}

/// Tracked return: MIR program plus lowering diagnostics.
#[derive(salsa::Update, Debug, Clone, PartialEq)]
pub struct LowerMirQueryResult {
    pub program: Arc<MirProgram>,
    pub diagnostics: Vec<leek_diagnostics::Diagnostic>,
}

/// Tracked return: `Arc<MirProgram>` newtype, salsa-friendly.
#[derive(salsa::Update, Debug, Clone, PartialEq)]
pub struct LoweredMir(pub Arc<MirProgram>);

/// Salsa-tracked entry point. Re-runs only when
/// [`lower_hir_query`](leek_hir::pipeline::lower_hir_query)'s HIR
/// changes.
#[salsa::tracked]
pub fn lower_mir_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> LowerMirQueryResult {
    #[cfg(test)]
    salsa_probe::LOWER_MIR_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hir = leek_hir::pipeline::lower_hir_query(db, file);
    // O0: the cached program is the one analysis drivers read, and a
    // codegen driver optimizes its own copy (see `run_lower_mir`).
    let (program, diagnostics) = lower_and_optimize(hir.hir.as_ref(), OptLevel::O0);
    LowerMirQueryResult {
        program: Arc::new(program),
        diagnostics,
    }
}

#[cfg(test)]
mod salsa_probe {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    pub(super) static LOWER_MIR_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub(super) static SERIAL: Mutex<()> = Mutex::new(());
}

#[cfg(test)]
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
        SourceFile::new(db, String::new(), 1, text.into(), 4, false, false, 0)
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

        file.set_text(&mut db).to("var y = 6;".into());

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
