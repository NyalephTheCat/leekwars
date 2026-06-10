//! Differential semantic verification of `leek-migrate` against the
//! embedded upstream corpus.
//!
//! The bar: for any source that runs under version X, `migrate_text`
//! to version Y must produce a source that runs under Y with the
//! SAME runtime value (`Value::loose_eq`). For every enabled,
//! non-error corpus case we:
//!
//!  1. Run the original at its own version on the native JIT — that
//!     value is the baseline. Cases native can't run are skipped.
//!  2. Migrate to every other version (or just adjacent ones with
//!     `--adjacent`), run the migrated source at the target version,
//!     and compare against the baseline.
//!
//! Mismatches where the migration emitted NO diagnostic are silent
//! semantic breaks — the bug list this harness exists to produce.
//! Mismatches WITH a diagnostic are at least flagged for manual
//! review; they're tallied separately.
//!
//! Usage (always build with --release; the corpus is 10k+ cases):
//!   cargo run --release -p leek-migrate --example corpus_verify
//!     [--adjacent]        only migrate to v±1, not all other versions
//!     [--up | --down]     restrict migration direction
//!     [--chunk=I/N]       run the I-th of N case slices (0-based)
//!     [--case=SUBSTR]     only cases whose id contains SUBSTR (verbose)
//!     [--max-failures=N]  print at most N mismatch details per direction

use std::collections::BTreeMap;
use std::sync::Arc;

use leek_diagnostics::Severity;
use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_migrate::migrate_text;
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target};
use leek_span::SourceId;
use leek_syntax::Version;
use leek_test_corpus::{TestCase, embedded_manifest, run_on_large_stack};

/// Op budget for both sides of every comparison. Generous enough
/// that no value-producing corpus case trips it, small enough that a
/// migration bug which manufactures an infinite loop can't hang the
/// harness. Identical on both sides so the budget itself can't cause
/// an asymmetric verdict.
const OP_LIMIT: u64 = 50_000_000;

fn version_of(byte: u8) -> Option<Version> {
    Some(match byte {
        1 => Version::V1,
        2 => Version::V2,
        3 => Version::V3,
        4 => Version::V4,
        _ => return None,
    })
}

/// Frontend build, mirroring the corpus harness (`build_context` in
/// leek-test-driver): full pipeline to HIR with permissive params.
struct Built {
    hir: Option<Arc<HirFile>>,
    compile_error: bool,
    first_error: Option<String>,
}

fn build(text: &str, version: u8, strict: bool) -> Built {
    let input = Input {
        source: SourceId::new(1).unwrap(),
        text: text.to_string().into(),
        version_byte: version,
        strict,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let pipeline =
        leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive()).expect("recipe");
    let run = pipeline.run(input);
    let first_error = run
        .diagnostics()
        .iter()
        .find(|d| d.severity == Severity::Error)
        .map(|d| format!("[{}] {}", d.code.0, d.message));
    let hir = run.get::<HirArtifact>().map(|a| Arc::clone(&a.0));
    Built {
        hir,
        compile_error: first_error.is_some(),
        first_error,
    }
}

enum Run {
    Value(leek_runtime::Value),
    /// Compile error from the shared frontend.
    CompileError(String),
    /// Native couldn't run it (unsupported construct) — skip.
    Unsupported,
    RuntimeError(String),
    Panicked(String),
}

fn run_at(text: &str, version: u8, strict: bool) -> Run {
    let built = build(text, version, strict);
    if built.compile_error {
        return Run::CompileError(
            built
                .first_error
                .unwrap_or_else(|| "<no message>".to_string()),
        );
    }
    let Some(hir) = built.hir.as_deref() else {
        return Run::Unsupported;
    };
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(version));
    let opts = leek_backend_native::NativeOptions::release()
        .with_lang(version, strict)
        .with_op_limit(OP_LIMIT);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        leek_backend_native::run(hir, &opts)
    }));
    match result {
        Ok(Ok(v)) => Run::Value(v),
        Ok(Err(leek_backend_native::NativeError::Runtime(m))) => Run::RuntimeError(m),
        Ok(Err(_)) => Run::Unsupported,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".to_string());
            Run::Panicked(msg)
        }
    }
}

