//! Repository automation, run as `cargo xtask <task>` (the alias lives in
//! `.cargo/config.toml`).

mod artifacts;
mod errors;
mod fmt_idiom;
mod graph;
mod layers;
mod toolchain;

use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <task>

tasks:
  check-artifacts  check generated output stays untracked and ignored
  check-errors     keep `anyhow` out of the library layers (see docs/architecture.md)
  check-fmt-idiom  keep `write!` into a `String` on one spelling: `let _ = write!(…)`
  check-layers     enforce the crate layering rule (see docs/architecture.md)
  check-toolchain  check the Rust pin and the advertised MSRV agree
  graph            regenerate docs/crate-graph.md from cargo metadata
                   (--check fails instead, when it is out of date)";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("check-artifacts") => artifacts::run(),
        Some("check-errors") => errors::run(),
        Some("check-fmt-idiom") => fmt_idiom::run(),
        Some("check-layers") => layers::run(),
        Some("check-toolchain") => toolchain::run(),
        Some("graph") => graph::run(args.next().as_deref() == Some("--check")),
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
