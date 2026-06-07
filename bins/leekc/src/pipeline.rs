//! Pipeline assembly and diagnostic-code resolution.

use anyhow::Result;
use leek_diagnostics::Code;
use leek_fmt::FormatOptions;
use leek_pipeline::Pipeline;
use leek_recipes::{self, Target};

use crate::cli::Emit;

/// Pick the shortest pipeline that produces the artifact `emit` needs.
pub fn pipeline_for(emit: Emit, fmt_opts: FormatOptions) -> Pipeline {
    let params = leek_recipes::driver_params();
    match emit {
        Emit::Check | Emit::Hir | Emit::Java | Emit::Run | Emit::Native => {
            leek_recipes::pipeline(Target::Linted, &params).expect("recipe")
        }
        Emit::Tokens | Emit::FlatCst => {
            leek_recipes::pipeline(Target::Tokens, &params).expect("recipe")
        }
        Emit::Cst => leek_recipes::pipeline(Target::Parsed, &params).expect("recipe"),
        Emit::Fmt => leek_recipes::pipeline_formatted(fmt_opts, &params).expect("recipe"),
        Emit::Mir => leek_recipes::pipeline(Target::Mir, &params).expect("recipe"),
    }
}

/// Look up a code by ID (`E0240`) or canonical name (`PrivateField`).
pub fn resolve_code(raw: &str) -> Result<Code> {
    use leek_diagnostics::codes::CATALOG;
    if let Some(meta) = CATALOG.iter().find(|m| m.id == raw || m.name == raw) {
        Ok(Code(meta.id))
    } else {
        anyhow::bail!(
            "unknown diagnostic code `{raw}` (try one of: {})",
            CATALOG
                .iter()
                .take(5)
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// True if stderr is attached to a terminal. Honors `NO_COLOR`.
pub fn is_stderr_tty() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stderr().is_terminal()
}
