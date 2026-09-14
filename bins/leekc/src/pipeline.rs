//! Pipeline assembly and diagnostic-code resolution.

use anyhow::Result;
use leek_diagnostics::Code;
use leek_fmt::FormatOptions;
use leek_pipeline::{LintGroups, Pipeline};
use leek_recipes::{self, Target};

use crate::cli::Emit;

/// Pick the shortest pipeline that produces the artifact `emit` needs.
pub fn pipeline_for(emit: Emit, fmt_opts: FormatOptions, lints: LintGroups) -> Pipeline {
    let params = leek_recipes::driver_params().with_lints(lints);
    match emit {
        Emit::Check | Emit::Hir | Emit::Java | Emit::LeekScript | Emit::Run | Emit::Native => {
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

/// True if stderr is attached to a terminal. Honors `NO_COLOR`.
pub fn is_stderr_tty() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum;

    use super::*;

    const FRONT_END: [&str; 6] = [
        "pragma",
        "lex",
        "parse",
        "resolve",
        "type-check",
        "lower-hir",
    ];

    /// The pass sequence each `--emit` is expected to drive. Written out
    /// rather than derived, so a recipe change that silently lengthens or
    /// shortens a pipeline has to be acknowledged here.
    fn expected_steps(emit: Emit) -> Vec<&'static str> {
        let mut steps = Vec::new();
        match emit {
            // Every artifact-producing emit needs the full front end; the
            // lint step rides along because `Target::Linted` is the check
            // recipe these emits share.
            Emit::Check | Emit::Hir | Emit::Java | Emit::LeekScript | Emit::Run | Emit::Native => {
                steps.extend(FRONT_END);
                steps.push("lint");
            }
            Emit::Tokens | Emit::FlatCst => steps.extend(["pragma", "lex"]),
            Emit::Cst => steps.extend(["pragma", "lex", "parse"]),
            Emit::Fmt => steps.extend(["pragma", "lex", "parse", "fmt"]),
            Emit::Mir => {
                steps.extend(FRONT_END);
                steps.push("lower-mir");
            }
        }
        steps
    }

    #[test]
    fn every_emit_maps_to_the_shortest_pipeline_that_produces_its_artifact() {
        for emit in Emit::value_variants() {
            let pipeline = pipeline_for(*emit, FormatOptions::default(), LintGroups::default());
            assert_eq!(
                pipeline.step_names(),
                expected_steps(*emit),
                "--emit {emit:?}"
            );
        }
    }

    #[test]
    fn no_emit_builds_an_empty_pipeline() {
        // `pipeline_for` unwraps the recipe; an empty plan would mean the
        // emit silently produces nothing at all.
        for emit in Emit::value_variants() {
            let pipeline = pipeline_for(*emit, FormatOptions::default(), LintGroups::default());
            assert!(!pipeline.is_empty(), "--emit {emit:?} planned no steps");
        }
    }

    #[test]
    fn lint_groups_do_not_change_the_pass_sequence() {
        // `--pedantic` / `--nursery` widen what the lint step reports; they
        // must not add or drop a pass.
        let plain = pipeline_for(Emit::Check, FormatOptions::default(), LintGroups::default());
        let loud = pipeline_for(
            Emit::Check,
            FormatOptions::default(),
            LintGroups {
                pedantic: true,
                nursery: true,
            },
        );
        assert_eq!(plain.step_names(), loud.step_names());
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
