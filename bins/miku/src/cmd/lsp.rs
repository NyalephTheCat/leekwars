//! `miku lsp` — start the language server on stdio.

use std::process::ExitCode;

pub fn run() -> ExitCode {
    leek_lsp::run_stdio();
    ExitCode::SUCCESS
}
