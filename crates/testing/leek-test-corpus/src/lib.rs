//! Fixtures, extraction, and multi-backend runner for the upstream JUnit suite.

pub mod extract;

pub use leek_test_driver::{
    CaseAudit, CasePlan, CheckKind, Expectation, Manifest, MultiReport, SuiteBackend, TestCase,
    audit::audit_case, backends, cases, checks, run,
};

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Manifest embedded at build time from `upstream_cases.toml`.
pub fn embedded_manifest() -> &'static Manifest {
    static CACHE: OnceLock<Manifest> = OnceLock::new();
    CACHE.get_or_init(|| {
        const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/upstream_cases.toml"));
        toml::from_str(std::str::from_utf8(BYTES).expect("upstream_cases.toml must be utf-8"))
            .expect("malformed embedded upstream_cases.toml")
    })
}

/// Official-LeekScript reference dataset embedded at build time (TSV:
/// `version, strict, kind, value, jvm_ops, code, java`). Empty (header
/// only) when no JDK / upstream submodule was available at build time —
/// run `cargo run -p leek-test-corpus -- extract-reference` to populate.
/// See `src/reference.rs` for the gated-regen policy.
pub fn embedded_reference() -> &'static str {
    include_str!(concat!(env!("OUT_DIR"), "/reference.tsv"))
}

pub fn upstream_fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("official-generator/leek-wars-generator/leekscript/src/test/resources/ai")
        .canonicalize()
        .expect("upstream fixtures dir missing; vendored submodule not checked out")
}

pub fn upstream_fixture(rel: &str) -> String {
    let path = upstream_fixtures_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e))
}

pub fn upstream_tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("official-generator/leek-wars-generator/leekscript/src/test/java/test")
        .canonicalize()
        .expect("upstream tests dir missing; vendored submodule not checked out")
}

pub fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data/upstream_cases.toml")
}

pub fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("data/baseline.toml")
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
pub const UPSTREAM_SUITE_STACK: usize = 64 * 1024 * 1024;

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
    let manifest = manifest.clone();
    let backends = backends.to_vec();
    run_on_large_stack("upstream-suite", move || {
        backends::run_manifest(&manifest, &backends)
    })
}

pub fn run_upstream_suite() -> MultiReport {
    run_on_large_stack("upstream-suite", || {
        let backends = suite_backends();
        eprintln!(
            "upstream suite backends: {}",
            backends
                .iter()
                .map(|b| b.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        backends::run_manifest(embedded_manifest(), &backends)
    })
}
