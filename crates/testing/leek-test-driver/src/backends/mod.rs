//! Run upstream cases on every linked / enabled backend.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use leek_backends::version_from_byte;
use leek_diagnostics::Severity;
use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_manifest::{BackendKind, BackendTable};
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target};
use leek_span::SourceId;
use serde::{Deserialize, Serialize};

use crate::cases::{Expectation, Manifest, TestCase};
use crate::checks::{CaseChecks, CheckKind};
use crate::run::CaseOutcome;

/// Corpus runner target — includes the shared pipeline plus each linked backend.
///
/// The variant names are the baseline column names, and each one states what
/// that column is allowed to claim:
///
/// * `pipeline` — compile gate. The program parses, resolves, typechecks and
///   lowers to HIR. It says nothing about the value.
/// * `native` — the value-checking column. It runs the program on the
///   Cranelift JIT and compares the value (and, where the expectation carries
///   one, the operation count).
/// * `java-emit` — emit-only. It proves the Java emitter turned this HIR into
///   a file without panicking; it never compiles or executes that file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuiteBackend {
    Pipeline,
    JavaEmit,
    Native,
}

impl SuiteBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pipeline => "pipeline",
            Self::JavaEmit => "java-emit",
            Self::Native => "native",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pipeline" => Self::Pipeline,
            // `java` is the pre-rename spelling, kept as an alias so existing
            // `leek-test-corpus -- failures java` invocations keep working.
            // `parse` returns `None` for an unknown name and the caller then
            // reads the argument as a *category* filter, so dropping the alias
            // would print a plausible empty table instead of an error.
            "java-emit" | "java" => Self::JavaEmit,
            "native" => Self::Native,
            _ => return None,
        })
    }

    fn from_manifest_kind(kind: BackendKind) -> Option<Self> {
        match kind {
            BackendKind::Java => Some(Self::JavaEmit),
            BackendKind::Native => Some(Self::Native),
            BackendKind::Jar | BackendKind::Wasm | BackendKind::LeekScript => None,
        }
    }
}

/// Backends to exercise: always `pipeline`, plus each [`leek_backends::LINKED`]
/// entry (and optional `[backend.*]` enable flags from `Miku.toml`).
pub fn detect_backends(table: Option<&BackendTable>) -> Vec<SuiteBackend> {
    let mut out = vec![SuiteBackend::Pipeline];
    for &kind in leek_backends::LINKED {
        let Some(sb) = SuiteBackend::from_manifest_kind(kind) else {
            continue;
        };
        let enabled = table.and_then(|t| t.get(kind)).is_none_or(|s| s.enable);
        if enabled && !out.contains(&sb) {
            out.push(sb);
        }
    }
    out
}

/// Per-backend reports keyed by [`SuiteBackend::as_str`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MultiReport {
    pub schema_version: u32,
    pub backends: BTreeMap<String, crate::run::Report>,
}

impl MultiReport {
    /// Bumped to 3 when baselines became failures-only (see [`Self::save`]).
    pub const SCHEMA_VERSION: u32 = 3;

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// Write this report as a baseline.
    ///
    /// Only *non-passing* outcomes are stored: the corpus is ~11k cases
    /// per backend and almost all of them pass, so a full pass map costs
    /// megabytes of tracked data per refresh for no added signal. The
    /// per-backend summaries are kept whole, and
    /// [`crate::run::Report::diff_against`] reads an absent id as
    /// [`crate::run::CaseOutcome::Pass`].
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(&self.failures_only())?)?;
        Ok(())
    }

    /// This report with every passing outcome dropped — the on-disk
    /// baseline shape.
    #[must_use]
    pub fn failures_only(&self) -> Self {
        Self {
            schema_version: self.schema_version,
            backends: self
                .backends
                .iter()
                .map(|(name, report)| (name.clone(), report.failures_only()))
                .collect(),
        }
    }

    pub fn diff_against(&self, baseline: &Self) -> MultiDiff {
        let mut diff = MultiDiff::default();
        for (name, report) in &self.backends {
            match baseline.backends.get(name) {
                Some(base) => {
                    let d = report.diff_against(base);
                    if !d.regressions.is_empty() {
                        diff.regressions.insert(name.clone(), d.regressions);
                    }
                    if !d.improvements.is_empty() {
                        diff.improvements.insert(name.clone(), d.improvements);
                    }
                }
                None => diff.new_backends.push(name.clone()),
            }
        }
        diff
    }
}

#[derive(Debug, Default)]
pub struct MultiDiff {
    pub regressions: BTreeMap<String, Vec<crate::run::Change>>,
    pub improvements: BTreeMap<String, Vec<crate::run::Change>>,
    pub new_backends: Vec<String>,
}

struct CaseContext {
    green: Option<rowan::GreenNode>,
    hir: Option<Arc<HirFile>>,
    has_compile_error: bool,
}

/// Stack for one corpus worker. A handful of upstream cases nest deeply
/// enough to blow the stack a default thread gets, so every thread that
/// compiles a case needs this. `leek_test_corpus::UPSTREAM_SUITE_STACK`
/// re-exports this same constant, so the two cannot drift.
pub const WORKER_STACK: usize = 64 * 1024 * 1024;

/// Cases a worker claims per `fetch_add`. Per-case cost spans orders of
/// magnitude (a parse-error case versus a JIT-compiled loop), so workers steal
/// small batches instead of taking a contiguous slice up front — a static
/// split leaves one worker holding all the heavy cases.
const BATCH: usize = 16;

/// Environment override for [`RunConfig::default`]'s worker count.
///
/// Deliberately *not* a `LEEK_EXPERIMENTAL_*` key: `FeatureFlags::from_env`
/// reads only those eight names, so this cannot move a single case outcome —
/// it only changes how many threads compute them. That is what makes it safe
/// to set next to a baseline-diffing gate.
pub const JOBS_ENV: &str = "LEEK_CORPUS_JOBS";

/// Cap on the *default* worker count. Peak memory scales with the number of
/// workers (each holds a live JIT module and its own bump arenas), and the
/// suite is compute-bound, so more than this trades memory for little.
const MAX_DEFAULT_JOBS: usize = 8;

/// One slice of the manifest, for splitting a run across CI jobs.
///
/// The split is by index modulo `count`, not contiguous: cases from one
/// upstream test file sit together and cost alike, so a contiguous split
/// would hand one shard a much longer run than the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shard {
    index: usize,
    count: usize,
}

