//! Shared front/middle-end pipeline used by benchmark backends.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use leek_hir::HirFile;
use leek_project::Input;
use leek_query::TimingSink;
use leek_session::OptLevel;

/// HIR plus per-stage prepare timings.
pub struct CompiledHir {
    pub hir: Arc<HirFile>,
    pub steps: Vec<(String, Duration)>,
}

/// Lower `input` to optimized HIR, timing the stages it goes through.
///
/// Through the tracked queries rather than a planned pipeline, so the
/// benchmark measures what the toolchain actually executes — which is what
/// it has always claimed to measure, and what changed under it when
/// `Compilation` moved off the run.
///
/// The timings are per *stage* now rather than per pipeline step, because
/// there are no steps: `lower_hir_query` calls `parse_query` calls
/// `lex_query`, and salsa reports which of them recomputed (`WillExecute`)
/// but never how long one took. So the breakdown is the two numbers this
/// level can honestly measure — the frontend's diagnostics and the
/// lowering — instead of a step list that no longer exists.
///
/// Returns the diagnostics alongside, since there is no `Run` to ask.
pub fn compile_hir(input: &Input) -> Result<(CompiledHir, Vec<leek_diagnostics::Diagnostic>)> {
    let db = leek_db::LeekDb::default();
    let file = leek_db::input_file(&db, String::new(), input);

    let sink = TimingSink::new();
    let diagnostics = sink.time("diagnostics", || {
        leek_db::queries::diagnostics_without_lints(&db, file)
            .as_ref()
            .clone()
    });
    // `O1` like the real codegen drivers (`miku run`, native), so this
    // measures the tree users execute rather than an unoptimized one.
    let hir = sink.time("lower-hir", || {
        let lowered = leek_hir::pipeline::lower_hir_query(&db, file);
        // The query caches unoptimized HIR (it is keyed on the file, not on
        // an opt level), so the codegen drivers' `O1` is applied here —
        // exactly as `leek_hir::pipeline`'s own `run_lower` does.
        leek_hir::lower::finish(
            lowered.hir.as_ref().clone(),
            &leek_hir::fold::fold_map(leek_prelude::active_fold_set()),
            OptLevel::O1,
        )
    });

    let steps = sink
        .entries()
        .into_iter()
        .map(|t| (t.step.to_string(), t.duration))
        .collect();
    Ok((CompiledHir { hir, steps }, diagnostics))
}

/// Convenience: read a `.leek` file and compile to HIR.
pub fn compile_hir_file(
    path: &std::path::Path,
    version_byte: u8,
    strict: bool,
) -> Result<CompiledHir> {
    use leek_span::SourceId;

    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let src_id = SourceId::new(1).unwrap();
    let (compiled, diagnostics) = compile_hir(&Input {
        source: src_id,
        text: text.into(),
        version_byte,
        strict,
        flags: leek_span::FeatureFlags::from_env(),
    })?;
    // The permissive pipeline still produces HIR alongside error diagnostics
    // (error-tolerant lowering). Executing that HIR benchmarks garbage — an
    // unsupported literal lowers to a poison value and can even fault (e.g.
    // `% <error>` → SIGFPE in the JIT) — so treat frontend errors as a
    // compile failure, like the JVM backends do when javac rejects a case.
    let n_errors = diagnostics
        .iter()
        .filter(|d| matches!(d.severity, leek_diagnostics::Severity::Error))
        .count();
    if n_errors > 0 {
        anyhow::bail!("{n_errors} frontend error(s)");
    }
    Ok(compiled)
}
