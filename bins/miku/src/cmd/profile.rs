//! `miku profile` — run the program under the interpreter with
//! per-call-stack ops profiling enabled, then print a folded-
//! stack file (`flamegraph.pl`-ready) or a human-readable table.
//!
//! The Leekscript interpreter is fully deterministic and op-counted,
//! so a single run gives a perfectly reproducible profile —
//! re-running yields identical numbers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use leek_backend_interp::{Interpreter, Profiler};
use leek_hir::pipeline::HirArtifact;
use leek_mir::lower_file as lower_mir;
use leek_span::SourceId;

use crate::cli::{Profile, ProfileFormat};
use leek_pipeline::Input;
use leek_project::Project;

/// Generous budget — profile runs shouldn't trip the limit just
/// because they instrumented a few extra frames.
const OP_BUDGET: u64 = 200_000_000;

pub fn run(args: Profile, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    let entry = match &args.path {
        Some(p) if p.is_absolute() => p.clone(),
        Some(p) => std::env::current_dir()
            .context("determining current directory")?
            .join(p),
        None => project.entry_path(),
    };

    let source = SourceId::new(1).unwrap();
    let (src, _text) = project.pipeline_input(source, &entry)?;
    let input = Input::from(src);
    let version_byte = input.version_byte;
    let pipeline =
        leek_recipes::pipeline(leek_recipes::Target::Hir, &leek_recipes::driver_params())
            .expect("recipe");
    let result = pipeline.run(input);

    let hir = result.get::<HirArtifact>().ok_or_else(|| {
        anyhow::anyhow!(
            "miku profile: lowering produced no HIR for {}",
            entry.display()
        )
    })?;
    let (program, mir_errs) = lower_mir(hir.0.as_ref());
    if let Some(first) = mir_errs.first() {
        anyhow::bail!("MIR lowering failed: {}", first.message);
    }

    // Run the program with profiling enabled.
    let mut interp = Interpreter::with_op_limit(&program, OP_BUDGET);
    interp.set_version(version_byte);
    interp.set_profiler(Profiler::new());
    let outcome = interp.run();
    if let Some(err) = outcome.error {
        eprintln!("warning: program halted with error: {err}");
    }
    let total_ops = interp.ops_used();
    let profiler = interp.take_profiler().expect("profiler was just set");

    match args.format {
        ProfileFormat::Folded => {
            for line in profiler.folded_lines() {
                println!("{line}");
            }
        }
        ProfileFormat::Table => {
            print_table(&profiler, total_ops, args.min_ops, quiet);
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Print the human-readable table: rolled-up per-leaf-function
/// self-ops, with their share of the total.
fn print_table(p: &Profiler, total_ops: u64, min_ops: u64, quiet: bool) {
    // Sum self-ops per leaf frame name (last element of each stack).
    let mut by_leaf: HashMap<String, (u64, u64)> = HashMap::new();
    for (stack, &ops) in p.samples() {
        let leaf = stack.last().cloned().unwrap_or_else(|| "<unknown>".into());
        let entry = by_leaf.entry(leaf).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(ops);
        entry.1 = entry.1.saturating_add(1); // call-site count
    }
    let mut rows: Vec<(String, u64, u64)> = by_leaf
        .into_iter()
        .map(|(n, (ops, sites))| (n, ops, sites))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));

    let mut shown_other: u64 = 0;
    let mut rows_to_print: Vec<&(String, u64, u64)> = Vec::new();
    for r in &rows {
        if r.1 < min_ops {
            shown_other = shown_other.saturating_add(r.1);
            continue;
        }
        rows_to_print.push(r);
    }

    if !quiet {
        println!(
            "total ops: {total_ops}   ({} frames, {} call sites)",
            rows.len(),
            rows.iter().map(|r| r.2).sum::<u64>(),
        );
    }

    let name_w = rows_to_print
        .iter()
        .map(|r| r.0.len())
        .max()
        .unwrap_or(0)
        .max(8);
    println!(
        "  {:<name_w$}  {:>10}  {:>7}  call sites",
        "function",
        "self ops",
        "% total",
        name_w = name_w,
    );
    for (name, ops, sites) in &rows_to_print {
        let pct = if total_ops == 0 {
            0.0
        } else {
            100.0 * *ops as f64 / total_ops as f64
        };
        println!(
            "  {name:<name_w$}  {ops:>10}  {pct:>6.2}%  {sites}",
        );
    }
    if shown_other > 0 {
        println!(
            "  {:<name_w$}  {:>10}  {:>6}   --",
            "(other)",
            shown_other,
            "",
            name_w = name_w,
        );
    }
    let _ = PathBuf::new(); // silence unused-import if it appears
}
