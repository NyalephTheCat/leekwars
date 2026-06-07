//! [`RustNative`] — run the program via the native (Cranelift) backend.
//!
//! The native backend JIT-compiles and runs in one step (it doesn't expose a
//! reusable compiled artifact), so **every** timed run includes JIT
//! compilation. For compute-heavy programs execution dominates that fixed
//! per-run cost; for tiny programs the JIT compile is a meaningful fraction.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use leek_backend_native::NativeOptions;

use crate::{compile_hir_file, Backend, BenchOptions, RunResult};

pub struct RustNative {
    hir: Option<std::sync::Arc<leek_hir::HirFile>>,
    version: u8,
}

impl RustNative {
    pub fn new() -> Self {
        Self {
            hir: None,
            version: 4,
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
        let compiled = compile_hir_file(source, opts.version)
            .with_context(|| format!("compiling {}", source.display()))?;
        self.hir = Some(compiled.hir);
        self.version = opts.version;
        Ok(())
    }
    fn bench_runs(&mut self, runs: usize) -> Result<Vec<RunResult>> {
        let hir = self
            .hir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not prepared"))?;
        let opts = NativeOptions::release().with_lang(self.version, false);
        let mut out = Vec::with_capacity(runs);
        for _ in 0..runs {
            let t0 = Instant::now();
            let value = leek_backend_native::run(hir.as_ref(), &opts)
                .map_err(|e| anyhow::anyhow!("native run: {e}"))?;
            let elapsed = t0.elapsed();
            out.push(RunResult {
                elapsed,
                stdout: value.to_string(),
            });
        }
        Ok(out)
    }
}
