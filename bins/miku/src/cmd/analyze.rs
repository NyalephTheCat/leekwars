//! `miku analyze` — per-function complexity table.
//!
//! Lowers each source file to HIR, runs [`leek_complexity::analyze_file`],
//! and prints a per-function summary:
//!
//! ```text
//! src/main.leek
//!   sum(arr)            O(arr)            5·arr + 9
//!   total(arr, m)       O(arr · m)        arr·(m + 4) + 13
//!   helper()            O(1)              7
//! ```
//!
//! `--formula` widens the formula column to show the full
//! expression. Otherwise the formula is rendered compactly.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use leek_complexity::{Complexity, analyze_file};
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::Input;
use leek_span::SourceId;

use crate::cli::Analyze;
use leek_project::Project;

pub fn run(args: Analyze, manifest_path: Option<&Path>, quiet: bool) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    let files = if let Some(p) = args.path { vec![resolve_path(&p)?] } else {
        let mut all = project.walk_sources();
        if all.is_empty() {
            all.push(project.entry_path());
        }
        all
    };

    for (i, path) in files.iter().enumerate() {
        let source = SourceId::new((i + 1).try_into().unwrap()).unwrap();
        let (src, _text) = project.pipeline_input(source, path)?;
        let input = Input::from(src);
        let pipeline =
            leek_recipes::pipeline(leek_recipes::Target::Hir, &leek_recipes::driver_params())
                .expect("recipe");
        let result = pipeline.run(input);
        let Some(hir_artifact) = result.get::<HirArtifact>() else {
            eprintln!(
                "miku analyze: failed to lower {} to HIR",
                display_relative(&project.root, path).display(),
            );
            continue;
        };
        let report = analyze_file(&hir_artifact.0);
        print_report(&project.root, path, &report, args.formula, quiet);
    }

    Ok(ExitCode::SUCCESS)
}

fn resolve_path(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("determining current directory")?
            .join(p))
    }
}

fn display_relative(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root).map_or_else(|_| p.to_path_buf(), std::path::Path::to_path_buf)
}

fn print_report(root: &Path, path: &Path, report: &[Complexity], show_formula: bool, quiet: bool) {
    if !quiet {
        println!("{}", display_relative(root, path).display());
    }
    // Determine column widths.
    let name_w = report
        .iter()
        .map(|c| display_name(c).len())
        .max()
        .unwrap_or(0)
        .max(8);
    let bigo_w = report
        .iter()
        .map(|c| c.big_o.render().len())
        .max()
        .unwrap_or(0)
        .max(4);
    for c in report {
        let name = display_name(c);
        let bigo = c.big_o.render();
        if show_formula {
            println!(
                "  {:<name_w$}  {:<bigo_w$}  {}",
                name,
                bigo,
                c.formula,
                name_w = name_w,
                bigo_w = bigo_w,
            );
        } else {
            println!("  {name:<name_w$}  {bigo}",);
        }
    }
}

fn display_name(c: &Complexity) -> String {
    if c.params.is_empty() {
        format!("{}()", c.name)
    } else {
        let params: Vec<String> = c.params.iter().map(|p| p.name.clone()).collect();
        format!("{}({})", c.name, params.join(", "))
    }
}
