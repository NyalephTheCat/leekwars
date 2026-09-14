//! `miku lsp` — start the language server on stdio.

use std::process::ExitCode;

use anyhow::{Context, Result};

/// Serve until the editor disconnects, then hand the exit code back to
/// `miku` so its own teardown runs. `leek_lsp::run_stdio` used to end in
/// `process::exit(0)`, which made this subcommand unable to fail and left
/// the `ExitCode` below unreachable.
pub fn run() -> Result<ExitCode> {
    // The subscriber belongs to the binary, not to `leek-lsp`: see
    // `leek_lsp::log`.
    leek_lsp::log::init();
    leek_lsp::run_stdio().context("running the Leekscript language server on stdio")?;
    Ok(ExitCode::SUCCESS)
}