impl Shard {
    /// The whole manifest — what every caller that is not sharding uses.
    pub const ALL: Self = Self { index: 0, count: 1 };

    /// Shard `index` of `count`. `None` unless `count >= 1 && index < count`,
    /// so an out-of-range `--shard` cannot quietly run zero cases.
    pub fn new(index: usize, count: usize) -> Option<Self> {
        (count >= 1 && index < count).then_some(Self { index, count })
    }

    pub fn index(self) -> usize {
        self.index
    }

    pub fn count(self) -> usize {
        self.count
    }

    /// Whether this is the whole manifest rather than a slice of it.
    pub fn is_all(self) -> bool {
        self.count == 1
    }

    fn contains(self, case_index: usize) -> bool {
        case_index % self.count == self.index
    }

    /// How many of `manifest`'s cases this shard owns. A sharded job can assert
    /// its report against this exact number — not a tolerance — so a shard that
    /// silently ran short is a red job rather than a thinner merged report.
    pub fn expected_len(self, manifest: &Manifest) -> usize {
        (0..manifest.cases.len())
            .filter(|&i| self.contains(i))
            .count()
    }
}

/// How to run a manifest: across how many worker threads, and which slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunConfig {
    /// Worker threads. `1` is not a special case — it takes the same code
    /// path, so a serial measurement exercises the shipped runner.
    pub jobs: usize,
    pub shard: Shard,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            jobs: default_jobs(),
            shard: Shard::ALL,
        }
    }
}

impl RunConfig {
    #[must_use]
    pub fn with_jobs(mut self, jobs: usize) -> Self {
        self.jobs = jobs.max(1);
        self
    }

    #[must_use]
    pub fn with_shard(mut self, shard: Shard) -> Self {
        self.shard = shard;
        self
    }
}

/// [`JOBS_ENV`] when it parses to a positive number, else this machine's
/// parallelism capped at [`MAX_DEFAULT_JOBS`].
fn default_jobs() -> usize {
    if let Some(raw) = std::env::var_os(JOBS_ENV)
        && let Some(n) = raw.to_str().and_then(|s| s.trim().parse::<usize>().ok())
        && n >= 1
    {
        return n;
    }
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_DEFAULT_JOBS)
}

/// What a worker needs to compile a case, built once per *worker* instead of
/// once per case: the recipe pipeline (~10k rebuilds otherwise) and the
/// feature flags (eight `env::var_os` lookups per case otherwise).
///
/// `Pipeline` holds `Box<dyn Step>` and is not `Send`, so each worker builds
/// its own — which is also what keeps the hoist honest: no pipeline is ever
/// shared between threads, and `Step::run` takes `&self`, so reusing one
/// across cases is the same contract the pipeline already documents.
struct CaseRunner {
    pipeline: leek_pipeline::Pipeline,
    flags: leek_pipeline::FeatureFlags,
    source: SourceId,
}

impl CaseRunner {
    fn new(source: SourceId) -> Self {
        Self {
            pipeline: leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive())
                .expect("recipe"),
            flags: leek_pipeline::FeatureFlags::from_env(),
            source,
        }
    }

    fn context(&self, case: &TestCase) -> CaseContext {
        let input = Input {
            source: self.source,
            text: case.code.clone().into(),
            version_byte: case.version,
            strict: case.strict,
            flags: self.flags,
        };
        let run = self.pipeline.run(input);
        let has_compile_error = run
            .diagnostics()
            .iter()
            .any(|d| d.severity == Severity::Error);
        let green = run
            .get::<leek_parser::pipeline::GreenTreeArtifact>()
            .map(|g| g.0.clone());
        let hir = run.get::<HirArtifact>().map(|a| Arc::clone(&a.0));
        CaseContext {
            green,
            hir,
            has_compile_error,
        }
    }
}

fn build_context(case: &TestCase, source: SourceId) -> CaseContext {
    CaseRunner::new(source).context(case)
}

/// Apply `f` to every case of `manifest` that `cfg.shard` owns, on `cfg.jobs`
/// worker threads, and return the `Some` results **in manifest order**.
///
/// Ordering the merge by case index rather than by completion is what keeps
/// every caller deterministic: the workers race, the result does not.
fn map_cases<T, F>(manifest: &Manifest, cfg: RunConfig, f: F) -> Vec<(usize, T)>
where
    T: Send,
    F: Fn(&TestCase, &CaseRunner) -> Option<T> + Sync,
{
    let indices: Vec<usize> = (0..manifest.cases.len())
        .filter(|&i| cfg.shard.contains(i))
        .collect();
    let jobs = cfg.jobs.max(1).min(indices.len().max(1));
    let cursor = AtomicUsize::new(0);
    let (f, indices, cursor) = (&f, &indices, &cursor);

    let mut parts: Vec<Vec<(usize, T)>> = Vec::with_capacity(jobs);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..jobs)
            .map(|w| {
                std::thread::Builder::new()
                    .name(format!("corpus-{w}"))
                    // Without this the deeply nested cases overflow the stack
                    // and abort the *process*, turning passes into a crash
                    // that names no failing case.
                    .stack_size(WORKER_STACK)
                    .spawn_scoped(scope, move || {
                        let runner = CaseRunner::new(SourceId::new(1).unwrap());
                        let mut out = Vec::new();
                        loop {
                            let start = cursor.fetch_add(BATCH, Ordering::Relaxed);
                            if start >= indices.len() {
                                break;
                            }
                            let end = (start + BATCH).min(indices.len());
                            for &i in &indices[start..end] {
                                if let Some(value) = f(&manifest.cases[i], &runner) {
                                    out.push((i, value));
                                }
                            }
                        }
                        out
                    })
                    .expect("spawn corpus worker")
            })
            .collect();
        for worker in workers {
            match worker.join() {
                Ok(part) => parts.push(part),
                // Re-raise rather than swallow: a worker panic means a case
                // escaped the harness's own `catch_unwind`, and a run that
                // quietly lost those cases would diff green against the
                // baseline.
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
    });

    let mut out: Vec<(usize, T)> = parts.into_iter().flatten().collect();
    out.sort_by_key(|(i, _)| *i);
    out
}

