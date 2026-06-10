//! [`RustNative`] — run the program via the native (Cranelift) backend.
//!
//! The native backend JIT-compiles and runs in one step (it doesn't expose a
//! reusable compiled artifact), so **every** timed run includes JIT
//! compilation. For compute-heavy programs execution dominates that fixed
//! per-run cost; for tiny programs the JIT compile is a meaningful fraction.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use leek_backend_native::NativeOptions;

use crate::{Backend, BenchOptions, RunResult, compile_hir_file};

pub struct RustNative {
    hir: Option<std::sync::Arc<leek_hir::HirFile>>,
    version: u8,
    strict: bool,
    /// Per-run JIT-compile durations captured from the backend, sorted after
    /// the run loop so the median can be reported alongside execution.
    compile_samples: Vec<Duration>,
    /// Per-run execution durations (the JIT'd `main` call only).
    exec_samples: Vec<Duration>,
}

impl RustNative {
    pub fn new() -> Self {
        Self {
            hir: None,
            version: 4,
            strict: false,
            compile_samples: Vec::new(),
            exec_samples: Vec::new(),
        }
    }
}

impl Default for RustNative {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for RustNative {
    fn name(&self) -> &'static str {
        "rust-native"
    }
    fn prepare(&mut self, source: &Path, opts: &BenchOptions) -> Result<()> {
        let compiled = compile_hir_file(source, opts.version, opts.strict)
            .with_context(|| format!("compiling {}", source.display()))?;
        self.hir = Some(compiled.hir);
        self.version = opts.version;
        self.strict = opts.strict;
        Ok(())
    }
    fn bench_runs(&mut self, runs: usize) -> Result<Vec<RunResult>> {
        let hir = self
            .hir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not prepared"))?;
        let opts = NativeOptions::release().with_lang(self.version, self.strict);
        let mut out = Vec::with_capacity(runs);
        self.compile_samples.clear();
        self.exec_samples.clear();
        for _ in 0..runs {
            let t0 = Instant::now();
            let value = leek_backend_native::run(hir.as_ref(), &opts)
                .map_err(|e| anyhow::anyhow!("native run: {e}"))?;
            let elapsed = t0.elapsed();
            // The backend records how much of this run was JIT compilation vs
            // executing the compiled `main`, so we can report them separately.
            if let Some((compile, exec)) = leek_backend_native::last_jit_split() {
                self.compile_samples.push(compile);
                self.exec_samples.push(exec);
            }
            out.push(RunResult {
                elapsed,
                stdout: value.to_string(),
            });
        }
        Ok(out)
    }

    fn prepare_steps(&self) -> Vec<(String, Duration)> {
        // Report the warm medians (runs 2..N) so the figures match the
        // warm-median execution column and exclude the cold first run.
        let median = |samples: &[Duration]| -> Option<Duration> {
            let mut warm: Vec<Duration> = samples.iter().skip(1).copied().collect();
            if warm.is_empty() {
                return samples.first().copied();
            }
            warm.sort();
            Some(warm[warm.len() / 2])
        };
        let mut steps = Vec::new();
        if let Some(c) = median(&self.compile_samples) {
            steps.push(("jit-compile".to_string(), c));
        }
        if let Some(e) = median(&self.exec_samples) {
            steps.push(("execute".to_string(), e));
        }
        steps
    }
}
