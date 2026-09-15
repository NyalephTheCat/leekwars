//! `miku check` — diagnostics only.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_backends::resolve_backend;
use leek_manifest::BackendKind;
use leek_project::Project;
use leek_session::{Compilation, DriverConfig, RecipeParams, Session, Target};

use crate::cli::{Check, ColorWhen, MessageFormat};

pub fn run(
    args: &Check,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    _quiet: bool,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    if leek_session::report_manifest(&project, color.into(), format.into()) {
        return Ok(ExitCode::from(1));
    }

    let config = DriverConfig {
        target: Target::Linted,
        params: RecipeParams::default(),
        color: color.into(),
        format: format.into(),
        // `scope` and `timing` stay at their defaults: every `miku`
        // subcommand compiles the whole program, and only `build --verbose`
        // wants timings.
        ..DriverConfig::default()
    };
    let session = Session::new(&project, config)?;
    let compiled = session.compile_entry()?;
    let had_error = compiled.report();
    if !had_error && native_compat_wanted(args, &project) {
        report_native_compat(&project, &compiled);
    }
    Ok(if had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Whether to add the native-compat pass.
///
/// Default-on only for a project whose default backend *is* native: a project
/// that targets Java has no reason to hear which constructs Cranelift can't
/// lower, and the pass costs a full translation of every reachable function.
/// A manifest with no resolvable backend gets nothing rather than an error —
/// `miku check` is the command that is supposed to work on a half-finished
/// project.
fn native_compat_wanted(args: &Check, project: &Project) -> bool {
    if args.no_native_compat {
        return false;
    }
    args.native_compat
        || resolve_backend(&project.manifest, None).is_ok_and(|kind| kind == BackendKind::Native)
}

/// Emit the native backend's compat diagnostics as **warnings**.
///
/// Warnings, not errors, and deliberately: `miku check` is a gate in CI, and
/// a project that builds for Java or LeekScript must not start failing it
/// because the native JIT can't lower one of its constructs. What the author
/// gets is the location — the point of the pass — without a changed exit code.
fn report_native_compat(project: &Project, compiled: &Compilation<'_>) {
    let Some(hir) = compiled.hir() else {
        return;
    };
    let mut opts = leek_backend_native::NativeOptions::jit_for_input(
        compiled.input(),
        leek_backend_native::DEFAULT_OP_BUDGET,
    );
    crate::util::apply_native_settings(&mut opts, &project.manifest);
    let diagnostics: Vec<leek_diagnostics::Diagnostic> =
        leek_backend_native::check_native_compat(hir, &opts)
            .into_iter()
            .map(|mut d| {
                d.severity = leek_diagnostics::Severity::Warning;
                d
            })
            .collect();
    // Through the compilation's own reporter and source map, so a warning
    // raised inside an included file points at that file.
    let _ = compiled.report_backend(&diagnostics);
}
