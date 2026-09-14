//! Fixtures, extraction, and multi-backend runner for the upstream JUnit suite.

pub mod extract;
pub mod fmt_ratchet;
pub mod reference;

pub use fmt_ratchet::{
    FmtFailure, FmtRatchet, FmtRatchetDiff, KIND_NOT_IDEMPOTENT, KIND_UNSAFE, KIND_UNSAFE_REFORMAT,
    diff_fmt_ratchet, first_difference,
};

pub use leek_test_driver::{
    CaseAudit, CaseChecks, CasePlan, CheckKind, Expectation, Manifest, MultiReport, SuiteBackend,
    TestCase,
    audit::audit_case,
    backends::{self, RunConfig, Shard},
    cases, checks, run,
};

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Manifest embedded at build time. `build.rs` re-extracts it from the
/// upstream Java sources into `OUT_DIR/upstream_cases.toml` on every
/// build, so no copy is committed.
pub fn embedded_manifest() -> &'static Manifest {
    static CACHE: OnceLock<Manifest> = OnceLock::new();
    CACHE.get_or_init(|| {
        const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upstream_cases.toml"));
        toml::from_str(std::str::from_utf8(BYTES).expect("upstream_cases.toml must be utf-8"))
            .expect("malformed embedded upstream_cases.toml")
    })
}

pub fn upstream_fixtures_dir() -> PathBuf {
    try_upstream_fixtures_dir()
        .expect("upstream fixtures dir missing; vendored submodule not checked out")
}

/// [`upstream_fixtures_dir`], or `None` when the submodule is not checked out.
fn try_upstream_fixtures_dir() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("official-generator/leek-wars-generator/leekscript/src/test/resources/ai")
        .canonicalize()
        .ok()
}

pub fn upstream_fixture(rel: &str) -> String {
    let path = upstream_fixtures_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e))
}

/// Whether the vendored upstream fixture files are on disk.
///
/// Every fixture suite has to ask: a non-recursive clone has no fixtures, and
/// a suite that panicked on the missing directory would report a checkout
/// choice as a test failure. [`upstream_fixture`] and [`upstream_fixtures_dir`]
/// both panic when this is `false`, so guard with it and skip.
///
/// This asks the filesystem, deliberately. `embedded_manifest().cases` is not
/// the same question and is not a safe proxy for it: the manifest is baked
/// into `OUT_DIR` at build time, so a target directory shared with a checkout
/// that *does* have the submodule hands this crate a populated manifest while
/// the fixture files are still missing here.
#[must_use]
pub fn upstream_fixtures_available() -> bool {
    try_upstream_fixtures_dir().is_some()
}

/// Every `.leek` file under `dir`, recursively, in a stable order.
///
/// Shared by the suites that sweep the upstream fixtures whole
/// (`tests/parser_fixtures.rs`, `tests/fmt_roundtrip.rs`) so they cannot
/// disagree about what "every fixture" means.
pub fn leek_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .unwrap_or_else(|e| panic!("reading {}: {e}", current.display()));
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "leek") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Path of `path` relative to the upstream fixtures directory, with
/// forward slashes — the stable id for a fixture.
///
/// The absolute path carries the checkout root, which differs between a
/// developer's machine and the CI runner, so anything tracked (or merely
/// reported) has to be keyed on this instead.
pub fn fixture_id(path: &Path) -> String {
    path.strip_prefix(upstream_fixtures_dir())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub fn upstream_tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("official-generator/leek-wars-generator/leekscript/src/test/java/test")
        .canonicalize()
        .expect("upstream tests dir missing; vendored submodule not checked out")
}

/// Per-backend baseline of *non-passing* outcomes (`run --save-baseline`).
pub fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data/baseline.toml")
}

/// The formatter's known-bad list for one suite (`corpus` or `ai`). See
/// [`fmt_ratchet`] and `tests/fmt_roundtrip.rs`.
pub fn fmt_known_failures_path(suite: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("data/fmt-known-failures-{suite}.tsv"))
}

pub fn suite_backends() -> Vec<SuiteBackend> {
    let miku = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("Miku.toml");
    let table = miku
        .exists()
        .then(|| {
            leek_manifest::load_from(&miku)
                .ok()
                .map(|load| load.manifest.backend)
        })
        .flatten();
    backends::detect_backends(table.as_ref())
}

/// Worker stack for the full upstream suite (some cases recurse very deeply).
/// Aliased, not copied, from the stack the driver gives each of its corpus
/// workers — the two must not drift.
pub use leek_test_driver::backends::WORKER_STACK as UPSTREAM_SUITE_STACK;

/// Run `f` on a thread with [`UPSTREAM_SUITE_STACK`] — avoids main-thread stack overflow.
pub fn run_on_large_stack<F, T>(name: &str, f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(UPSTREAM_SUITE_STACK)
        .spawn(f)
        .unwrap_or_else(|e| panic!("spawn {name} worker: {e}"))
        .join()
        .unwrap_or_else(|_| panic!("{name} worker panicked"))
}

pub fn run_manifest_on_large_stack(manifest: &Manifest, backends: &[SuiteBackend]) -> MultiReport {
    run_manifest_on_large_stack_with(manifest, backends, RunConfig::default())
}

/// As [`run_manifest_on_large_stack`], with an explicit worker count / shard.
/// The outer big-stack thread is kept even though the pool spawns its own
/// [`UPSTREAM_SUITE_STACK`] workers: it costs one thread and keeps every
/// caller of `run_on_large_stack` on the same footing.
pub fn run_manifest_on_large_stack_with(
    manifest: &Manifest,
    backends: &[SuiteBackend],
    cfg: RunConfig,
) -> MultiReport {
    let manifest = manifest.clone();
    let backends = backends.to_vec();
    run_on_large_stack("upstream-suite", move || {
        backends::run_manifest_with(&manifest, &backends, cfg)
    })
}

pub fn run_upstream_suite() -> MultiReport {
    run_on_large_stack("upstream-suite", || {
        let backends = suite_backends();
        let cfg = RunConfig::default();
        eprintln!(
            "upstream suite backends: {} ({} worker(s); override with {}=N)",
            backends
                .iter()
                .map(|b| b.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            cfg.jobs,
            backends::JOBS_ENV,
        );
        backends::run_manifest_with(embedded_manifest(), &backends, cfg)
    })
}
