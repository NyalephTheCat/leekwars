//! Per-backend benchmark harness for Leekscript.
//!
//! Three backends are compared:
//!
//! - [`RustInterp`] — runs the program via [`leek_backend_interp`] in-process.
//! - [`RustJavaEmit`] — emits Java with our backend, compiles with
//!   `javac`, runs with `java`. Cold runs include javac.
//! - [`UpstreamJava`] — invokes the upstream reference's
//!   `leekscript.TopLevel` driver against the same source. Requires
//!   the upstream classes + their gradle-cached deps to be present.
//!
//! Each [`Backend`] reports a cold time (first run, includes any
//! one-shot cost) and a warm median (runs 2..N). For the JVM
//! backends, warm runs still spawn a fresh JVM each iteration — the
//! "warm" label refers to amortised per-iteration cost rather than a
//! JIT-warmed JVM. A true warm-JIT measurement would need a
//! long-lived JVM harness; that's planned but not done here.
//!
//! Usage:
//!
//! ```no_run
//! use leek_bench::{bench, BenchOptions, RustInterp, RustJavaEmit, UpstreamJava};
//! use std::path::Path;
//!
//! let src = Path::new("fixtures/knapsack.leek");
//! let opts = BenchOptions { runs: 10, version: 4 };
//!
//! for mut b in [
//!     Box::new(RustInterp::new()) as Box<dyn leek_bench::Backend>,
//!     Box::new(RustJavaEmit::auto()),
//!     Box::new(UpstreamJava::auto()),
//! ] {
//!     match bench(&mut *b, src, &opts) {
//!         Ok(s)  => println!("{}: cold={:?} warm_med={:?}", s.backend, s.cold, s.warm_median),
//!         Err(e) => eprintln!("{} skipped: {e}", b.name()),
//!     }
//! }
//! ```

mod compile;
mod runner_interp;
mod runner_native;
mod runner_rust_java;
mod runner_upstream;

use std::path::Path;
use std::time::Duration;

pub use compile::{CompiledHir, compile_hir, compile_hir_file};

pub use runner_interp::RustInterp;
pub use runner_native::RustNative;
pub use runner_rust_java::RustJavaEmit;
pub use runner_upstream::UpstreamJava;

/// Per-run output of a backend.
pub struct RunResult {
    pub elapsed: Duration,
    pub stdout: String,
}

/// Tuning for [`bench`].
#[derive(Debug, Clone, Copy)]
pub struct BenchOptions {
    /// Number of runs per backend. Run 1 is the cold sample; runs
    /// 2..N feed the warm median. Minimum 1.
    pub runs: usize,
    /// Leekscript version to compile/run with (1..=4).
    pub version: u8,
    /// Strict mode — typed-assignment coercion (`var a = 5.5; a = 2` stores
    /// `2.0`). Must match the upstream case's mode or value-agreement checks
    /// on conversion-sensitive programs spuriously fail.
    pub strict: bool,
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            runs: 5,
            version: 4,
            strict: false,
        }
    }
}

/// Aggregated result of benchmarking one backend.
pub struct BenchSummary {
    pub backend: &'static str,
    pub cold: Duration,
    pub warm_median: Duration,
    /// All `runs - 1` warm samples, sorted ascending. Use the first
    /// entry as min and `samples[len * 95 / 100]` as p95.
    pub warm_runs: Vec<Duration>,
    /// First-run stdout, truncated to a short sample.
    pub stdout_sample: String,
    /// Per-step preparation timings, populated only by backends that
    /// can expose them (today: [`RustInterp`]). Pairs of
    /// `(step_name, duration)` in pipeline execution order.
    pub prepare_steps: Vec<(String, Duration)>,
}

/// A runnable backend.
pub trait Backend {
    fn name(&self) -> &'static str;
    /// One-time setup before [`bench_runs`](Self::bench_runs). Returns
    /// `Err` to skip the backend (missing toolchain, etc.).
    fn prepare(&mut self, source: &Path, opts: &BenchOptions) -> anyhow::Result<()>;
    /// Produce `runs` timed samples. Backends that benefit (the
    /// JVM ones) loop *inside* a single host process so JVM cold-
    /// start and JIT warm-up are paid once. In-process backends
    /// just loop `runs` times in Rust.
    fn bench_runs(&mut self, runs: usize) -> anyhow::Result<Vec<RunResult>>;
    /// Per-stage timings captured during `prepare`. Default: empty.
    fn prepare_steps(&self) -> Vec<(String, Duration)> {
        Vec::new()
    }
}

/// Drive `backend` against `source` and aggregate the timings.
pub fn bench<B: Backend + ?Sized>(
    backend: &mut B,
    source: &Path,
    opts: &BenchOptions,
) -> anyhow::Result<BenchSummary> {
    backend.prepare(source, opts)?;
    let runs = opts.runs.max(1);
    let all = backend.bench_runs(runs)?;
    if all.is_empty() {
        anyhow::bail!("backend returned 0 runs");
    }
    let cold = all[0].elapsed;
    let mut warm: Vec<Duration> = all.iter().skip(1).map(|r| r.elapsed).collect();
    warm.sort();
    let warm_median = if warm.is_empty() {
        cold
    } else {
        warm[warm.len() / 2]
    };
    // Keep the FULL program output — callers compare it against the expected
    // value for the agreement check, and display sites truncate it themselves.
    // Clipping here silently failed the corpus agreement check on every long
    // result (arrays, …): a truncated sample never equals the full expected
    // string.
    let sample = all[0].stdout.clone();
    Ok(BenchSummary {
        backend: backend.name(),
        cold,
        warm_median,
        warm_runs: warm,
        stdout_sample: sample,
        prepare_steps: backend.prepare_steps(),
    })
}
