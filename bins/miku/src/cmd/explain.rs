//! `miku explain <CODE>` — print the extended write-up for a diagnostic
//! code, the equivalent of `rustc --explain`.

use std::process::ExitCode;

use leek_diagnostics::Code;
use leek_diagnostics::codes::CATALOG;

use crate::cli::Explain;

pub fn run(args: &Explain) -> ExitCode {
    let trimmed = args.code.trim();

    // Resolve the query against the catalog so we get back the `'static`
    // id needed to look up the explanation. Accept the canonical name
    // (`PrivateField`) as well as the id (`E0240`, case-insensitive).
    let Some(code) =
        Code::resolve(trimmed).or_else(|| Code::resolve(&trimmed.to_ascii_uppercase()))
    else {
        eprintln!("miku: unknown diagnostic code `{}`", args.code);
        print_available();
        return ExitCode::from(2);
    };

    let Some(text) = code.explain() else {
        eprintln!(
            "miku: no extended explanation for `{}` ({}) yet",
            code.id(),
            code.name()
        );
        print_available();
        return ExitCode::from(1);
    };

    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    ExitCode::SUCCESS
}

/// List the codes that currently have an extended explanation, so a user
/// who guessed wrong sees what they *can* look up.
fn print_available() {
    let mut available: Vec<&str> = CATALOG
        .iter()
        .filter(|m| Code(m.id).explain().is_some())
        .map(|m| m.id)
        .collect();
    available.sort_unstable();
    if available.is_empty() {
        return;
    }
    eprintln!("\nextended explanations are available for:");
    eprintln!("  {}", available.join(", "));
}
