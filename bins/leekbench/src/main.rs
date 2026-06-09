//! `leekbench` — compare backend execution speed.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use comfy_table::{Cell, CellAlignment, ContentArrangement, Row, Table, presets};
use leek_bench::{BenchOptions, BenchSummary, RustNative, RustJavaEmit, UpstreamJava, bench};
use leek_test_corpus::cases::{Expectation, Manifest, TestCase};
use leek_test_corpus::embedded_manifest;
mod cli;
use cli::{Cli, CorpusExpectation};

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.corpus {
        if cli.fast_java {
            run_corpus_fast_java(&cli)
        } else {
            run_corpus(&cli)
        }
    } else {
        let input = cli
            .input
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing input file (or pass --corpus)"))?;
        run_single(&cli, &input)
    }
}

// ---------------------------------------------------------------------
// Single-file mode
// ---------------------------------------------------------------------

fn run_single(cli: &Cli, input: &PathBuf) -> Result<()> {
    let opts = BenchOptions {
        runs: cli.runs.max(1),
        version: cli.lang_version,
        strict: false,
    };
    let mut summaries: Vec<(String, Result<BenchSummary>)> = Vec::new();

    let mut nat = RustNative::new();
    summaries.push((nat.name_str(), bench(&mut nat, input, &opts)));
    if !cli.no_rust_java {
        let mut rj = RustJavaEmit::auto();
        summaries.push((rj.name_str(), bench(&mut rj, input, &opts)));
    }
    if !cli.no_upstream {
        let mut up = UpstreamJava::auto();
        summaries.push((up.name_str(), bench(&mut up, input, &opts)));
    }

    let baseline = summaries
        .iter()
        .filter_map(|(_, r)| r.as_ref().ok())
        .map(|s| s.warm_median)
        .min()
        .unwrap_or(Duration::from_nanos(1));
    let first_value = summaries
        .iter()
        .find_map(|(_, r)| r.as_ref().ok())
        .map(|s| s.stdout_sample.clone());

    println!("Run times are inner program time only — JVM startup excluded.");
    let mut table = make_table(&[
        "backend",
        "cold",
        "warm_med",
        "warm_min",
        "warm_p95",
        "vs_best/share",
        "result",
    ]);
    for (name, r) in &summaries {
        match r {
            Ok(s) => {
                let warm_min = s.warm_runs.first().copied().unwrap_or(s.warm_median);
                let warm_p95 = pctile(&s.warm_runs, 95).unwrap_or(s.warm_median);
                let ratio = s.warm_median.as_secs_f64() / baseline.as_secs_f64();
                let agree = match &first_value {
                    Some(v) => {
                        if v == &s.stdout_sample {
                            "✓"
                        } else {
                            "✗"
                        }
                    }
                    None => " ",
                };
                table.add_row(Row::from(vec![
                    Cell::new(s.backend),
                    right(fmt(s.cold)),
                    right(fmt(s.warm_median)),
                    right(fmt(warm_min)),
                    right(fmt(warm_p95)),
                    right(format!("{ratio:.2}×")),
                    Cell::new(format!("{agree} {}", truncate(&s.stdout_sample, 30))),
                ]));
                if cli.detail && !s.prepare_steps.is_empty() {
                    push_step_rows(&mut table, &s.prepare_steps);
                }
            }
            Err(e) => {
                table.add_row(Row::from(vec![
                    Cell::new(name),
                    Cell::new(format!("skipped: {e:#}")).set_alignment(CellAlignment::Left),
                ]));
            }
        }
    }
    println!("{table}");
    if !cli.detail
        && summaries.iter().any(|(_, r)| {
            r.as_ref()
                .map(|s| !s.prepare_steps.is_empty())
                .unwrap_or(false)
        })
    {
        println!("(pass --detail to see per-step prepare timings)");
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Corpus mode
// ---------------------------------------------------------------------

/// Fast batch rust-java correctness sweep (`--corpus --fast-java`): one `javac`,
/// one JVM, values only. See [`leek_bench::run_fast_java_corpus`].
fn run_corpus_fast_java(cli: &Cli) -> Result<()> {
    let manifest_owned;
    let manifest = if let Some(path) = &cli.manifest {
        manifest_owned =
            Manifest::load(path).with_context(|| format!("load manifest {}", path.display()))?;
        &manifest_owned
    } else {
        embedded_manifest()
    };
    // Only `equals(...)` cases carry a comparable value.
    let cases: Vec<leek_bench::FastCase> = manifest
        .cases
        .iter()
        .filter(|c| cli.include_disabled || c.enabled)
        .filter(|c| expectation_matches(cli.corpus_expectation, c))
        .filter(|c| case_matches_filter(c, cli.case_filter.as_deref()))
        .filter_map(|c| match &c.expected {
            Expectation::Equals { value } => Some(leek_bench::FastCase {
                id: c.id.clone(),
                code: c.code.clone(),
                version: c.version,
                strict: c.strict,
                expected: value.clone(),
            }),
            _ => None,
        })
        .take(cli.limit.max(1))
        .collect();

    eprintln!("fast rust-java sweep: {} cases …", cases.len());
    let report = leek_bench::run_fast_java_corpus(&cases, cli.corpus_lang_version)?;

    println!(
        "\n{} cases in {:.1}s ({} javac round(s))",
        report.total,
        report.elapsed.as_secs_f64(),
        report.javac_rounds,
    );
    println!("  agree:     {} / {}", report.agree, report.total);
    println!("  disagree:  {}", report.disagree());
    println!("  errors:    {}", report.errors());
    if cli.verbose {
        for (id, outcome) in &report.failures {
            match outcome {
                leek_bench::FastOutcome::Disagree { got, expected } => {
                    println!("  ✗ {id}\n      got      {got}\n      expected {expected}");
                }
                leek_bench::FastOutcome::CompileError => println!("  ⊗ {id}  (compile error)"),
                leek_bench::FastOutcome::EmitError(e) => println!("  ⊘ {id}  (emit: {e})"),
                leek_bench::FastOutcome::RuntimeError(e) => println!("  !  {id}  (runtime: {e})"),
                leek_bench::FastOutcome::Timeout => println!("  ⏱ {id}  (timeout)"),
                leek_bench::FastOutcome::NoResult => println!("  ?  {id}  (no result)"),
            }
        }
    }
    Ok(())
}

fn run_corpus(cli: &Cli) -> Result<()> {
    let manifest_owned;
    let manifest = if let Some(path) = &cli.manifest {
        manifest_owned =
            Manifest::load(path).with_context(|| format!("load manifest {}", path.display()))?;
        &manifest_owned
    } else {
        embedded_manifest()
    };

    let cases: Vec<&TestCase> = manifest
        .cases
        .iter()
        .filter(|c| cli.include_disabled || c.enabled)
        .filter(|c| expectation_matches(cli.corpus_expectation, c))
        .filter(|c| case_matches_filter(c, cli.case_filter.as_deref()))
        .take(cli.limit.max(1))
        .collect();

    eprintln!(
        "running {} cases (runs/case={}); upstream={}, rust-java={}",
        cases.len(),
        cli.runs,
        if cli.no_upstream { "off" } else { "on" },
        if cli.no_rust_java { "off" } else { "on" },
    );

    let work_root = cli.work_root.clone().unwrap_or_else(|| {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("leekbench-corpus-{}-{}", std::process::id(), ts))
    });
    std::fs::create_dir_all(&work_root)?;

    let mut nat = Aggregate::new("rust-native");
    let mut rj = Aggregate::new("rust-java");
    let mut up = Aggregate::new("upstream-java");

    let mut per_case_table = if cli.verbose {
        let mut t = make_table(&["case", "native", "rust-java", "upstream", "agree"]);
        t.set_content_arrangement(ContentArrangement::Disabled);
        Some(t)
    } else {
        None
    };

    let bench_start = Instant::now();
    for (i, case) in cases.iter().enumerate() {
        let path = work_root.join(format!("case_{i:05}.leek"));
        std::fs::write(&path, &case.code).with_context(|| format!("write {}", path.display()))?;
        let expected = match &case.expected {
            Expectation::Equals { value } => Some(value.as_str()),
            _ => None,
        };
        let opts = BenchOptions {
            runs: cli.runs.max(1),
            version: cli.corpus_lang_version.unwrap_or(case.version),
            strict: case.strict,
        };

        // Native is the in-process reference backend (the interpreter was removed).
        let r1 = bench(&mut RustNative::new(), &path, &opts);
        let r2 = if cli.no_rust_java {
            None
        } else {
            Some(bench(&mut RustJavaEmit::auto(), &path, &opts))
        };
        let r3 = if cli.no_upstream {
            None
        } else {
            Some(bench(&mut UpstreamJava::auto(), &path, &opts))
        };

        nat.record(&r1, expected);
        if let Some(r) = &r2 {
            rj.record(r, expected);
        }
        if let Some(r) = &r3 {
            up.record(r, expected);
        }

        if let Some(t) = per_case_table.as_mut() {
            let agree = agreement_marker(&r1, r2.as_ref(), r3.as_ref());
            t.add_row(Row::from(vec![
                Cell::new(truncate(&case.id, 60)),
                right(or_dash(&r1)),
                right(or_dash_opt(&r2)),
                right(or_dash_opt(&r3)),
                Cell::new(agree),
            ]));
        }
    }
    let bench_total = bench_start.elapsed();
    if let Some(t) = per_case_table {
        println!("{t}");
    }

    let baseline = [&nat, &rj, &up]
        .iter()
        .filter_map(|a| median(&a.warm))
        .min()
        .unwrap_or(Duration::from_nanos(1));

    let mut table = make_table(&[
        "backend",
        "ok",
        "err",
        "agree",
        "cold_med",
        "warm_min",
        "warm_med",
        "warm_p95",
        "vs_best/share",
        "total",
    ]);
    for a in [&nat, &rj, &up] {
        if a.attempts == 0 {
            continue;
        }
        let cm = median(&a.cold).unwrap_or(Duration::ZERO);
        let wm = median(&a.warm).unwrap_or(Duration::ZERO);
        let wmin = a.warm.iter().copied().min().unwrap_or(Duration::ZERO);
        let wp95 = pctile_unsorted(&a.warm, 95).unwrap_or(Duration::ZERO);
        let ratio = if baseline > Duration::ZERO {
            wm.as_secs_f64() / baseline.as_secs_f64()
        } else {
            1.0
        };
        let agree = if a.expected_checks > 0 {
            format!("{}/{}", a.expected_agreed, a.expected_checks)
        } else {
            "-".into()
        };
        table.add_row(Row::from(vec![
            Cell::new(a.name),
            right(a.ok.to_string()),
            right(a.err.to_string()),
            right(agree),
            right(fmt(cm)),
            right(fmt(wmin)),
            right(fmt(wm)),
            right(fmt(wp95)),
            right(format!("{ratio:.2}×")),
            right(fmt(a.total_wall)),
        ]));
        if cli.detail && !a.step_samples.is_empty() {
            let medians: Vec<(String, Duration)> = a
                .step_order
                .iter()
                .filter_map(|name| {
                    let samples = a.step_samples.get(name)?;
                    median(samples).map(|m| (name.clone(), m))
                })
                .collect();
            push_step_rows_corpus(&mut table, &medians);
        }
    }
    println!("{table}");
    println!("bench wall-clock: {}", fmt(bench_total));
    if !cli.detail
        && [&nat, &rj, &up]
            .iter()
            .any(|a| !a.step_samples.is_empty())
    {
        println!("(pass --detail to see per-step prepare timings)");
    }
    Ok(())
}

fn expectation_matches(filter: CorpusExpectation, case: &TestCase) -> bool {
    match filter {
        CorpusExpectation::Equals => matches!(case.expected, Expectation::Equals { .. }),
        CorpusExpectation::Clean => case.expected.implies_clean_parse(),
        CorpusExpectation::All => true,
    }
}

fn case_matches_filter(case: &TestCase, filter: Option<&str>) -> bool {
    let Some(raw) = filter else {
        return true;
    };
    let needle = raw.trim();
    if needle.is_empty() {
        return true;
    }

    case.id.contains(needle)
        || case.source_file.contains(needle)
        || case.method_name.contains(needle)
}

// ---------------------------------------------------------------------
// Table helpers
// ---------------------------------------------------------------------

fn make_table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_preset(presets::UTF8_BORDERS_ONLY);
    t.set_content_arrangement(ContentArrangement::Disabled);
    t.set_header(headers.iter().enumerate().map(|(i, h)| {
        let c = Cell::new(*h);
        if i == 0 {
            c.set_alignment(CellAlignment::Left)
        } else {
            c.set_alignment(CellAlignment::Right)
        }
    }));
    t
}

