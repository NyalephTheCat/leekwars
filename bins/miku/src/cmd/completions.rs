//! `miku completions <shell>` — print shell completion scripts.

use std::io;

use anyhow::Result;
use clap::CommandFactory;
use clap_complete::{Shell as CcShell, generate};

use crate::cli::{Cli, Completions, Shell};

pub fn run(args: Completions) -> Result<()> {
    let target = match args.shell {
        Shell::Bash => CcShell::Bash,
        Shell::Zsh => CcShell::Zsh,
        Shell::Fish => CcShell::Fish,
        Shell::Powershell => CcShell::PowerShell,
        Shell::Elvish => CcShell::Elvish,
    };
    let mut cmd = Cli::command();
    generate(target, &mut cmd, "miku", &mut io::stdout());
    Ok(())
}
