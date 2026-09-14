//! The shape of the *real* recipes, target by target.
//!
//! `leek-recipes` derives these chains from the artifact graph rather than
//! writing them out, so a wrong `Requires`/`Produces` in any pass crate
//! silently reorders or drops a pass. Spelling the expected sequences out
//! here turns that into a failing assertion naming the offending target.

use leek_pipeline::{Context, Step, Tap};
use leek_recipes::{Target, driver_params, lsp_params, plan, plan_with_includes};

fn names(target: Target) -> Vec<&'static str> {
    plan(target, &driver_params())
        .unwrap_or_else(|e| panic!("planning {target:?}: {e}"))
        .step_names()
}

#[test]
fn every_target_plans_the_expected_pass_sequence() {
    assert_eq!(names(Target::Tokens), ["pragma", "lex"]);
    assert_eq!(names(Target::Parsed), ["pragma", "lex", "parse"]);
    assert_eq!(
        names(Target::Resolved),
        ["pragma", "lex", "parse", "resolve"]
    );
    assert_eq!(
        names(Target::TypeChecked),
        ["pragma", "lex", "parse", "resolve", "type-check"]
    );
    assert_eq!(
        names(Target::Hir),
        [
            "pragma",
            "lex",
            "parse",
            "resolve",
            "type-check",
            "lower-hir"
        ]
    );
    assert_eq!(
        names(Target::Linted),
        [
            "pragma",
            "lex",
            "parse",
            "resolve",
            "type-check",
            "lower-hir",
            "lint"
        ]
    );
    assert_eq!(
        names(Target::Mir),
        [
            "pragma",
            "lex",
            "parse",
            "resolve",
            "type-check",
            "lower-hir",
            "lower-mir"
        ]
    );
    assert_eq!(
        names(Target::Complexity),
        [
            "pragma",
            "lex",
            "parse",
            "resolve",
            "type-check",
            "lower-hir",
            "complexity"
        ]
    );
}

#[test]
fn parse_is_planned_once_even_though_it_yields_two_artifacts() {
    // `Parse` produces both the green tree and the AST; `Resolved` needs the
    // AST while `Parsed` needs the tree. Neither may plan `parse` twice.
    for target in [Target::Parsed, Target::Resolved, Target::Linted] {
        let planned = names(target);
        assert_eq!(
            planned.iter().filter(|n| **n == "parse").count(),
            1,
            "{target:?}: {planned:?}"
        );
    }
}

#[test]
fn lsp_params_plan_the_same_passes_as_the_one_shot_driver() {
    // The LSP only drops the stop-on-error *wrapping*; the pass sequence
    // must stay identical or hover/completion would see a different tree.
    for target in [Target::Linted, Target::Mir, Target::Complexity] {
        let driver = plan(target, &driver_params()).expect("driver plan");
        let lsp = plan(target, &lsp_params()).expect("lsp plan");
        assert_eq!(driver.step_names(), lsp.step_names(), "{target:?}");
    }
}

fn fake_includes() -> Box<dyn Step> {
    Box::new(Tap::new("resolve-includes", |_: &mut Context<'_>| {}))
}

#[test]
fn include_resolution_is_sequenced_between_lexing_and_parsing() {
    // The entry parse consumes the include closure's class names, so the
    // includes step must run after `lex` and before `parse`. Moving it is
    // what broke `lowercaseClassFromInclude x = …` before.
    for target in [
        Target::Parsed,
        Target::Resolved,
        Target::TypeChecked,
        Target::Hir,
        Target::Linted,
        Target::Mir,
        Target::Complexity,
    ] {
        let planned = plan_with_includes(target, fake_includes(), &driver_params())
            .unwrap_or_else(|e| panic!("planning {target:?}: {e}"))
            .step_names();
        let at = |name: &str| {
            planned
                .iter()
                .position(|n| *n == name)
                .unwrap_or_else(|| panic!("{target:?} has no `{name}` step: {planned:?}"))
        };
        assert!(
            at("lex") < at("resolve-includes"),
            "{target:?}: {planned:?}"
        );
        assert!(
            at("resolve-includes") < at("parse"),
            "{target:?}: {planned:?}"
        );
    }
}

#[test]
fn include_resolution_runs_even_when_only_tokens_are_wanted() {
    let planned = plan_with_includes(Target::Tokens, fake_includes(), &driver_params())
        .expect("plan")
        .step_names();
    assert_eq!(planned, ["pragma", "lex", "resolve-includes"]);
}