/// Run the full manifest on each detected backend, across the default number
/// of workers (see [`RunConfig`]).
pub fn run_manifest(manifest: &Manifest, backends: &[SuiteBackend]) -> MultiReport {
    run_manifest_with(manifest, backends, RunConfig::default())
}

/// Run `cfg`'s slice of the manifest on each backend, across `cfg.jobs`
/// workers (one pipeline build per *worker*, not per case).
///
/// The result does not depend on `cfg.jobs`: cases are independent (every
/// native run reinstalls its tables and reseeds the RNG from a fixed seed, and
/// the runtime's state is thread-local), outcomes merge in manifest order, and
/// the summary counters are commutative. `tests/parallel_determinism.rs` pins
/// that equality rather than leaving it as a claim.
pub fn run_manifest_with(
    manifest: &Manifest,
    backends: &[SuiteBackend],
    cfg: RunConfig,
) -> MultiReport {
    // Ids key the outcome map, so a duplicate makes the merge order-dependent:
    // whichever worker finished last would win and the baseline diff would
    // flicker between runs. The serial loop hid this (last-wins by position),
    // which is why uniqueness is asserted here rather than assumed.
    let ids: BTreeSet<&str> = manifest.cases.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids.len(),
        manifest.cases.len(),
        "the manifest has duplicate case id(s); outcomes are keyed by id, so the \
         merged report would depend on worker scheduling",
    );

    let mut multi = MultiReport {
        schema_version: MultiReport::SCHEMA_VERSION,
        backends: BTreeMap::new(),
    };
    for &backend in backends {
        multi
            .backends
            .insert(backend.as_str().to_string(), crate::run::Report::default());
    }

    let results = map_cases(manifest, cfg, |case, runner| {
        let outcomes: Vec<CaseOutcome> = if case.enabled {
            let ctx = runner.context(case);
            backends
                .iter()
                .map(|&backend| run_case_with_ctx(case, runner.source, backend, &ctx))
                .collect()
        } else {
            vec![CaseOutcome::SkippedDisabled; backends.len()]
        };
        Some(outcomes)
    });

    for (i, outcomes) in results {
        let case = &manifest.cases[i];
        for (&backend, &outcome) in backends.iter().zip(&outcomes) {
            let report = multi.backends.get_mut(backend.as_str()).expect("report");
            report.summary.record(outcome);
            let previous = report.outcomes.insert(case.id.clone(), outcome);
            debug_assert!(previous.is_none(), "case id {} recorded twice", case.id);
        }
    }
    multi
}

/// Run one case on a single backend.
pub fn run_case_backend(case: &TestCase, source: SourceId, backend: SuiteBackend) -> CaseOutcome {
    if !case.enabled {
        return CaseOutcome::SkippedDisabled;
    }
    if !(1..=4).contains(&case.version) {
        return CaseOutcome::SkippedUnknown;
    }

    let ctx = build_context(case, source);
    run_case_with_ctx(case, source, backend, &ctx)
}

fn run_case_with_ctx(
    case: &TestCase,
    source: SourceId,
    backend: SuiteBackend,
    ctx: &CaseContext,
) -> CaseOutcome {
    match backend {
        SuiteBackend::Pipeline => run_pipeline(case, ctx, source),
        SuiteBackend::JavaEmit => run_java_emit(case, ctx),
        SuiteBackend::Native => run_native(case, ctx),
    }
}

/// Run a case on the native (Cranelift JIT) backend. The backend only
/// handles the scalar (integer / boolean) + control-flow subset so far,
/// so anything it can't compile reports `Unsupported` → we skip it. A
/// value mismatch on something it *did* compile is a real failure;
/// compile/runtime errors are skipped (treated as not-yet-supported)
/// while the backend matures.
/// Outcome of a native JIT run, distinguishing a real value from the two
/// non-value cases the caller must treat differently: an `Unsupported` error
/// (construct not in the compiled subset → skip) versus a *panic* during
/// codegen/execution, which is a genuine backend defect and must be a failure
/// — never a silent skip or, worse, a whole-suite abort.
enum NativeRun {
    Value(String),
    /// The compiled program trapped at runtime (`NativeError::Runtime`) —
    /// e.g. an array out-of-bounds write. Distinct from `Unsupported` so the
    /// harness can verify runtime-error expectations against it. The message
    /// is logged in [`native_run`]; only the *distinction* is needed here.
    RuntimeError,
    Unsupported,
    Panicked,
}

/// Run the native backend, converting any panic during JIT compilation or
/// execution into [`NativeRun::Panicked`]. Without this, a single panicking
/// case aborts the entire corpus worker (`run_on_large_stack` re-panics on
/// `join`), so a miscompile-via-panic would be invisible. Mirrors the
/// `catch_unwind` discipline in `leek-backend-java`'s parity tests.
fn native_run(
    case: &TestCase,
    hir: &leek_hir::HirFile,
    opts: &leek_backend_native::NativeOptions,
) -> NativeRun {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        leek_backend_native::run(hir, opts)
    }));
    match result {
        Ok(Ok(v)) => NativeRun::Value(v.to_string()),
        Ok(Err(leek_backend_native::NativeError::Runtime(m))) => {
            // A clean compile that trapped at runtime. Log for triage; the
            // caller distinguishes this from `Unsupported` to verify
            // runtime-error expectations.
            eprintln!("native runtime error on case {}: {m}", case.id);
            NativeRun::RuntimeError
        }
        Ok(Err(_)) => NativeRun::Unsupported,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            // Surface for the triage pass — a native panic is a real defect.
            eprintln!("native backend panicked on case {}: {msg}", case.id);
            NativeRun::Panicked
        }
    }
}

