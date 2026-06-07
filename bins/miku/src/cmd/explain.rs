//! `miku explain <CODE>` — print the extended write-up for a diagnostic
//! code, the equivalent of `rustc --explain`.

use std::process::ExitCode;

use leek_diagnostics::Code;
use leek_diagnostics::codes::CATALOG;

use crate::cli::Explain;

pub fn run(args: &Explain) -> ExitCode {
    let query = args.code.trim().to_ascii_uppercase();

    // Resolve the query against the catalog so we get back the `'static`
    // id needed to look up the explanation.
    let Some(meta) = CATALOG.iter().find(|m| m.id == query) else {
        eprintln!("miku: unknown diagnostic code `{}`", args.code);
        print_available();
        return ExitCode::from(2);
    };

    let Some(text) = Code(meta.id).explain() else {
        eprintln!(
            "miku: no extended explanation for `{}` ({}) yet",
            meta.id, meta.name
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