fn right(s: impl Into<String>) -> Cell {
    Cell::new(s.into()).set_alignment(CellAlignment::Right)
}

fn push_step_rows(table: &mut Table, steps: &[(String, Duration)]) {
    let total: Duration = steps.iter().map(|(_, d)| *d).sum();
    for (name, d) in steps {
        let pct = if total > Duration::ZERO {
            d.as_secs_f64() / total.as_secs_f64() * 100.0
        } else {
            0.0
        };
        table.add_row(Row::from(vec![
            Cell::new(format!("  ↳ {name}")),
            Cell::new(""),
            right(fmt(*d)),
            Cell::new(""),
            Cell::new(""),
            right(fmt_pct(pct)),
            Cell::new(""),
        ]));
    }
}

fn push_step_rows_corpus(table: &mut Table, steps: &[(String, Duration)]) {
    let total: Duration = steps.iter().map(|(_, d)| *d).sum();
    for (name, d) in steps {
        let pct = if total > Duration::ZERO {
            d.as_secs_f64() / total.as_secs_f64() * 100.0
        } else {
            0.0
        };
        // Match the 10-column corpus table.
        table.add_row(Row::from(vec![
            Cell::new(format!("  ↳ {name}")),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
            right(fmt(*d)),
            Cell::new(""),
            right(fmt_pct(pct)),
            Cell::new(""),
        ]));
    }
}