fn run_native(case: &TestCase, ctx: &CaseContext) -> CaseOutcome {
    if !case
        .check_plan()
        .kinds
        .iter()
        .any(|k| matches!(k, CheckKind::NativeRun))
    {
        return CaseOutcome::SkippedUnknown;
    }

    // Structural cases (compile/runtime errors) are verified before we touch
    // the JIT: an erroneous program has no valid HIR for native to run, and a
    // compile error is produced by the shared frontend native depends on.
    if case.expected.implies_error() {
        return native_error_outcome(case, ctx);
    }
    // Any other expectation that nonetheless failed to compile is a real
    // regression — but native shares the frontend, so defer that signal to the
    // pipeline backend (which owns compile-error reporting) and skip here.
    if ctx.has_compile_error {
        return CaseOutcome::SkippedUnknown;
    }
    let Some(hir) = ctx.hir.as_deref() else {
        return CaseOutcome::SkippedUnknown;
    };

    leek_runtime::DISPLAY_VERSION.with(|c| c.set(case.version));
    let opts = leek_backend_native::NativeOptions::release().with_lang(case.version, case.strict);

    match &case.expected {
        // Exact value match.
        Expectation::Equals { value } => match native_run(case, hir, &opts) {
            NativeRun::Value(got) => {
                if got == *value {
                    CaseOutcome::Pass
                } else {
                    CaseOutcome::FailWrongValue
                }
            }
            // Not in the supported subset yet, or no compile-error
            // model — skip rather than count as a failure.
            NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
            // A value was expected but the program trapped, or the JIT
            // panicked — both are real defects, not skips.
            NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
        },
        // Approximate float match (upstream `.almost(X)` — loose tolerance).
        Expectation::Almost { value } => match native_run(case, hir, &opts) {
            NativeRun::Value(got) => match native_almost_matches(&got, value) {
                Some(true) => CaseOutcome::Pass,
                Some(false) => CaseOutcome::FailWrongValue,
                // Expected is a Java expression (e.g. `Math.PI`) we can't
                // evaluate here — skip rather than false-pass/false-fail.
                None => CaseOutcome::SkippedUnknown,
            },
            NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
            NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
        },
        // `.equalsOps("value", N)` — native charges ops at the same MIR sites
        // as the interp, so it verifies BOTH the value and the operation count.
        Expectation::EqualsOps { .. } | Expectation::Unknown { .. } => {
            let Some((value, ops)) = equals_ops_expectation(case) else {
                return CaseOutcome::SkippedUnknown;
            };
            match native_run(case, hir, &opts) {
                NativeRun::Value(got) => {
                    if got == value && leek_backend_native::ops_used() == ops {
                        CaseOutcome::Pass
                    } else {
                        CaseOutcome::FailWrongValue
                    }
                }
                NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
                NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
            }
        }
        // No error expected (`Error{NONE}`) or a warning/no-warning case: the
        // program compiles cleanly, so native must run it without trapping. We
        // don't verify the warning *code* (that is a frontend diagnostic the
        // resolver/typechecker own) — only that the compiled program executes
        // cleanly, mirroring the interp path.
        Expectation::Error { .. } | Expectation::Warning { .. } | Expectation::NoWarning => {
            match native_run(case, hir, &opts) {
                NativeRun::Value(_) => CaseOutcome::Pass,
                // Not in the compiled subset yet — skip, don't fail.
                NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
                // A clean program trapped or panicked on native — a real defect.
                NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
            }
        }
        // `.ops(N)` carries only an operation count. Native charges ops at the
        // same MIR sites as the interp (plus the `leek-charge` static charges),
        // so it runs the program (unbounded op budget, so it completes) and
        // compares its charged total to the expected count.
        Expectation::Ops { count } => match native_run(case, hir, &opts) {
            NativeRun::Value(_) => {
                if leek_backend_native::ops_used() == *count {
                    CaseOutcome::Pass
                } else {
                    CaseOutcome::FailWrongValue
                }
            }
            NativeRun::Unsupported => CaseOutcome::SkippedUnknown,
            NativeRun::RuntimeError | NativeRun::Panicked => CaseOutcome::FailWrongValue,
        },
        // `AnyError` is reached only via a frontend leniency gap (handled by
        // `native_error_outcome`); nothing to verify here.
        Expectation::AnyError => CaseOutcome::SkippedUnknown,
    }
}

/// Verify an upstream *error* expectation (`Error{code != "NONE"}` or
/// `AnyError`) against the native backend. Compile errors are produced by the
/// shared frontend native depends on, so a present compile diagnostic means
/// native correctly refuses the program → `PassExpectedError` (mirrors
/// [`compile_error_outcome`], which the pipeline backend uses). Runtime-error
/// codes (`TOO_MUCH_OPERATIONS`, `ARRAY_OUT_OF_BOUND`, …) have no compile
/// diagnostic; verifying those means *running* the program and observing a
/// trap, which is deferred to a later phase (running an unbounded
/// `TOO_MUCH_OPERATIONS` loop on the JIT, which has no op limit, would hang).
fn native_error_outcome(case: &TestCase, ctx: &CaseContext) -> CaseOutcome {
    if ctx.has_compile_error {
        return CaseOutcome::PassExpectedError;
    }
    // Runtime-error codes have no compile diagnostic — verifying them means
    // *running* the program and observing a fault. Native now charges ops and,
    // under a finite op budget, stops a runaway loop at a back-edge — so the
    // resource-exhaustion codes (`TOO_MUCH_OPERATIONS` / `OUT_OF_MEMORY`) trip
    // the budget instead of spinning, and `ARRAY_OUT_OF_BOUND` faults on the
    // bad write. The upstream harness accepts *any* runtime error for these.
    if let Expectation::Error { code } = &case.expected
        && is_runtime_error_code(code)
        && let Some(hir) = ctx.hir.as_deref()
    {
        leek_runtime::DISPLAY_VERSION.with(|c| c.set(case.version));
        // A *small* op budget bounds execution tightly: a runaway loop trips it
        // (recording TOO_MUCH_OPERATIONS) within a few thousand iterations. This
        // matters because native leaks intermediate composite values (handles
        // are freed only at the result boundary), so an `a += i` concat loop
        // accumulates memory until it stops — a low cap keeps that bounded. The
        // harness accepts *any* runtime error here, so the exact budget is
        // immaterial as long as the loop faults.
        let opts = leek_backend_native::NativeOptions::release()
            .with_lang(case.version, case.strict)
            .with_op_limit(10_000);
        return match native_run(case, hir, &opts) {
            NativeRun::RuntimeError => CaseOutcome::PassExpectedError,
            // Native ran to completion without faulting, couldn't compile, or
            // panicked — don't claim a pass; skip rather than fail.
            _ => CaseOutcome::SkippedUnknown,
        };
    }
    CaseOutcome::SkippedUnknown
}

