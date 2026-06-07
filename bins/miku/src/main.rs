//! `miku` — Leekscript workspace tool (cargo-equivalent).
//!
//! Reads `Miku.toml`, drives the compiler pipeline in-process, and
//! dispatches to backends, formatter, linter, LSP, and test runner.

use std::process::ExitCode;

mod cli;
mod cmd;
mod util;

fn main() -> ExitCode {
    let cli = <cli::Cli as clap::Parser>::parse();
    match cmd::dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("miku: {e:#}");
            ExitCode::from(2)
        }
    }
}
