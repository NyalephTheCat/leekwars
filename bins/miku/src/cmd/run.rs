//! `miku run` — build via the interpreter and execute.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_backends::resolve_run_backend;
use leek_driver::{DriverConfig, run_entry};
use leek_hir::pipeline::HirArtifact;
use leek_project::Project;
use leek_recipes::{RecipeParams, Target};

use crate::cli::{ColorWhen, MessageFormat, Run};

const OP_BUDGET: u64 = 20_000_000;

pub fn run(
    args: Run,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    _quiet: bool,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    resolve_run_backend(args.backend.as_deref())?;

    let config = DriverConfig {
        target: Target::Linted,
        params: RecipeParams::default(),
        color: color.into(),
        format: format.into(),
    };
    let driver_run = run_entry(&project, &config)?;
    if driver_run.had_error {
        return Ok(ExitCode::from(1));
    }

    let hir = if let Some(h) = driver_run.run.get::<HirArtifact>() { h } else {
        eprintln!("miku: lowering produced no HIR");
        return Ok(ExitCode::from(1));
    };

    let version_byte = driver_run.run.input().version_byte;
    let r = leek_backend_interp::run_with_limit_version(hir.0.as_ref(), OP_BUDGET, version_byte);
    if let Some(err) = r.error {
        eprintln!("error: {err}");
        return Ok(ExitCode::from(1));
    }
    println!("{}", r.value);
    Ok(ExitCode::SUCCESS)
}
