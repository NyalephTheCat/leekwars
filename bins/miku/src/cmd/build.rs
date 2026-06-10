//! `miku build` — compile via the manifest's selected backend.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use leek_backends::{java_clean_mode, pick_java_out_dir, resolve_backend, version_from_byte};
use leek_driver::{DriverConfig, run_entry, run_entry_timed};
use leek_hir::pipeline::HirArtifact;
use leek_manifest::BackendKind;
use leek_project::Project;
use leek_recipes::{RecipeParams, Target};

use crate::cli::{Build, ColorWhen, MessageFormat};

pub fn run(
    args: &Build,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    quiet: bool,
    verbose: bool,
    environment: Option<&std::sync::Arc<dyn leek_environment::EnvironmentCatalog>>,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    let backend = resolve_backend(&project.manifest, args.backend.as_deref())?;

    // Java *exact* mode must mirror the upstream reference compiler's emission
    // shape, so it keeps the IR source-faithful (O0). Every other build path —
    // Java clean, native — folds constants to shrink the program's op budget.
    let clean_java = matches!(backend, BackendKind::Java)
        && java_clean_mode(
            args.clean,
            &project.manifest.backend.java.clone().unwrap_or_default(),
        );
    let opt = if matches!(backend, BackendKind::Java) && !clean_java {
        leek_recipes::OptLevel::O0
    } else {
        leek_recipes::OptLevel::O1
    };

    let config = DriverConfig {
        target: Target::Linted,
        params: RecipeParams::default().with_opt(opt),
        color: color.into(),
        format: format.into(),
    };
    let driver_run = if verbose {
        let sink = leek_pipeline::TimingSink::new();
        let run = run_entry_timed(&project, &config, &sink)?;
        eprintln!(
            "miku build: pipeline timings for {}:",
            project.entry_path().display()
        );
        let mut total = std::time::Duration::ZERO;
        for entry in sink.entries() {
            total += entry.duration;
            eprintln!("  {:>14}: {:?}", entry.step, entry.duration);
        }
        eprintln!("  {:>14}: {:?}", "total", total);
        run
    } else {
        run_entry(&project, &config)?
    };
    if driver_run.had_error {
        return Ok(ExitCode::from(1));
    }

    let version = version_from_byte(driver_run.run.input().version_byte);

    match backend {
        BackendKind::Java => {
            emit_java(&project, &driver_run.run, version, args, quiet, environment)
        }
        BackendKind::Native => emit_native(&project, &driver_run.run, args, quiet),
        BackendKind::Jar => {
            bail!("jar backend not yet supported in this toolchain");
        }
        BackendKind::Wasm => {
            bail!("wasm backend not yet supported in this toolchain");
        }
    }
}

/// AOT-compile the project to a standalone native executable. The output path
/// is `--out-dir` if given, else `<project root>/<project name>`.
fn emit_native(
    project: &Project,
    result: &leek_pipeline::Run<'_>,
    args: &Build,
    quiet: bool,
) -> Result<ExitCode> {
    let hir = result
        .get::<HirArtifact>()
        .ok_or_else(|| anyhow::anyhow!("lowering produced no HIR"))?;
    let input = result.input();
    let out = args
        .out_dir
        .clone()
        .unwrap_or_else(|| project.root.join(&project.manifest.project.name));

    let opts = leek_backend_native::NativeOptions::release()
        .with_lang(input.version_byte, input.strict)
        // A standalone binary runs unbounded — no per-turn op budget.
        .with_op_limit(u64::MAX);
    leek_backend_native::aot::compile_to_executable(hir.0.as_ref(), &opts, &out, quiet)
        .with_context(|| format!("compiling native executable to {}", out.display()))?;
    Ok(ExitCode::SUCCESS)
}

fn emit_java(
    project: &Project,
    result: &leek_pipeline::Run<'_>,
    version: leek_syntax::Version,
    args: &Build,
    quiet: bool,
    environment: Option<&std::sync::Arc<dyn leek_environment::EnvironmentCatalog>>,
) -> Result<ExitCode> {
    let hir = result
        .get::<HirArtifact>()
        .ok_or_else(|| anyhow::anyhow!("lowering produced no HIR"))?;

    let settings = project.manifest.backend.java.clone().unwrap_or_default();

    let clean = java_clean_mode(args.clean, &settings);
    let mut opts = if clean {
        leek_backend_java::Options::clean(version, 0)
    } else {
        leek_backend_java::Options::exact(version, 0)
    }
    .with_source_path(project.entry_path().display().to_string());
    if let Some(env) = environment {
        opts = opts.with_environment(env.clone());
    }

    let out = leek_backend_java::emit(hir.0.as_ref(), &opts);

    let out_dir = pick_java_out_dir(project, args.out_dir.as_deref(), &settings);
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    let java_path = out_dir.join(format!("{}.java", out.class_name));
    std::fs::write(&java_path, &out.java)
        .with_context(|| format!("writing {}", java_path.display()))?;
    if settings.emit_lines {
        let lines_path = out_dir.join(format!("{}.lines", out.class_name));
        std::fs::write(&lines_path, &out.lines)
            .with_context(|| format!("writing {}", lines_path.display()))?;
        if !quiet {
            eprintln!("wrote {} and {}", java_path.display(), lines_path.display());
        }
    } else if !quiet {
        eprintln!("wrote {}", java_path.display());
    }
    Ok(ExitCode::SUCCESS)
}