/// Compare a native runtime value's string form against an upstream
/// `.almost(X)` expectation. Mirrors [`crate::run::check_almost`]: parse
/// both as `f64` (normalizing v1 comma decimals, stripping Java numeric
/// suffixes) and accept within a loose relative tolerance.
///
/// Returns `Some(true)`/`Some(false)` for a real verdict, or `None` when
/// the expected side is something we can't evaluate here — the caller
/// skips those rather than guessing.
///
/// Handles plain floats, the two-arg `value, delta` form, and the small
/// Java math grammar upstream uses (`Math.PI / 2`, `Math.sqrt(2)`,
/// `-3 * Math.PI / 4`, …) via [`eval_java_math`].
fn native_almost_matches(got: &str, expected_str: &str) -> Option<bool> {
    let normalized = got.replace(',', ".");
    let got_f = normalized.parse::<f64>().ok()?;
    let (expected, explicit_delta) = eval_java_almost_expected(expected_str)?;
    // Upstream's default tolerance is loose-relative; an explicit second
    // argument overrides it.
    let tol = explicit_delta.unwrap_or_else(|| 1e-9_f64.max(expected.abs() * 1e-9));
    Some((got_f - expected).abs() <= tol)
}

/// Parse an upstream `.almost(...)` argument list into `(value, delta?)`.
/// The list is either a single expression or `value, delta`. Shared with the
/// interpreter/pipeline `check_almost` path so both backends evaluate the
/// expected side identically (and fail-closed when it can't be evaluated).
pub(crate) fn eval_java_almost_expected(s: &str) -> Option<(f64, Option<f64>)> {
    let parts = split_top_level_commas(s.trim());
    match parts.as_slice() {
        [v] => Some((eval_java_math(v)?, None)),
        [v, d] => Some((eval_java_math(v)?, Some(eval_java_math(d)?))),
        _ => None,
    }
}

/// Split on commas that sit at paren-depth 0 (so `Math.pow(2, 3)` stays
/// whole but `12.0, 1e-14` splits into two).
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// Evaluate the bounded Java math grammar used by upstream `.almost(...)`
/// expectations: float/int literals, `Math.PI`/`Math.E`, `Math.fn(args)`
/// calls, parenthesised groups, unary `±`, and `+ - * /`. Returns `None`
/// for anything outside the grammar so the caller can skip rather than
/// fabricate a verdict.
fn eval_java_math(s: &str) -> Option<f64> {
    let tokens = lex_java_math(s)?;
    let mut p = MathParser {
        tokens: &tokens,
        pos: 0,
    };
    let v = p.expr()?;
    if p.pos == p.tokens.len() {
        Some(v)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
enum MathTok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    Comma,
}

fn lex_java_math(s: &str) -> Option<Vec<MathTok>> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == '+' {
            out.push(MathTok::Plus);
            i += 1;
        } else if c == '-' {
            out.push(MathTok::Minus);
            i += 1;
        } else if c == '*' {
            out.push(MathTok::Star);
            i += 1;
        } else if c == '/' {
            out.push(MathTok::Slash);
            i += 1;
        } else if c == '(' {
            out.push(MathTok::LParen);
            i += 1;
        } else if c == ')' {
            out.push(MathTok::RParen);
            i += 1;
        } else if c == ',' {
            out.push(MathTok::Comma);
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // Optional exponent: e[+/-]digits
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                i += 1;
                if i < chars.len() && (chars[i] == '+' || chars[i] == '-') {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            // Drop a trailing Java numeric suffix (f/F/d/D/l/L).
            let lit: String = chars[start..i].iter().collect();
            if i < chars.len() && matches!(chars[i], 'f' | 'F' | 'd' | 'D' | 'l' | 'L') {
                i += 1;
            }
            out.push(MathTok::Num(lit.parse::<f64>().ok()?));
        } else if c.is_ascii_alphabetic() {
            // Identifier may contain dots (`Math.PI`, `Math.cos`).
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            out.push(MathTok::Ident(chars[start..i].iter().collect()));
        } else {
            return None;
        }
    }
    Some(out)
}

struct MathParser<'a> {
    tokens: &'a [MathTok],
    pos: usize,
}

