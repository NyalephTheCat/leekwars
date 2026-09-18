//! `miku lint` — run the linter over the project.
//!
//! Scope is [`crate::cmd::scope`]'s: the entry and its include closure by
//! default, every file under `src/` and `tests/` with `--all`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;
use leek_project::Project;
use leek_query::LintGroups;
use leek_session::{CompileParams, DriverConfig, Session, Target};

use crate::cli::{ColorWhen, Lint, MessageFormat};
use crate::cmd::scope;

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
        params: CompileParams::default().with_lints(LintGroups {
            pedantic: args.pedantic,
            nursery: args.nursery,
        }),
        color: color.into(),
        format: format.into(),
        // `scope` and `timing` stay at their defaults: a compiled file
        // covers its whole include closure, not the one file, and only
        // `build --verbose` wants timings.
        ..DriverConfig::default()
    };
    let session = Session::new(&project, config)?;
    let files = scope::targets(&project, args.all);
    Ok(
        if scope::compile_and_report(&session, &files, |_, _| Ok(()))? {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        },
    )
}
