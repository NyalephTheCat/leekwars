//! [`RustInterp`] — run the program via [`leek_backend_interp`] in-process.
//!
//! Uses the shared [`compile_hir`](crate::compile_hir) pipeline so per-stage
//! durations flow into [`BenchSummary::prepare_steps`].

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::{Backend, BenchOptions, RunResult, compile_hir_file};

pub struct RustInterp {
    hir: Option<std::sync::Arc<leek_hir::HirFile>>,
    version: u8,
    strict: bool,
    steps: Vec<(String, Duration)>,
}

impl RustInterp {
    pub fn new() -> Self {
        Self {
            hir: None,
            version: 4,
            strict: false,
            steps: Vec::new(),
        }
    }
}

impl Default for RustInterp {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for RustInterp {
    fn name(&self) -> &'static str {
        "rust-interp"
    }
    fn prepare(&mut self, source: &Path, opts: &BenchOptions) -> Result<()> {
        let compiled = compile_hir_file(source, opts.version)
            .with_context(|| format!("compiling {}", source.display()))?;
        self.hir = Some(compiled.hir);
        self.version = opts.version;
        self.strict = opts.strict;
        self.steps = compiled.steps;
        Ok(())
    }
    fn bench_runs(&mut self, runs: usize) -> Result<Vec<RunResult>> {
        let hir = self
            .hir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not prepared"))?;
        let mut out = Vec::with_capacity(runs);
        for _ in 0..runs {
            let t0 = Instant::now();
            let r = leek_backend_interp::run_with_limit_version_strict(
                hir.as_ref(),
                200_000_000,
                self.version,
                self.strict,
            );
            let elapsed = t0.elapsed();
            if let Some(err) = r.error {
                anyhow::bail!("runtime error: {err}");
            }
            out.push(RunResult {
                elapsed,
                stdout: r.value.to_string(),
            });
        }
        Ok(out)
    }
    fn prepare_steps(&self) -> Vec<(String, Duration)> {
        self.steps.clone()
    }
}