impl MathParser<'_> {
    fn peek(&self) -> Option<&MathTok> {
        self.tokens.get(self.pos)
    }
    fn bump(&mut self) -> Option<&MathTok> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &MathTok) -> Option<()> {
        if self.peek() == Some(t) {
            self.pos += 1;
            Some(())
        } else {
            None
        }
    }

    fn expr(&mut self) -> Option<f64> {
        let mut acc = self.term()?;
        while let Some(t) = self.peek() {
            match t {
                MathTok::Plus => {
                    self.pos += 1;
                    acc += self.term()?;
                }
                MathTok::Minus => {
                    self.pos += 1;
                    acc -= self.term()?;
                }
                _ => break,
            }
        }
        Some(acc)
    }

    fn term(&mut self) -> Option<f64> {
        let mut acc = self.factor()?;
        while let Some(t) = self.peek() {
            match t {
                MathTok::Star => {
                    self.pos += 1;
                    acc *= self.factor()?;
                }
                MathTok::Slash => {
                    self.pos += 1;
                    acc /= self.factor()?;
                }
                _ => break,
            }
        }
        Some(acc)
    }

    fn factor(&mut self) -> Option<f64> {
        match self.peek()? {
            MathTok::Minus => {
                self.pos += 1;
                Some(-self.factor()?)
            }
            MathTok::Plus => {
                self.pos += 1;
                self.factor()
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Option<f64> {
        match self.bump()?.clone() {
            MathTok::Num(n) => Some(n),
            MathTok::LParen => {
                let v = self.expr()?;
                self.eat(&MathTok::RParen)?;
                Some(v)
            }
            MathTok::Ident(name) => {
                // A function call if followed by `(`, otherwise a constant.
                if self.eat(&MathTok::LParen).is_some() {
                    let mut args = vec![self.expr()?];
                    while self.eat(&MathTok::Comma).is_some() {
                        args.push(self.expr()?);
                    }
                    self.eat(&MathTok::RParen)?;
                    eval_math_fn(&name, &args)
                } else {
                    eval_math_const(&name)
                }
            }
            _ => None,
        }
    }
}

fn eval_math_const(name: &str) -> Option<f64> {
    match name {
        "Math.PI" => Some(std::f64::consts::PI),
        "Math.E" => Some(std::f64::consts::E),
        _ => None,
    }
}

fn eval_math_fn(name: &str, args: &[f64]) -> Option<f64> {
    let one = |f: fn(f64) -> f64| (args.len() == 1).then(|| f(args[0]));
    match name {
        "Math.cos" => one(f64::cos),
        "Math.sin" => one(f64::sin),
        "Math.tan" => one(f64::tan),
        "Math.acos" => one(f64::acos),
        "Math.asin" => one(f64::asin),
        "Math.atan" => one(f64::atan),
        "Math.cosh" => one(f64::cosh),
        "Math.sinh" => one(f64::sinh),
        "Math.tanh" => one(f64::tanh),
        "Math.sqrt" => one(f64::sqrt),
        "Math.cbrt" => one(f64::cbrt),
        "Math.abs" => one(f64::abs),
        "Math.exp" => one(f64::exp),
        "Math.log" => one(f64::ln),
        "Math.log10" => one(f64::log10),
        "Math.ceil" => one(f64::ceil),
        "Math.floor" => one(f64::floor),
        "Math.signum" => one(f64::signum),
        "Math.toRadians" => one(f64::to_radians),
        "Math.toDegrees" => one(f64::to_degrees),
        "Math.pow" if args.len() == 2 => Some(args[0].powf(args[1])),
        "Math.atan2" if args.len() == 2 => Some(args[0].atan2(args[1])),
        "Math.hypot" if args.len() == 2 => Some(args[0].hypot(args[1])),
        "Math.min" if args.len() == 2 => Some(args[0].min(args[1])),
        "Math.max" if args.len() == 2 => Some(args[0].max(args[1])),
        _ => None,
    }
}

/// One row of the native skip-reason histogram: a normalized reason, how
/// many cases hit it, and one sample snippet.
#[derive(Debug, Clone)]
pub struct NativeSkipRow {
    pub reason: String,
    pub count: u32,
    pub sample: String,
}

/// Histogram of *why* native skips cases it attempts (enabled `Equals`
/// cases that compile to HIR). Drives coverage-growth decisions: the
/// biggest buckets are the highest-value features to add next. Rows are
/// returned sorted by descending count.
pub fn native_skip_histogram(manifest: &Manifest) -> Vec<NativeSkipRow> {
    // Same worker pool as the suite; folding the per-case reasons back in
    // manifest order keeps both the counts and the sample snippet (first case
    // in the bucket) independent of which worker got there first.
    let skips = map_cases(manifest, RunConfig::default(), |case, runner| {
        if !case.enabled || !matches!(case.expected, Expectation::Equals { .. }) {
            return None;
        }
        let reason = native_skip_reason(case, runner)?;
        Some((reason, case.code.clone()))
    });

    let mut hist: BTreeMap<String, (u32, String)> = BTreeMap::new();
    for (_, (reason, code)) in skips {
        let entry = hist.entry(reason).or_insert((0, code));
        entry.0 += 1;
    }
    let mut rows: Vec<NativeSkipRow> = hist
        .into_iter()
        .map(|(reason, (count, sample))| NativeSkipRow {
            reason,
            count,
            sample,
        })
        .collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.reason.cmp(&b.reason)));
    rows
}

/// A single skipped case dumped for triage: full source, the normalized
/// skip reason, and the expected value.
#[derive(Debug, Clone)]
pub struct NativeSkipCase {
    pub reason: String,
    pub expected: String,
    pub code: String,
}

/// Every attempted `Equals` case whose normalized native skip reason
/// contains `filter` (case-insensitive substring) — with full source, for
/// hands-on triage of a histogram bucket.
pub fn native_skips_matching(manifest: &Manifest, filter: &str) -> Vec<NativeSkipCase> {
    let needle = filter.to_lowercase();
    let matches = map_cases(manifest, RunConfig::default(), |case, runner| {
        let Expectation::Equals { value } = &case.expected else {
            return None;
        };
        if !case.enabled {
            return None;
        }
        let reason = native_skip_reason(case, runner)?;
        if !reason.to_lowercase().contains(&needle) {
            return None;
        }
        Some(NativeSkipCase {
            reason,
            expected: value.clone(),
            code: case.code.clone(),
        })
    });
    matches.into_iter().map(|(_, c)| c).collect()
}

/// Why native declined this case, or `None` if it compiled and ran (or never
/// reached the backend). Shared by the two triage listings above so they
/// cannot drift apart in what counts as "attempted".
fn native_skip_reason(case: &TestCase, runner: &CaseRunner) -> Option<String> {
    let ctx = runner.context(case);
    if ctx.has_compile_error {
        return None;
    }
    let hir = ctx.hir.as_deref()?;
    let opts = leek_backend_native::NativeOptions::release().with_lang(case.version, case.strict);
    leek_backend_native::run(hir, &opts)
        .err()
        .map(|e| normalize_native_skip(&e))
}

/// Collapse a [`leek_backend_native::NativeError`] into a stable bucket
/// key by dropping the case-specific tail (identifiers, `Debug` payloads).
fn normalize_native_skip(e: &leek_backend_native::NativeError) -> String {
    let msg = e.to_string();
    // Drop the error-kind prefix ("unsupported: ", "compile error: ", …)
    // to get the bare reason.
    let reason = msg.split_once(": ").map_or(&*msg, |(_, rest)| rest);
    // Drop any case-specific tail (a concrete identifier or `Debug` blob)
    // so similar reasons bucket together.
    let head = reason.split([':', '(']).next().unwrap_or(reason).trim();
    let words: Vec<&str> = head.split_whitespace().collect();
    match words.as_slice() {
        ["builtin", _rest @ ..] => "builtin <name>".to_string(),
        ["assign", "to", _rest @ ..] => "assign to <place>".to_string(),
        ["const", _rest @ ..] => "const <other>".to_string(),
        ["rvalue", kind, ..] => format!("rvalue {kind}"),
        ["real", "binary", "op", ..] => "real binary op <other>".to_string(),
        ["binary", "op", ..] => "binary op <other>".to_string(),
        [name, "expected", "1", "arg"] => format!("{name}: arity != 1"),
        _ => head.to_string(),
    }
}

fn is_runtime_error_code(code: &str) -> bool {
    matches!(
        code,
        "TOO_MUCH_OPERATIONS" | "OUT_OF_MEMORY" | "ARRAY_OUT_OF_BOUND" | "STACK_OVERFLOW"
    )
}

