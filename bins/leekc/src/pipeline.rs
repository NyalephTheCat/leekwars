//! What each `--emit` asks the compiler for, and diagnostic-code lookup.

use anyhow::Result;
use leek_diagnostics::Code;
use leek_session::{Scope, Target};

use crate::cli::Emit;

/// The entry file's own `SourceId`; its includes get the ones after it.
pub const ENTRY_SOURCE: u32 = 1;

/// How far the compiler has to go for `emit`, and over how much of the
/// program.
///
/// The emits that resolve names ask for [`Target::Linted`] over
/// [`Scope::Program`] — the same pair [`leek_session::DriverConfig`]'s
/// default carries, which is what `miku check` compiles with, so `leekc
/// main.leek` and `miku check` agree on what `include("helper")` means
/// (DRIVER-02).
///
/// The purely textual views of a single file (`tokens`, `flat-cst`,
/// `cst`, `fmt`) ask over [`Scope::File`]: they describe the bytes in
/// front of them, and the formatter must stay byte-faithful.
#[must_use]
pub fn shape_for(emit: Emit) -> (Target, Scope) {
    match emit {
        Emit::Check | Emit::Hir | Emit::Java | Emit::LeekScript | Emit::Run | Emit::Native => {
            (Target::Linted, Scope::Program)
        }
        Emit::Mir => (Target::Mir, Scope::Program),
        Emit::Tokens | Emit::FlatCst => (Target::Tokens, Scope::File),
        // `fmt` needs the green tree and nothing past it; the formatting
        // itself is a query off the same database, not a later pass.
        Emit::Cst | Emit::Fmt => (Target::Parsed, Scope::File),
    }
}

/// Look up a code by ID (`E0240`) or canonical name (`PrivateField`).
pub fn resolve_code(raw: &str) -> Result<Code> {
    use leek_diagnostics::codes::CATALOG;
    Code::resolve(raw).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown diagnostic code `{raw}` (try one of: {})",
            CATALOG
                .iter()
                .take(5)
                .map(|m| m.id)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;
    use leek_session::DriverConfig;

    use super::*;

    /// The target and scope each `--emit` is expected to ask for. Written
    /// out rather than derived, so a change that silently lengthens or
    /// shortens what an emit compiles has to be acknowledged here.
    fn expected_shape(emit: Emit) -> (Target, Scope) {
        match emit {
            Emit::Check | Emit::Hir | Emit::Java | Emit::LeekScript | Emit::Run | Emit::Native => {
                (Target::Linted, Scope::Program)
            }
            Emit::Mir => (Target::Mir, Scope::Program),
            Emit::Tokens | Emit::FlatCst => (Target::Tokens, Scope::File),
            Emit::Cst | Emit::Fmt => (Target::Parsed, Scope::File),
        }
    }

    #[test]
    fn every_emit_asks_for_the_shortest_compilation_that_produces_its_artifact() {
        for emit in Emit::value_variants() {
            assert_eq!(shape_for(*emit), expected_shape(*emit), "--emit {emit:?}");
        }
    }

    #[test]
    fn the_name_resolving_emits_compile_what_the_driver_compiles() {
        // DRIVER-02: `leekc` and `miku` must not disagree about whether a
        // file's `include(...)` calls are resolved. The driver's default
        // config is what every one-file `miku` subcommand uses.
        let driver = DriverConfig::default();
        assert_eq!(shape_for(Emit::Check), (driver.target, driver.scope));
    }

    #[test]
    fn only_the_textual_views_leave_includes_unresolved() {
        // The complement of the rule above, so an emit that starts
        // resolving names cannot quietly keep a file scope — which would
        // drop every definition an include provides.
        for emit in Emit::value_variants() {
            let textual = matches!(emit, Emit::Tokens | Emit::FlatCst | Emit::Cst | Emit::Fmt);
            let (_, scope) = shape_for(*emit);
            assert_eq!(
                scope == Scope::File,
                textual,
                "--emit {emit:?} has the wrong scope"
            );
        }
    }

    #[test]
    fn a_code_resolves_the_same_by_id_and_by_canonical_name() {
        let by_id = resolve_code("E0240").expect("by id");
        let by_name = resolve_code("PrivateField").expect("by name");
        assert_eq!(by_id, by_name);
        assert_eq!(by_id, leek_diagnostics::codes::PRIVATE_FIELD);

        let warn = resolve_code("W0010").expect("warning code");
        assert_eq!(warn, leek_diagnostics::codes::PRAGMA_UNKNOWN);
    }

    #[test]
    fn an_unknown_code_errors_and_suggests_real_ones() {
        let err = resolve_code("NOPE9999").expect_err("unknown code");
        let msg = err.to_string();
        assert!(msg.contains("NOPE9999"), "{msg}");
        assert!(msg.contains("unknown diagnostic code"), "{msg}");
        // The hint lists catalog entries, not an empty tail.
        assert!(
            msg.contains(leek_diagnostics::codes::CATALOG[0].id),
            "{msg}"
        );
    }

    #[test]
    fn every_catalog_code_resolves_by_both_of_its_spellings() {
        for meta in leek_diagnostics::codes::CATALOG {
            assert!(resolve_code(meta.id).is_ok(), "{}", meta.id);
            assert!(resolve_code(meta.name).is_ok(), "{}", meta.name);
        }
    }
}