/// Tally for one migration direction (e.g. "v1->v2").
#[derive(Default)]
struct DirStats {
    /// Baseline ran, migrated ran, values loose_eq — the good case.
    equal: u64,
    /// Values differ and the migration was silent: a real bug.
    silent_mismatch: u64,
    /// Values differ but the migration flagged the construct.
    flagged_mismatch: u64,
    /// Migrated source no longer compiles at the target version,
    /// migration was silent: a real bug (different kind).
    silent_break: u64,
    /// Migrated source breaks, but the migration flagged it.
    flagged_break: u64,
    /// Migrated program trapped or panicked at runtime.
    silent_crash: u64,
    flagged_crash: u64,
    /// Baseline unavailable (native unsupported / compile error /
    /// runtime error on the ORIGINAL) — outside migration's control.
    baseline_skip: u64,
    /// Details for the printable failures, capped by --max-failures.
    failures: Vec<String>,
}

struct Options {
    adjacent: bool,
    up: bool,
    down: bool,
    chunk: Option<(usize, usize)>,
    case_filter: Option<String>,
    max_failures: usize,
}

fn parse_args() -> Options {
    let mut o = Options {
        adjacent: false,
        up: true,
        down: true,
        chunk: None,
        case_filter: None,
        max_failures: 30,
    };
    for arg in std::env::args().skip(1) {
        if arg == "--adjacent" {
            o.adjacent = true;
        } else if arg == "--up" {
            o.down = false;
        } else if arg == "--down" {
            o.up = false;
        } else if let Some(rest) = arg.strip_prefix("--chunk=") {
            let (i, n) = rest.split_once('/').expect("--chunk=I/N");
            o.chunk = Some((i.parse().expect("chunk idx"), n.parse().expect("chunk cnt")));
        } else if let Some(rest) = arg.strip_prefix("--case=") {
            o.case_filter = Some(rest.to_string());
        } else if let Some(rest) = arg.strip_prefix("--max-failures=") {
            o.max_failures = rest.parse().expect("max-failures");
        } else {
            panic!("unknown arg: {arg}");
        }
    }
    o
}

fn targets_for(from: u8, opts: &Options) -> Vec<u8> {
    let mut out = Vec::new();
    for to in 1..=4u8 {
        if to == from {
            continue;
        }
        if to > from && !opts.up {
            continue;
        }
        if to < from && !opts.down {
            continue;
        }
        if opts.adjacent && to.abs_diff(from) != 1 {
            continue;
        }
        out.push(to);
    }
    out
}

fn render(v: &leek_runtime::Value, display_version: u8) -> String {
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(display_version));
    v.to_string()
}

/// Two runs match when they're `loose_eq` OR render to the same
/// string under the same display version. The string fallback covers
/// objects/instances, which `loose_eq` compares by reference identity
/// (`new A() == new A()` is false) — across two separate JIT runs
/// every object would spuriously mismatch without it.
fn values_match(a: &leek_runtime::Value, b: &leek_runtime::Value, display_version: u8) -> bool {
    a.loose_eq(b) || render(a, display_version) == render(b, display_version)
}