fn compile_error_outcome(case: &TestCase, ctx: &CaseContext) -> CaseOutcome {
    if ctx.has_compile_error {
        return CaseOutcome::PassExpectedError;
    }
    if let Expectation::Error { code } = &case.expected
        && is_runtime_error_code(code)
        && let Some(hir) = ctx.hir.as_deref()
    {
        // The native JIT surfaces a runtime error (e.g. ARRAY_OUT_OF_BOUND,
        // TOO_MUCH_OPERATIONS) as `Err(NativeError::Runtime(..))`. The budget
        // matches `native_error_outcome`: it must stay *small*, because a
        // terminating program (e.g. `rec(10000)` under upstream `max_ops(1000)`)
        // has to trip the budget before it completes — op-charge coalescing
        // made a 200k budget too generous for exactly that case. Any runtime
        // error counts here, so tightening the budget can only turn "ran to
        // completion" into a trip, never break a passing case.
        let mut opts =
            leek_backend_native::NativeOptions::release().with_lang(case.version, case.strict);
        opts.op_limit = 10_000;
        opts.emit = leek_backend_native::NativeEmit::Jit;
        if matches!(
            leek_backend_native::compile(hir, &opts),
            Err(leek_backend_native::NativeError::Runtime(_))
        ) {
            return CaseOutcome::PassExpectedError;
        }
    }
    CaseOutcome::FailMissingError
}

fn equals_ops_expectation(case: &TestCase) -> Option<(String, u64)> {
    match &case.expected {
        Expectation::EqualsOps { value, count } => Some((value.clone(), *count)),
        Expectation::Unknown { detail } if detail == "equalsOps" => {
            parse_equals_ops_java_line(&case.java_line)
        }
        _ => None,
    }
}

/// Legacy manifest rows stored as `unknown` + `equalsOps` detail.
fn parse_equals_ops_java_line(java_line: &str) -> Option<(String, u64)> {
    let after = java_line.find(".equalsOps(")? + ".equalsOps(".len();
    let rest = java_line.get(after..)?;
    let end = rest.find(')')?;
    let inner = &rest[..end];
    let value = parse_java_string_literal(inner)?;
    let tail = tail_after_first_string_literal(inner)?;
    let count = tail
        .trim()
        .strip_prefix(',')?
        .trim()
        .trim_end_matches('L')
        .parse::<u64>()
        .ok()?;
    Some((value, count))
}

fn parse_java_string_literal(s: &str) -> Option<String> {
    let t = s.trim();
    if !t.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = t[1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    None
}

fn tail_after_first_string_literal(s: &str) -> Option<&str> {
    let mut chars = s.char_indices().peekable();
    while chars.peek().is_some_and(|(_, c)| c.is_ascii_whitespace()) {
        chars.next();
    }
    if chars.peek().map(|(_, c)| *c) != Some('"') {
        return None;
    }
    chars.next();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => return s.get(i + 1..),
            _ => {}
        }
    }
    None
}

fn run_pipeline(case: &TestCase, ctx: &CaseContext, _source: SourceId) -> CaseOutcome {
    let plan = case.check_plan();
    if !plan.kinds.iter().any(|k| {
        matches!(
            k,
            CheckKind::Parse | CheckKind::Resolve | CheckKind::Typecheck | CheckKind::Hir
        )
    }) {
        return CaseOutcome::SkippedUnknown;
    }

    if ctx.green.is_none() {
        return if case.expected.implies_error() {
            CaseOutcome::PassExpectedError
        } else {
            CaseOutcome::FailParseError
        };
    }
    if case.expected.implies_error() {
        return compile_error_outcome(case, ctx);
    }

    if equals_ops_expectation(case).is_some() {
        // Value + op count are verified by the native backend (`NativeRun`)
        // against the upstream (Java) oracle; the pipeline backend only
        // confirms the program parses/compiles cleanly (the interpreter that
        // used to re-check the value here was removed).
        return if ctx.has_compile_error {
            CaseOutcome::FailParseError
        } else {
            CaseOutcome::Pass
        };
    }

    if !case.expected.implies_clean_parse() {
        return CaseOutcome::SkippedUnknown;
    }

    if ctx.has_compile_error {
        return CaseOutcome::FailParseError;
    }

    // The program compiled cleanly. Value / `.almost` / `.ops` correctness is
    // verified by the native backend (`NativeRun`) against the upstream (Java)
    // oracle, not re-executed here.
    CaseOutcome::Pass
}

/// A coarse bucket for a failing case, derived from the case's
/// expectation plus the observed [`CaseOutcome`]. Used by the
/// `failures` subcommand to group failures into a readable table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailureCategory {
    /// Expected to run, but the pipeline rejected it (parse / resolve /
    /// type error). "Can't run the code."
    WontCompile,
    /// Expected a compile/runtime error; none was produced.
    MissingError,
    /// `.equals(X)` value mismatch.
    ValueEquals,
    /// `.almost(X)` numeric mismatch.
    ValueAlmost,
    /// `.ops(N)` operation-count mismatch.
    Ops,
    /// `.equalsOps(X, N)` value-or-ops mismatch.
    EqualsOps,
    /// Wrong value with an expectation kind that doesn't fit above.
    OtherWrong,
}

impl FailureCategory {
    /// All variants, in display order.
    pub const ALL: [FailureCategory; 7] = [
        Self::WontCompile,
        Self::MissingError,
        Self::ValueEquals,
        Self::ValueAlmost,
        Self::Ops,
        Self::EqualsOps,
        Self::OtherWrong,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::WontCompile => "won't compile",
            Self::MissingError => "missing error",
            Self::ValueEquals => "value (equals)",
            Self::ValueAlmost => "value (almost)",
            Self::Ops => "ops count",
            Self::EqualsOps => "value+ops",
            Self::OtherWrong => "other wrong value",
        }
    }
}

