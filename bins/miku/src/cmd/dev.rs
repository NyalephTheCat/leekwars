//! Developer hygiene commands — layer checks, builtin drift, etc.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result};

use crate::cli::{Dev, DevCommand};

pub fn run(args: Dev, quiet: bool) -> Result<ExitCode> {
    match args.command {
        DevCommand::Layers => run_tool("check-layers.sh", &[], quiet),
        DevCommand::Builtins => run_tool("builtin-extract.sh", &["--check"], quiet),
        DevCommand::BuiltinSuite => run_cargo_package("leek-builtin-suite", quiet),
        DevCommand::Pipeline(cmd) => pipeline(cmd, quiet),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn run_cargo_package(package: &str, quiet: bool) -> Result<ExitCode> {
    let root = workspace_root();
    if !quiet {
        eprintln!("miku dev: cargo run -p {package}");
    }
    let status = Command::new("cargo")
        .args(["run", "-p", package, "--quiet"])
        .current_dir(&root)
        .status()
        .with_context(|| format!("cargo run -p {package}"))?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn run_tool(script: &str, extra_args: &[&str], quiet: bool) -> Result<ExitCode> {
    let root = workspace_root();
    let path = root.join("tools").join(script);
    if !quiet {
        eprintln!("miku dev: {} {}", path.display(), extra_args.join(" "));
    }
    let status = Command::new("bash")
        .arg(&path)
        .args(extra_args)
        .current_dir(&root)
        .status()
        .with_context(|| format!("running {}", path.display()))?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn pipeline(cmd: crate::cli::DevPipeline, quiet: bool) -> Result<ExitCode> {
    use leek_pipeline::{Input, TimingSink};
    use leek_recipes::{RecipeParams, Target};
    use leek_span::SourceId;

    let path = cmd.path.unwrap_or_else(|| {
        workspace_root().join("crates/tools/leek-fmt/tests/fixtures/hello.in.leek")
    });
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let version = cmd.lang_version;
    let sink = TimingSink::new();
    let pipeline =
        leek_recipes::pipeline_timed(Target::Hir, &RecipeParams::permissive(), &sink)
            .expect("recipe");
    let _run = pipeline.run(Input {
        source: SourceId::new(1).unwrap(),
        text: text.into(),
        version_byte: version,
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    });
    if !quiet {
        eprintln!("Pipeline timings for {}:", path.display());
        for entry in sink.entries() {
            eprintln!("  {:>14}: {:?}", entry.step, entry.duration);
        }
    }
    Ok(ExitCode::SUCCESS)
}
