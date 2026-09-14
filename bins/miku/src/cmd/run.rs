//! `miku run` — build and execute via the native JIT.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_backends::resolve_run_backend;
use leek_hir::pipeline::HirArtifact;
use leek_project::Project;
use leek_session::{DriverConfig, OptLevel, RecipeParams, Target, run_entry};

use crate::cli::{ColorWhen, MessageFormat, Run};

pub fn run(
    args: &Run,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    _quiet: bool,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    if leek_session::report_manifest(&project, color.into(), format.into()) {
        return Ok(ExitCode::from(1));
    }

    resolve_run_backend(args.backend.as_deref())?;

    let config = DriverConfig {
        target: Target::Linted,
        // The interpreter enforces an op budget, so fold constants to shrink it.
        params: RecipeParams::default().with_opt(OptLevel::O1),
        color: color.into(),
        format: format.into(),
        timing: None,
    };
    let driver_run = run_entry(&project, &config)?;
    if driver_run.had_error {
        return Ok(ExitCode::from(1));
    }
    // The same source map the driver rendered the frontend diagnostics
    // against, reused so a backend failure points at the same files.
    let entry_label = project.entry_path().display().to_string();
    let entry_text = std::fs::read_to_string(project.entry_path()).unwrap_or_default();
    let sources = leek_session::run_sources(&driver_run.run, &entry_text, &entry_label);

    let Some(hir) = driver_run.run.get::<HirArtifact>() else {
        eprintln!("miku: lowering produced no HIR");
        return Ok(ExitCode::from(1));
    };

    // Execute via the native JIT (the interpreter backend was removed), at the
    // input's settled version *and* strict mode.
    use leek_backend_native::{DEFAULT_OP_BUDGET, NativeArtifact, NativeOptions};
    let mut opts = NativeOptions::jit_for_input(driver_run.run.input(), DEFAULT_OP_BUDGET);
    crate::util::apply_native_settings(&mut opts, &project.manifest);
    match leek_backend_native::compile(hir.0.as_ref(), &opts) {
        Ok(NativeArtifact::Value(v)) => {
            println!("{v}");
            Ok(ExitCode::SUCCESS)
        }
        Ok(_) => unreachable!("Jit emit yields a Value"),
        Err(e) => {
            // `error: unsupported: switch on real` told the user nothing about
            // *where*. Render it as a diagnostic instead: same codes, same
            // caret, same `-->` header a frontend error gets.
            report_native_error(&project, &e, &sources, color, format);
            Ok(ExitCode::from(1))
        }
    }
}

/// Render a backend failure through the project's reporter, falling back to
/// the plain one-line form if the reporter can't be built (a broken `[lint]`
/// table, which `report_manifest` above has already complained about).
fn report_native_error(
    project: &Project,
    err: &leek_backend_native::NativeError,
    sources: &leek_diagnostics::Sources,
    color: ColorWhen,
    format: MessageFormat,
) {
    if !crate::util::report_diagnostics(project, &err.diagnostics(), sources, color, format) {
        eprintln!("error: {err}");
    }
}