/// Classify a failing outcome into a [`FailureCategory`]. Returns
/// `None` for outcomes that are not failures (pass / skip).
pub fn categorize_failure(outcome: CaseOutcome, expected: &Expectation) -> Option<FailureCategory> {
    Some(match outcome {
        CaseOutcome::FailParseError => FailureCategory::WontCompile,
        CaseOutcome::FailMissingError => FailureCategory::MissingError,
        CaseOutcome::FailWrongValue => match expected {
            Expectation::Equals { .. } => FailureCategory::ValueEquals,
            Expectation::Almost { .. } => FailureCategory::ValueAlmost,
            Expectation::Ops { .. } => FailureCategory::Ops,
            Expectation::EqualsOps { .. } => FailureCategory::EqualsOps,
            Expectation::Unknown { detail } if detail == "equalsOps" => FailureCategory::EqualsOps,
            _ => FailureCategory::OtherWrong,
        },
        _ => return None,
    })
}

/// Expected value, rendered for the failures table.
pub fn expected_display(e: &Expectation) -> String {
    match e {
        Expectation::Equals { value } => value.clone(),
        Expectation::Almost { value } => format!("≈ {value}"),
        Expectation::Ops { count } => format!("ops={count}"),
        Expectation::EqualsOps { value, count } => format!("{value} (ops={count})"),
        Expectation::Error { code } => format!("error {code}"),
        Expectation::AnyError => "<any error>".into(),
        Expectation::Warning { code } => format!("warning {code}"),
        Expectation::NoWarning => "<no warning>".into(),
        Expectation::Unknown { detail } => format!("unknown({detail})"),
    }
}

/// Expected vs. actual probe for a single failing case, rendered as
/// human-readable strings. Re-runs the relevant backend, so call only
/// for the (few) cases that actually fail.
pub struct CaseProbe {
    pub expected: String,
    pub actual: String,
}

/// Build [`CaseProbe`] strings for `case` on `backend`. The `actual`
/// side mirrors what the corpus runner observed: a compile error, an
/// interpreter value/error/ops count, or a Java-emit summary.
pub fn probe_case(case: &TestCase, source: SourceId, backend: SuiteBackend) -> CaseProbe {
    CaseProbe {
        expected: expected_display(&case.expected),
        actual: actual_display(case, source, backend),
    }
}

fn first_error_message(case: &TestCase, source: SourceId) -> String {
    let input = Input {
        source,
        text: case.code.clone().into(),
        version_byte: case.version,
        strict: case.strict,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let Ok(pipeline) = leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive()) else {
        return "<pipeline build failed>".into();
    };
    pipeline
        .run(input)
        .diagnostics()
        .iter()
        .find(|d| d.severity == Severity::Error)
        .map_or_else(
            || "<no error message>".into(),
            |d| format!("[{}] {}", d.code.0, d.message),
        )
}

fn actual_display(case: &TestCase, source: SourceId, backend: SuiteBackend) -> String {
    let ctx = build_context(case, source);
    if ctx.has_compile_error {
        return format!("compile error: {}", first_error_message(case, source));
    }
    let Some(hir) = ctx.hir.as_deref() else {
        return "<no HIR produced>".into();
    };
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(case.version));
    match backend {
        SuiteBackend::JavaEmit => match java_emit(case, hir) {
            JavaEmitRun::Emitted { bytes } => format!("emit ok, {bytes} bytes (not compiled)"),
            JavaEmitRun::Panicked => "emitter panicked".into(),
        },
        SuiteBackend::Native => {
            let opts =
                leek_backend_native::NativeOptions::release().with_lang(case.version, case.strict);
            match leek_backend_native::run(hir, &opts) {
                Ok(v) => v.to_string(),
                Err(e) => format!("{e}"),
            }
        }
        SuiteBackend::Pipeline => "compiled cleanly".into(),
    }
}

/// Outcome of one Java emission. There is no "emitted the wrong thing" arm:
/// `emit_clean` is infallible by construction, so the only failure it can
/// report is a panic.
enum JavaEmitRun {
    Emitted { bytes: usize },
    Panicked,
}

/// Emit Java for a case, converting a panic in the emitter into
/// [`JavaEmitRun::Panicked`]. Mirrors the `catch_unwind` discipline in
/// [`native_run`]: without it a single panicking case aborts the whole corpus
/// worker (`run_on_large_stack` re-panics on `join`), so an emitter defect
/// would take the suite down instead of being recorded against its case.
fn java_emit(case: &TestCase, hir: &leek_hir::HirFile) -> JavaEmitRun {
    let version = version_from_byte(case.version);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        leek_backend_java::emit_clean(hir, version, 1).java.len()
    }));
    match result {
        Ok(bytes) => JavaEmitRun::Emitted { bytes },
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            // Surface for the triage pass — an emitter panic is a real defect.
            eprintln!("java emitter panicked on case {}: {msg}", case.id);
            JavaEmitRun::Panicked
        }
    }
}

/// Emit-only backend. It proves exactly one thing: the Java emitter turned
/// this HIR into a file without panicking. It does **not** compile or execute
/// the output, so it cannot see a miscompile — values are verified by
/// `native`, and against a real JVM by `leek-bench`'s
/// `run_fast_java_corpus` and `leek-backend-java`'s parity tests, neither of
/// which is wired into this corpus yet (see #70).
///
/// This used to assert `emitted.java.contains("class AI_")`, which is a
/// tautology: `emit_file` writes `public class AI_<id> extends …` before it
/// emits any body, so the check was true for every HIR that did not panic the
/// emitter, and the column was a strictly weaker restatement of `pipeline`.
/// Error expectations are still verified for real, by the shared frontend
/// ([`compile_error_outcome`]).
fn run_java_emit(case: &TestCase, ctx: &CaseContext) -> CaseOutcome {
    if !case.check_plan().kinds.contains(&CheckKind::JavaEmit) {
        return CaseOutcome::SkippedUnknown;
    }

    if case.expected.implies_error() {
        return compile_error_outcome(case, ctx);
    }

    // A case that expects a value but does not compile is a failure here for
    // the same reason it is on `pipeline`. Lowering recovers HIR from an
    // erroring parse, so `ctx.hir` alone is not evidence the program was
    // accepted — without this check `java-emit` reported `Pass` for programs
    // `pipeline` reported as `FailParseError`.
    if ctx.has_compile_error {
        return CaseOutcome::FailParseError;
    }

    let Some(hir) = ctx.hir.as_deref() else {
        return CaseOutcome::FailParseError;
    };

    match java_emit(case, hir) {
        JavaEmitRun::Emitted { .. } => CaseOutcome::Pass,
        JavaEmitRun::Panicked => CaseOutcome::FailWrongValue,
    }
}
