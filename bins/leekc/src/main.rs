//! `leekc` — the compiler driver.

mod cli;
mod pipeline;
mod print;
mod run;

use std::process::ExitCode;

fn main() -> ExitCode {
    match run::run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("leekc: {e:#}");
            ExitCode::from(2)
        }
    }
}
