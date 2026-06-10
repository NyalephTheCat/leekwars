//! `miku run` — build and execute via the native JIT.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_backends::resolve_run_backend;
use leek_driver::{DriverConfig, run_entry};
use leek_hir::pipeline::HirArtifact;
use leek_project::Project;
use leek_recipes::{OptLevel, RecipeParams, Target};

use crate::cli::{ColorWhen, MessageFormat, Run};

const OP_BUDGET: u64 = 20_000_000;

pub fn run(
    args: &Run,
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
        // The interpreter enforces an op budget, so fold constants to shrink it.
        params: RecipeParams::default().with_opt(OptLevel::O1),
        color: color.into(),
        format: format.into(),
    };
    let driver_run = run_entry(&project, &config)?;
    if driver_run.had_error {
        return Ok(ExitCode::from(1));
    }

    let Some(hir) = driver_run.run.get::<HirArtifact>() else {
        eprintln!("miku: lowering produced no HIR");
        return Ok(ExitCode::from(1));
    };

    let version_byte = driver_run.run.input().version_byte;
    // Execute via the native JIT (the interpreter backend was removed). The 20M
    // op budget matches the prior interpreter run.
    use leek_backend_native::{NativeArtifact, NativeEmit, NativeOptions};
    let mut opts = NativeOptions::debug();
    opts.version = version_byte;
    opts.op_limit = OP_BUDGET;
    opts.emit = NativeEmit::Jit;
    match leek_backend_native::compile(hir.0.as_ref(), &opts) {
        Ok(NativeArtifact::Value(v)) => {
            println!("{v}");
            Ok(ExitCode::SUCCESS)
        }
        Ok(_) => unreachable!("Jit emit yields a Value"),
        Err(e) => {
            eprintln!("error: {e}");
            Ok(ExitCode::from(1))
        }
    }
}