fn or_dash(r: &Result<BenchSummary>) -> String {
    r.as_ref()
        .ok().map_or_else(|| "-".into(), |s| fmt(s.warm_median))
}

fn or_dash_opt(r: &Option<Result<BenchSummary>>) -> String {
    r.as_ref()
        .and_then(|x| x.as_ref().ok()).map_or_else(|| "-".into(), |s| fmt(s.warm_median))
}

// ---------------------------------------------------------------------
// Stats / Aggregate helpers
// ---------------------------------------------------------------------

struct Aggregate {
    name: &'static str,
    attempts: usize,
    ok: usize,
    err: usize,
    cold: Vec<Duration>,
    warm: Vec<Duration>,
    total_wall: Duration,
    expected_checks: usize,
    expected_agreed: usize,
    step_order: Vec<String>,
    step_samples: std::collections::HashMap<String, Vec<Duration>>,
}

impl Aggregate {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            attempts: 0,
            ok: 0,
            err: 0,
            cold: Vec::new(),
            warm: Vec::new(),
            total_wall: Duration::ZERO,
            expected_checks: 0,
            expected_agreed: 0,
            step_order: Vec::new(),
            step_samples: std::collections::HashMap::new(),
        }
    }
    fn record(&mut self, r: &Result<BenchSummary>, expected: Option<&str>) {
        self.attempts += 1;
        match r {
            Ok(s) => {
                self.ok += 1;
                self.cold.push(s.cold);
                self.warm.push(s.warm_median);
                self.total_wall += s.cold + s.warm_runs.iter().copied().sum::<Duration>();
                if let Some(v) = expected {
                    self.expected_checks += 1;
                    if s.stdout_sample.trim() == v.trim() {
                        self.expected_agreed += 1;
                    }
                }
                for (name, dur) in &s.prepare_steps {
                    if !self.step_samples.contains_key(name) {
                        self.step_order.push(name.clone());
                    }
                    self.step_samples
                        .entry(name.clone())
                        .or_default()
                        .push(*dur);
                }
            }
            Err(_) => self.err += 1,
        }
    }
}

