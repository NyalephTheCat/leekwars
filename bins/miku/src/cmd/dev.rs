//! Developer hygiene commands — layer checks, builtin drift, etc.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result};

use crate::cli::{Dev, DevCommand};

pub fn run(args: Dev, quiet: bool) -> Result<ExitCode> {
    match args.command {
        DevCommand::Layers => run_cargo(&LAYERS_ARGS, quiet),
        DevCommand::Builtins => run_tool("builtin-extract.sh", &["--check"], quiet),
        DevCommand::BuiltinSuite => {
            run_cargo(&["run", "-p", "leek-builtin-suite", "--quiet"], quiet)
        }
        DevCommand::Pipeline(cmd) => pipeline(cmd, quiet),
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// Cargo arguments for `miku dev layers`: the `xtask` layer check, spelled
/// out rather than through the `cargo xtask` alias so it does not depend on
/// `.cargo/config.toml` being picked up.
const LAYERS_ARGS: [&str; 6] = ["run", "-p", "xtask", "--quiet", "--", "check-layers"];

/// Runs `cargo <args>` from the workspace root, using `$CARGO` when set.
fn run_cargo(args: &[&str], quiet: bool) -> Result<ExitCode> {
    let root = workspace_root();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let command_line = format!("cargo {}", args.join(" "));
    if !quiet {
        eprintln!("miku dev: {command_line}");
    }
    let status = Command::new(cargo)
        .args(args)
        .current_dir(&root)
        .status()
        .with_context(|| command_line.clone())?;
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
    let pipeline = leek_recipes::pipeline_timed(Target::Hir, &RecipeParams::permissive(), &sink)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_runs_the_xtask_check() {
        let root = workspace_root();
        assert_eq!(&LAYERS_ARGS[..3], ["run", "-p", "xtask"]);
        assert_eq!(LAYERS_ARGS.last(), Some(&"check-layers"));
        assert!(
            root.join("xtask/Cargo.toml").is_file(),
            "the xtask package invoked by `miku dev layers` must exist"
        );
    }

    #[test]
    fn referenced_tool_scripts_exist() {
        let script = workspace_root().join("tools/builtin-extract.sh");
        assert!(script.is_file(), "missing {}", script.display());
    }
}
