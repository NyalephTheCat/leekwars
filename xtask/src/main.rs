//! Repository automation, run as `cargo xtask <task>` (the alias lives in
//! `.cargo/config.toml`).

mod layers;

use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <task>

tasks:
  check-layers   enforce the crate layering rule (see docs/architecture.md)";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("check-layers") => layers::run(),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("xtask: unknown task `{other}`\n\n{USAGE}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}
