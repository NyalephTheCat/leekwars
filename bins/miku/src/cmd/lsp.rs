//! `miku lsp` — start the language server on stdio.

use std::process::ExitCode;

use anyhow::Result;

pub fn run() -> Result<ExitCode> {
    leek_lsp::run_stdio();
    Ok(ExitCode::SUCCESS)
}
