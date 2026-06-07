//! `miku check` — diagnostics only.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_driver::{DriverConfig, run_entry};
use leek_project::Project;
use leek_recipes::{RecipeParams, Target};

use crate::cli::{ColorWhen, MessageFormat};

pub fn run(
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    _quiet: bool,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }

    let config = DriverConfig {
        target: Target::Linted,
        params: RecipeParams::default(),
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