fn agreement_marker(
    r1: &Result<BenchSummary>,
    r2: Option<&Result<BenchSummary>>,
    r3: Option<&Result<BenchSummary>>,
) -> &'static str {
    let vals: Vec<&str> = [
        r1.as_ref().ok().map(|s| s.stdout_sample.as_str()),
        r2.and_then(|r| r.as_ref().ok())
            .map(|s| s.stdout_sample.as_str()),
        r3.and_then(|r| r.as_ref().ok())
            .map(|s| s.stdout_sample.as_str()),
    ]
    .into_iter()
    .flatten()
    .collect();
    if vals.len() < 2 {
        return " ";
    }
    if vals.iter().all(|v| *v == vals[0]) {
        "✓"
    } else {
        "✗"
    }
}

fn pctile(sorted: &[Duration], p: usize) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let idx = (sorted.len() * p / 100).min(sorted.len() - 1);
    Some(sorted[idx])
}

fn pctile_unsorted(samples: &[Duration], p: usize) -> Option<Duration> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort();
    pctile(&s, p)
}

fn median(samples: &[Duration]) -> Option<Duration> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort();
    Some(s[s.len() / 2])
}

fn fmt(d: Duration) -> String {
    if d.as_millis() >= 1 {
        format!("{:.2}ms", d.as_secs_f64() * 1000.0)
    } else {
        format!("{:.2}µs", d.as_secs_f64() * 1_000_000.0)
    }
}

fn fmt_pct(pct: f64) -> String {
    if pct >= 10.0 {
        format!("{pct:.1}%")
    } else if pct >= 0.1 {
        format!("{pct:.2}%")
    } else if pct > 0.0 {
        format!("{pct:.4}%")
    } else {
        "0%".into()
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n - 1).collect();
        out.push('…');
        out
    }
}

trait NameStr {
    fn name_str(&self) -> String;
}
impl NameStr for RustNative {
    fn name_str(&self) -> String {
        leek_bench::Backend::name(self).to_string()
    }
}
impl NameStr for RustJavaEmit {
    fn name_str(&self) -> String {
        leek_bench::Backend::name(self).to_string()
    }
}
impl NameStr for UpstreamJava {
    fn name_str(&self) -> String {
        leek_bench::Backend::name(self).to_string()
    }
}
