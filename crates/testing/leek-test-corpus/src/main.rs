//! `leek-test-corpus` — one executable for the upstream JUnit corpus.
//!
//! Case extraction now lives entirely in `build.rs` (it embeds the
//! manifest at compile time), so the former `extract` / `enrich`
//! binaries are gone. What remains are the run / inspect commands:
//!
//! ```text
//! cargo run -p leek-test-corpus -- run [--save-baseline|--check-baseline|--manifest=PATH]
//! cargo run -p leek-test-corpus -- failures [BACKEND] [CATEGORY]
//! cargo run -p leek-test-corpus -- unknown            # histogram of un-extracted expectations
//! ```

// `prepare_embed` / `HEADER` are only reached from `build.rs`; silence
// the binary-context dead-code lint for the build-only helpers.
#[allow(dead_code)]
mod reference;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use leek_span::SourceId;
use leek_test_corpus::backends::{
    self, CaseProbe, FailureCategory, SuiteBackend, categorize_failure, probe_case,
};
use leek_test_corpus::cases::Expectation;
use leek_test_corpus::run::Summary;
use leek_test_corpus::{
    Manifest, MultiReport, TestCase, baseline_path, embedded_manifest, run_manifest_on_large_stack,
    run_on_large_stack, run_upstream_suite, suite_backends,
};

const USAGE: &str = "\
leek-test-corpus — upstream JUnit corpus runner

USAGE:
    cargo run -p leek-test-corpus -- <COMMAND> [ARGS]

COMMANDS:
    run      [--save-baseline]    Run the suite on every linked backend
             [--check-baseline]   Fail on regressions vs the saved baseline
             [--manifest=PATH]
    failures [BACKEND] [CATEGORY] Categorized table of failing cases (expected vs actual)
             [--run]              Read the saved baseline by default; --run does a fresh run
    unknown                       Histogram of expectations we couldn't extract
    native-skips                  Histogram of why the native backend skips cases it attempts
    extract-reference             Run the official LeekScript suite (JDK 25) to refresh the
                                  committed reference dataset (value + ops + Java per case)
    help                          Show this message

