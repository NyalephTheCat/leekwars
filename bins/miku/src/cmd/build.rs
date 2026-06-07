//! `miku build` — compile via the manifest's selected backend.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use leek_backends::{java_clean_mode, pick_java_out_dir, resolve_backend, version_from_byte};
use leek_driver::{DriverConfig, run_entry};
use leek_hir::pipeline::HirArtifact;
use leek_manifest::BackendKind;
use leek_project::Project;
use leek_recipes::{RecipeParams, Target};

use crate::cli::{Build, ColorWhen, MessageFormat};

pub fn run(
    args: Build,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    quiet: bool,
    environment: Option<std::sync::Arc<dyn leek_environment::EnvironmentCatalog>>,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    let backend = resolve_backend(&project.manifest, args.backend.as_deref())?;

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

    let version = version_from_byte(driver_run.run.input().version_byte);

    match backend {
        BackendKind::Java => {
            emit_java(&project, &driver_run.run, version, &args, quiet, environment)
        }
        BackendKind::Interp => {
            if !quiet {
                eprintln!(
                    "miku: interpreter backend has no build output; use `miku run` to execute"
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        BackendKind::Native => {
            bail!("native backend not yet supported in this toolchain");
        }
        BackendKind::Jar => {
            bail!("jar backend not yet supported in this toolchain");
        }
        BackendKind::Wasm => {
            bail!("wasm backend not yet supported in this toolchain");
        }
    }
}

fn emit_java(
    project: &Project,
    result: &leek_pipeline::Run<'_>,
    version: leek_syntax::Version,
    args: &Build,
    quiet: bool,
    environment: Option<std::sync::Arc<dyn leek_environment::EnvironmentCatalog>>,
) -> Result<ExitCode> {
    let hir = result
        .get::<HirArtifact>()
        .ok_or_else(|| anyhow::anyhow!("lowering produced no HIR"))?;

    let settings = project
        .manifest
        .backend
        .java
        .clone()
        .unwrap_or_default();

    let clean = java_clean_mode(args.clean, &settings);
    let mut opts = if clean {
        leek_backend_java::Options::clean(version, 0)
    } else {
        leek_backend_java::Options::exact(version, 0)
    }
    .with_source_path(project.entry_path().display().to_string());
    if let Some(env) = &environment {
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
