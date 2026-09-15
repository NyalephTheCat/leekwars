//! `miku run` — build and execute via the native JIT.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_backends::resolve_run_backend;
use leek_project::Project;
use leek_session::{Compilation, DriverConfig, OptLevel, RecipeParams, Session, Target};

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
        // `scope` and `timing` stay at their defaults: every `miku`
        // subcommand compiles the whole program, and only `build --verbose`
        // wants timings.
        ..DriverConfig::default()
    };
    let session = Session::new(&project, config)?;
    let compiled = session.compile_entry()?;
    if compiled.report() {
        return Ok(ExitCode::from(1));
    }

    let Some(hir) = compiled.hir() else {
        eprintln!("miku: lowering produced no HIR");
        return Ok(ExitCode::from(1));
    };

    // Execute via the native JIT (the interpreter backend was removed), at the
    // input's settled version *and* strict mode.
    use leek_backend_native::{DEFAULT_OP_BUDGET, NativeArtifact, NativeOptions};
    let mut opts = NativeOptions::jit_for_input(compiled.input(), DEFAULT_OP_BUDGET);
    crate::util::apply_native_settings(&mut opts, &project.manifest);
    match leek_backend_native::compile(hir, &opts) {
        Ok(NativeArtifact::Value(v)) => {
            println!("{v}");
            Ok(ExitCode::SUCCESS)
        }
        Ok(_) => unreachable!("Jit emit yields a Value"),
        Err(e) => {
            // `error: unsupported: switch on real` told the user nothing about
            // *where*. Render it as a diagnostic instead: same codes, same
            // caret, same `-->` header a frontend error gets.
            report_native_error(&compiled, &e);
            Ok(ExitCode::from(1))
        }
    }
}

/// Render a backend failure through the compilation's own reporter and
/// source map — the same `-->` header, source line and caret a frontend
/// diagnostic gets, against the file the failure was actually raised in.
fn report_native_error(compiled: &Compilation<'_>, err: &leek_backend_native::NativeError) {
    let _ = compiled.report_backend(&err.diagnostics());
}
