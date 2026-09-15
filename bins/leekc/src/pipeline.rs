//! Pipeline assembly and diagnostic-code resolution.

use std::path::Path;

use anyhow::Result;
use leek_diagnostics::Code;
use leek_fmt::FormatOptions;
use leek_pipeline::{LintGroups, Pipeline};
use leek_session::{self, DriverConfig, SessionError, Target};
use leek_span::SourceId;

use crate::cli::Emit;

/// The entry file's own `SourceId`; its includes get the ones after it.
pub const ENTRY_SOURCE: u32 = 1;

/// Pick the shortest pipeline that produces the artifact `emit` needs.
///
/// The emits that resolve names go through
/// [`leek_session::standalone_pipeline`], the manifest-less half of the
/// entry point `miku` plans every file with — so `leekc main.leek` and
/// `miku check` agree on what `include("helper")` means. `input` is the
/// entry file the include graph is walked from.
///
/// The purely textual views of a single file (`tokens`, `flat-cst`,
/// `cst`, `fmt`) stay on the single-file path: they describe the bytes
/// in front of them, and the formatter must stay byte-faithful.
pub fn pipeline_for(
    emit: Emit,
    fmt_opts: FormatOptions,
    lints: LintGroups,
    input: &Path,
) -> Result<Pipeline, SessionError> {
    let params = leek_session::driver_params().with_lints(lints);
    let with_includes = |target: Target| {
        let config = DriverConfig {
            target,
            params: params.clone(),
            ..DriverConfig::default()
        };
        leek_session::standalone_pipeline(input, SourceId::new(ENTRY_SOURCE).unwrap(), &config)
    };
    match emit {
        Emit::Check | Emit::Hir | Emit::Java | Emit::LeekScript | Emit::Run | Emit::Native => {
            with_includes(Target::Linted)
        }
        Emit::Tokens | Emit::FlatCst => Ok(leek_session::pipeline(Target::Tokens, &params)?),
        Emit::Cst => Ok(leek_session::pipeline(Target::Parsed, &params)?),
        Emit::Fmt => Ok(leek_session::pipeline_formatted(fmt_opts, &params)?),
        Emit::Mir => with_includes(Target::Mir),
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

    use super::*;

    /// The front end an include-resolving emit drives. `resolve_includes`
    /// sits between lexing and parsing: it only needs the entry's tokens,
    /// and the parse consumes the include closure's class names.
    const FRONT_END_WITH_INCLUDES: [&str; 7] = [
        "pragma",
        "lex",
        "resolve_includes",
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
                steps.extend(FRONT_END_WITH_INCLUDES);
                steps.push("lint");
            }
            Emit::Tokens | Emit::FlatCst => steps.extend(["pragma", "lex"]),
            Emit::Cst => steps.extend(["pragma", "lex", "parse"]),
            Emit::Fmt => steps.extend(["pragma", "lex", "parse", "fmt"]),
            Emit::Mir => {
                steps.extend(FRONT_END_WITH_INCLUDES);
                steps.push("lower-mir");
            }
        }
        steps
    }

    /// A path that need not exist: `pipeline_for` only plans the steps,
    /// and `includes_step` falls back to the path as given when
    /// canonicalization fails.
    fn entry() -> &'static Path {
        Path::new("main.leek")
    }

    #[test]
    fn every_emit_maps_to_the_shortest_pipeline_that_produces_its_artifact() {
        for emit in Emit::value_variants() {
            let pipeline = pipeline_for(
                *emit,
                FormatOptions::default(),
                LintGroups::default(),
                entry(),
            )
            .expect("planned");
            assert_eq!(
                pipeline.step_names(),
                expected_steps(*emit),
                "--emit {emit:?}"
            );
        }
    }

    #[test]
    fn the_name_resolving_emits_plan_the_same_front_end_as_the_driver() {
        // DRIVER-02: `leekc` and `miku` must not disagree about whether a
        // file's `include(...)` calls are resolved. Both plan through
        // `leek_session`, so the step sequence is identical.
        let driver = leek_session::standalone_pipeline(
            entry(),
            SourceId::new(ENTRY_SOURCE).unwrap(),
            &DriverConfig::default(),
        )
        .expect("driver pipeline");
        let leekc = pipeline_for(
            Emit::Check,
            FormatOptions::default(),
            LintGroups::default(),
            entry(),
        )
        .expect("planned");
        assert_eq!(leekc.step_names(), driver.step_names());
    }

    #[test]
    fn no_emit_builds_an_empty_pipeline() {
        // Every emit plans *something*: an empty plan would mean the emit
        // silently produces nothing at all.
        for emit in Emit::value_variants() {
            let pipeline = pipeline_for(
                *emit,
                FormatOptions::default(),
                LintGroups::default(),
                entry(),
            )
            .expect("planned");
            assert!(!pipeline.is_empty(), "--emit {emit:?} planned no steps");
        }
    }

    #[test]
    fn lint_groups_do_not_change_the_pass_sequence() {
        // `--pedantic` / `--nursery` widen what the lint step reports; they
        // must not add or drop a pass.
        let plain = pipeline_for(
            Emit::Check,
            FormatOptions::default(),
            LintGroups::default(),
            entry(),
        )
        .expect("planned");
        let loud = pipeline_for(
            Emit::Check,
            FormatOptions::default(),
            LintGroups {
                pedantic: true,
                nursery: true,
            },
            entry(),
        )
        .expect("planned");
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
