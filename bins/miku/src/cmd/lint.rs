//! `miku lint` — run the linter across the project's entry.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_driver::{DriverConfig, run_entry};
use leek_pipeline::LintGroups;
use leek_project::Project;
use leek_recipes::{RecipeParams, Target};

use crate::cli::{ColorWhen, Lint, MessageFormat};

pub fn run(
    args: &Lint,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    _quiet: bool,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    // CLI flags are OR'd with the manifest's `[lint]` table by the
    // driver, so a flag can only widen what Miku.toml asks for.
    let config = DriverConfig {
        target: Target::Linted,
        params: RecipeParams::default().with_lints(LintGroups {
            pedantic: args.pedantic,
            nursery: args.nursery,
        }),
        color: color.into(),
        format: format.into(),
    };
    let driver_run = run_entry(&project, &config)?;
    Ok(if driver_run.had_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