FAILURES FILTERS:
    BACKEND   one of: pipeline | interp | java
    CATEGORY  substring of a category label, e.g. `value`, `ops`, `compile`, `missing`";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let result = match cmd.as_str() {
        "run" => cmd_run(rest),
        "failures" => cmd_failures(rest),
        "unknown" => {
            cmd_unknown();
            Ok(())
        }
        "native-skips" => {
            cmd_native_skips(rest);
            Ok(())
        }
        "extract-reference" => cmd_extract_reference(),
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(anyhow::anyhow!("unknown subcommand `{other}`\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

// ───────────────────────── run ─────────────────────────

fn cmd_run(args: &[String]) -> Result<()> {
    let save = args.iter().any(|a| a == "--save-baseline");
    let check = args.iter().any(|a| a == "--check-baseline");
    let manifest_arg = args.iter().find_map(|a| a.strip_prefix("--manifest="));

    let multi = if let Some(path) = manifest_arg {
        let manifest = Manifest::load(std::path::Path::new(path))
            .with_context(|| format!("loading manifest from {path}"))?;
        eprintln!("loaded {} cases from {}", manifest.cases.len(), path);
        let backends = suite_backends();
        eprintln!("upstream suite backends: {}", backend_list(&backends));
        run_manifest_on_large_stack(&manifest, &backends)
    } else {
        run_upstream_suite()
    };

    for (name, report) in &multi.backends {
        println!("\n─── backend: {name} ───");
        print_summary(&report.summary);
    }

    if save {
        multi.save(&baseline_path())?;
        eprintln!("\nbaseline written to {}", baseline_path().display());
    }

    if check {
        let baseline = MultiReport::load(&baseline_path())
            .with_context(|| format!("loading baseline {}", baseline_path().display()))?;
        let diff = multi.diff_against(&baseline);
        let mut regressions = 0usize;
        for (backend, regs) in &diff.regressions {
            if !regs.is_empty() {
                regressions += regs.len();
                eprintln!("\n!! {backend}: {} regressions:", regs.len());
                for c in regs.iter().take(10) {
                    eprintln!("  {} : {:?} -> {:?}", c.id, c.before, c.after);
                }
            }
        }
        if regressions > 0 {
            bail!("{regressions} regression(s) vs baseline");
        }
        eprintln!("\nno regressions across backends");
    }

    Ok(())
}

fn backend_list(backends: &[SuiteBackend]) -> String {
    backends
        .iter()
        .map(|b| b.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_summary(s: &Summary) {
    let active = s.active_total().max(1);
    let pct = |n: u32| f64::from(n) * 100.0 / f64::from(active);
    println!(" total:    {}", s.total);
    if s.skipped_disabled > 0 {
        println!(" disabled: {:>5} (excluded from pass/fail/skip rates)", s.skipped_disabled);
    }
    println!(" pass:     {:>5} ({:.1}%)", s.pass_total(), pct(s.pass_total()));
    println!(" fail:     {:>5} ({:.1}%)", s.fail_total(), pct(s.fail_total()));
    if s.skipped_unknown > 0 {
        println!(" skipped:  {:>5} ({:.1}%)", s.skipped_unknown, pct(s.skipped_unknown));
    }
}

// ───────────────────────── failures ─────────────────────────

fn cmd_failures(args: &[String]) -> Result<()> {
    let force_run = args.iter().any(|a| a == "--run");
    let mut backend_filter: Option<SuiteBackend> = None;
    let mut category_filter: Option<String> = None;
    for a in args {
        if a.starts_with('-') {
            continue;
        }
        if let Some(b) = SuiteBackend::parse(a) {
            backend_filter = Some(b);
        } else {
            category_filter = Some(a.to_lowercase());
        }
    }

    // Run on the big worker stack: a handful of corpus cases recurse
    // deeply enough to blow the 8 MB main-thread stack, and the
    // per-case probe re-runs the interpreter on the same inputs.
    let report = run_on_large_stack("failures", move || {
        build_failure_report(backend_filter, category_filter, force_run)
    });
    print!("{report}");
    Ok(())
}

fn build_failure_report(
    backend_filter: Option<SuiteBackend>,
    category_filter: Option<String>,
    force_run: bool,
) -> String {
    let manifest = embedded_manifest();
    let by_id: BTreeMap<&str, &TestCase> =
        manifest.cases.iter().map(|c| (c.id.as_str(), c)).collect();
    let src = SourceId::new(1).unwrap();

    let mut out = String::new();
    let _ = writeln!(out, "\n══════════ upstream suite — failure report ══════════");

    // Default to the saved baseline (instant). A full re-run of ~10k
    // cases × 3 backends is minutes long, so we only do it on `--run`
    // (or when no baseline exists). Per-case probes below still run
    // the *current* code, so expected-vs-actual reflects HEAD.
    let backends = match backend_filter {
        Some(b) => vec![b],
        None => suite_backends(),
    };
    let multi = if force_run {
        let _ = writeln!(out, "  source: fresh run on {}", backend_list(&backends));
        backends::run_manifest(manifest, &backends)
    } else if let Ok(m) = MultiReport::load(&baseline_path()) {
        let _ = writeln!(
            out,
            "  source: baseline {} (may be stale — `failures --run` to re-run, \
             `run --save-baseline` to refresh)",
            baseline_path().display()
        );
        m
    } else {
        let _ = writeln!(out, "  no baseline found — fresh run on {}", backend_list(&backends));
        backends::run_manifest(manifest, &backends)
    };

    write_overall_table(&mut out, &multi);

    for (name, report) in &multi.backends {
        let Some(sb) = SuiteBackend::parse(name) else {
            continue;
        };
        if backend_filter.is_some_and(|bf| bf != sb) {
            continue;
        }

        // Bucket every failing case by category.
        let mut by_cat: BTreeMap<FailureCategory, Vec<&str>> = BTreeMap::new();
        for (id, &outcome) in &report.outcomes {
            if !outcome.is_fail() {
                continue;
            }
            let Some(case) = by_id.get(id.as_str()) else {
                continue;
            };
            if let Some(cat) = categorize_failure(outcome, &case.expected) {
                by_cat.entry(cat).or_default().push(id.as_str());
            }
        }
        if by_cat.is_empty() {
            continue;
        }

        let total: usize = by_cat.values().map(Vec::len).sum();
        let _ = writeln!(out, "\n──────── {name} · {total} failures ────────");
        write_category_table(&mut out, &by_cat);

        // Detailed listing, grouped by category, with expected vs actual.
        for cat in FailureCategory::ALL {
            let Some(ids) = by_cat.get(&cat) else {
                continue;
            };
            if category_filter
                .as_deref()
                .is_some_and(|f| !cat.label().contains(f))
            {
                continue;
            }
            let _ = writeln!(out, "\n  [{}]  ({} cases)", cat.label(), ids.len());
            for id in ids {
                let case = by_id[id];
                let CaseProbe { expected, actual } = probe_case(case, src, sb);
                let _ = writeln!(out, "    {id}");
                let _ = writeln!(out, "        expected: {}", truncate(&expected, 160));
                let _ = writeln!(out, "        actual:   {}", truncate(&actual, 160));
                let _ = writeln!(out, "        code:     {}", truncate(&case.code, 120));
            }
        }
    }
    out
}

fn write_overall_table(out: &mut String, multi: &MultiReport) {
    let _ = writeln!(out, "\n  {:<10} {:>8} {:>8} {:>8} {:>8}", "backend", "active", "pass", "fail", "skip");
    let _ = writeln!(out, "  {:-<10} {:->8} {:->8} {:->8} {:->8}", "", "", "", "", "");
    for (name, report) in &multi.backends {
        let s = &report.summary;
        let _ = writeln!(
            out,
            "  {:<10} {:>8} {:>8} {:>8} {:>8}",
            name,
            s.active_total(),
            s.pass_total(),
            s.fail_total(),
            s.skipped_unknown,
        );
    }
}

fn write_category_table(out: &mut String, by_cat: &BTreeMap<FailureCategory, Vec<&str>>) {
    let _ = writeln!(out, "  {:<20} {:>6}", "category", "count");
    let _ = writeln!(out, "  {:-<20} {:->6}", "", "");
    for cat in FailureCategory::ALL {
        if let Some(ids) = by_cat.get(&cat) {
            let _ = writeln!(out, "  {:<20} {:>6}", cat.label(), ids.len());
        }
    }
}

// ───────────────────────── unknown ─────────────────────────

fn cmd_unknown() {
    let m = embedded_manifest();
    let mut hist: BTreeMap<String, u32> = BTreeMap::new();
    let mut sample: BTreeMap<String, String> = BTreeMap::new();
    for c in &m.cases {
        if let Expectation::Unknown { detail } = &c.expected {
            *hist.entry(detail.clone()).or_default() += 1;
            sample.entry(detail.clone()).or_insert_with(|| c.code.clone());
        }
    }
    let mut v: Vec<_> = hist.iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (k, n) in v.iter().take(25) {
        let s = sample.get(*k).map(|s| s.replace('\n', "\\n")).unwrap_or_default();
        println!("{:6} {}  | {}", n, k, truncate(&s, 80));
    }
}

// ───────────────────────── native-skips ─────────────────────────

fn cmd_native_skips(rest: &[String]) {
    let m = embedded_manifest();
    // With a filter argument, dump every attempted case whose skip reason
    // contains the substring — full source + expected value — for triage.
    if let Some(filter) = rest.first() {
        let cases = backends::native_skips_matching(m, filter);
        println!("native skips matching {filter:?} — {} case(s)\n", cases.len());
        for (i, c) in cases.iter().enumerate() {
            println!("── case {i} [{}] expect={} ──", c.reason, c.expected);
            println!("{}\n", c.code);
        }
        return;
    }
    let rows = backends::native_skip_histogram(m);
    let total: u32 = rows.iter().map(|r| r.count).sum();
    println!("native skip reasons (attempted Equals cases) — {total} skipped\n");
    println!("  {:>6}  {:<28}  sample", "count", "reason");
    println!("  {:->6}  {:-<28}  {:-<40}", "", "", "");
    for r in &rows {
        let sample = r.sample.replace('\n', " ");
        println!("  {:>6}  {:<28}  {}", r.count, r.reason, truncate(&sample, 60));
    }
}

// ───────────────────────── extract-reference ─────────────────────────

fn cmd_extract_reference() -> Result<()> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = reference::committed_path(manifest_dir);

    if !reference::submodule_present(manifest_dir) {
        bail!("upstream submodule not checked out — cannot run the official LeekScript suite");
    }
    if !reference::jvm_available() {
        bail!("`java`/`javac` not found on PATH — install JDK 25 to run the official suite");
    }

    eprintln!("running the official LeekScript suite to extract value + ops + Java per case…");
    eprintln!("(this takes a few minutes; see /tmp/reference-run.log for progress)");
    reference::regenerate(manifest_dir, &out).map_err(|e| anyhow::anyhow!(e))?;

    let text =
        std::fs::read_to_string(&out).with_context(|| format!("reading {}", out.display()))?;
    let mut by_kind: BTreeMap<String, u32> = BTreeMap::new();
    let mut rows = 0u32;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        rows += 1;
        if let Some(kind) = line.split('\t').nth(2) {
            *by_kind.entry(kind.to_string()).or_default() += 1;
        }
    }
    eprintln!("\nwrote {} ({rows} rows)", out.display());
    for (kind, n) in &by_kind {
        eprintln!("  {kind:<8} {n}");
    }
    eprintln!("\nRebuild `leek-test-corpus` to embed the refreshed dataset, then commit it.");
    Ok(())
}

// ───────────────────────── shared ─────────────────────────

fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', "\\n");
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
