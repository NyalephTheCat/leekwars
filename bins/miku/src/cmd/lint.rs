//! `miku lint` — run the linter across the project's entry.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_pipeline::LintGroups;
use leek_project::Project;
use leek_session::{DriverConfig, RecipeParams, Session, Target};

use crate::cli::{ColorWhen, Lint, MessageFormat};

pub fn run(
    args: &Lint,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    _quiet: bool,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    if leek_session::report_manifest(&project, color.into(), format.into()) {
        return Ok(ExitCode::from(1));
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
        timing: None,
    };
    let session = Session::new(&project, config)?;
    Ok(if session.compile_entry()?.report() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
