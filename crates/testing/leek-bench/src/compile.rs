//! Shared front/middle-end pipeline used by benchmark backends.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::{Input, Run, TimingSink};
use leek_recipes::{OptLevel, RecipeParams, Target};

/// HIR plus per-step prepare timings from a standard compile pipeline.
pub struct CompiledHir {
    pub hir: Arc<HirFile>,
    pub steps: Vec<(String, Duration)>,
}

/// Run pragma → lex → parse → resolve → typecheck → lower-hir with timing.
pub fn compile_hir(input: Input) -> Result<(CompiledHir, Run<'static>)> {
    let sink = TimingSink::new();
    // Optimize like the real codegen drivers (`miku run`, native) so the
    // benchmark measures the pipeline users actually execute, not an
    // unoptimized one.
    let params = RecipeParams::permissive().with_opt(OptLevel::O1);
    let pipeline = leek_recipes::pipeline_timed(Target::Hir, &params, &sink)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let run = pipeline.run(input);
    let hir = run
        .get::<HirArtifact>()
        .map(|h| h.0.clone())
        .ok_or_else(|| anyhow::anyhow!("HIR not produced (parse failed?)"))?;
    let steps = sink
        .entries()
        .into_iter()
        .map(|t| (t.step.to_string(), t.duration))
        .collect();
    Ok((CompiledHir { hir, steps }, run))
}

/// Convenience: read a `.leek` file and compile to HIR.
pub fn compile_hir_file(path: &std::path::Path, version_byte: u8, strict: bool) -> Result<CompiledHir> {
    use leek_span::SourceId;

    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let src_id = SourceId::new(1).unwrap();
    let (compiled, _run) = compile_hir(Input {
        source: src_id,
        text: text.into(),
        version_byte,
        strict,
        flags: leek_pipeline::FeatureFlags::from_env(),
    })?;
    Ok(compiled)
}
