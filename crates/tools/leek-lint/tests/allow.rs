//! End-to-end `@allow(LXXXX)` suppression tests.

use leek_lint::pipeline::LintFindings;
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target};
use leek_span::SourceId;

fn lint_with_allows(src: &str) -> Vec<leek_diagnostics::Diagnostic> {
    let input = Input {
        source: SourceId::new(1).unwrap(),
        text: src.to_string().into(),
        version_byte: 4,
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let pipeline =
        leek_recipes::pipeline(Target::Linted, &RecipeParams::permissive()).expect("recipe");
    let run = pipeline.run(input);
    run.get::<LintFindings>()
        .map(|f| f.0.clone())
        .unwrap_or_default()
}

#[test]
fn allow_suppresses_matching_code() {
    let src = "function f() {\n// @allow(L0001)\nvar dead = 1\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    // L0001 UnusedVariable would normally fire; the annotation
    // suppresses it.
    assert!(
        diags
            .iter()
            .all(|d| d.code != leek_diagnostics::codes::UNUSED_VARIABLE),
        "expected no L0001 findings, got {diags:?}"
    );
}

#[test]
fn allow_does_not_suppress_other_codes() {
    // `// @allow(L0001)` shouldn't hide an L0006 finding.
    let src = "function f() {\n// @allow(L0001)\nif (true) { return 1 }\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    let l0006 = diags
        .iter()
        .filter(|d| d.code == leek_diagnostics::codes::CONSTANT_CONDITION)
        .count();
    assert_eq!(l0006, 1, "L0006 should still fire; got {diags:?}");
}

#[test]
fn allow_multiple_codes() {
    let src = "function f() {\n// @allow(L0001, L0006)\nif (true) { var dead = 1\nreturn 0 }\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    let suppressed_codes = [
        leek_diagnostics::codes::UNUSED_VARIABLE,
        leek_diagnostics::codes::CONSTANT_CONDITION,
    ];
    for c in suppressed_codes {
        assert!(
            !diags.iter().any(|d| d.code == c),
            "code {} should be suppressed; got {diags:?}",
            c.id()
        );
    }
}

#[test]
fn allow_suppress_synonym() {
    // `@suppress` is an accepted alias for `@allow`.
    let src = "function f() {\n// @suppress(L0001)\nvar dead = 1\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != leek_diagnostics::codes::UNUSED_VARIABLE)
    );
}

#[test]
fn allow_accepts_rule_name() {
    // `@allow(unused-variable)` (the kebab rule name) suppresses L0001
    // just like `@allow(L0001)` would.
    let src = "function f() {\n// @allow(unused-variable)\nvar dead = 1\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != leek_diagnostics::codes::UNUSED_VARIABLE),
        "rule-name allow should suppress L0001, got {diags:?}"
    );
}

#[test]
fn allow_by_name_does_not_suppress_others() {
    // Allowing one rule by name must not hide an unrelated finding.
    let src = "function f() {\n// @allow(unused-variable)\nvar dead = 1\nif (true) { return 1 }\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .any(|d| d.code == leek_diagnostics::codes::CONSTANT_CONDITION),
        "the constant-condition finding should survive, got {diags:?}"
    );
}

#[test]
fn allow_all_suppresses_every_lint_on_statement() {
    // `@allow(all)` is a catch-all: a single annotated statement that
    // would trip multiple lints goes silent.
    let src = "function f() {\n// @allow(all)\nif (true) { var dead = 1\nreturn 0 }\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        !diags
            .iter()
            .any(|d| d.code == leek_diagnostics::codes::UNUSED_VARIABLE
                || d.code == leek_diagnostics::codes::CONSTANT_CONDITION),
        "@allow(all) should suppress both L0001 and L0006, got {diags:?}"
    );
}