fn verify_case(case: &TestCase, opts: &Options, stats: &mut BTreeMap<String, DirStats>) {
    let from = case.version;
    let targets = targets_for(from, opts);
    if targets.is_empty() {
        return;
    }

    // Baseline: the original, at its own version.
    let Run::Value(baseline) = run_at(&case.code, from, case.strict) else {
        // No baseline — record the skip under every direction this
        // case would have exercised.
        for to in targets {
            stats
                .entry(format!("v{from}->v{to}"))
                .or_default()
                .baseline_skip += 1;
        }
        return;
    };

    for to in targets {
        let dir = format!("v{from}->v{to}");
        let entry = stats.entry(dir.clone()).or_default();
        let (from_v, to_v) = (version_of(from).unwrap(), version_of(to).unwrap());

        let migrated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            migrate_text(&case.code, SourceId::new(1).unwrap(), from_v, to_v)
        }));
        let Ok(migrated) = migrated else {
            entry.silent_break += 1;
            if entry.failures.len() < opts.max_failures {
                entry
                    .failures
                    .push(format!("{}: migrate_text PANICKED", case.id));
            }
            continue;
        };
        let flagged = !migrated.diagnostics.is_empty();

        match run_at(&migrated.text, to, case.strict) {
            Run::Value(after) => {
                if values_match(&baseline, &after, from) {
                    entry.equal += 1;
                } else {
                    if flagged {
                        entry.flagged_mismatch += 1;
                    } else {
                        entry.silent_mismatch += 1;
                    }
                    if entry.failures.len() < opts.max_failures {
                        entry.failures.push(format!(
                            "{}{}: value drift\n  before: {}\n  after:  {}\n  code: {}",
                            case.id,
                            if flagged { " (flagged)" } else { "" },
                            render(&baseline, from),
                            render(&after, from),
                            snippet(&case.code),
                        ));
                    }
                }
            }
            Run::CompileError(msg) => {
                if flagged {
                    entry.flagged_break += 1;
                } else {
                    entry.silent_break += 1;
                }
                if entry.failures.len() < opts.max_failures {
                    entry.failures.push(format!(
                        "{}{}: migrated source no longer compiles: {}\n  code: {}",
                        case.id,
                        if flagged { " (flagged)" } else { "" },
                        msg,
                        snippet(&case.code),
                    ));
                }
            }
            Run::Unsupported => {
                // Native ran the original but not the migrated form —
                // count as a skip, not a verdict.
                entry.baseline_skip += 1;
            }
            Run::RuntimeError(msg) | Run::Panicked(msg) => {
                if flagged {
                    entry.flagged_crash += 1;
                } else {
                    entry.silent_crash += 1;
                }
                if entry.failures.len() < opts.max_failures {
                    entry.failures.push(format!(
                        "{}{}: migrated program crashed: {}\n  code: {}",
                        case.id,
                        if flagged { " (flagged)" } else { "" },
                        msg,
                        snippet(&case.code),
                    ));
                }
            }
        }
    }
}

fn snippet(code: &str) -> String {
    let flat = code.replace('\n', " ⏎ ");
    if flat.chars().count() > 160 {
        let cut: String = flat.chars().take(160).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

fn main() {
    let opts = parse_args();
    run_on_large_stack("corpus-verify", move || {
        let manifest = embedded_manifest();
        let mut cases: Vec<&TestCase> = manifest
            .cases
            .iter()
            .filter(|c| c.enabled && (1..=4).contains(&c.version))
            .filter(|c| !c.expected.implies_error())
            .filter(|c| opts.case_filter.as_deref().is_none_or(|f| c.id.contains(f)))
            .collect();
        if let Some((i, n)) = opts.chunk {
            cases = cases
                .into_iter()
                .enumerate()
                .filter(|(idx, _)| idx % n == i)
                .map(|(_, c)| c)
                .collect();
        }

        let total = cases.len();
        eprintln!("verifying {total} cases…");
        let mut stats: BTreeMap<String, DirStats> = BTreeMap::new();
        for (i, case) in cases.iter().enumerate() {
            if i % 500 == 0 && i > 0 {
                eprintln!("  …{i}/{total}");
            }
            verify_case(case, &opts, &mut stats);
        }

        let mut bad_total = 0u64;
        println!("\n=== migration differential report ===");
        for (dir, s) in &stats {
            let compared = s.equal
                + s.silent_mismatch
                + s.flagged_mismatch
                + s.silent_break
                + s.flagged_break
                + s.silent_crash
                + s.flagged_crash;
            let silent_bad = s.silent_mismatch + s.silent_break + s.silent_crash;
            bad_total += silent_bad;
            println!(
                "{dir}: compared={compared} equal={} | SILENT bad: mismatch={} break={} crash={} | flagged: mismatch={} break={} crash={} | baseline-skip={}",
                s.equal,
                s.silent_mismatch,
                s.silent_break,
                s.silent_crash,
                s.flagged_mismatch,
                s.flagged_break,
                s.flagged_crash,
                s.baseline_skip,
            );
        }
        for (dir, s) in &stats {
            if s.failures.is_empty() {
                continue;
            }
            println!("\n--- {dir} failures (first {}) ---", s.failures.len());
            for f in &s.failures {
                println!("{f}");
            }
        }
        println!("\ntotal silent semantic breaks: {bad_total}");
        if bad_total > 0 {
            std::process::exit(1);
        }
    });
}
