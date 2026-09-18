//! End-to-end `@allow(LXXXX)` suppression tests.

use leek_project::Input;
use leek_span::SourceId;

fn lint_with_allows(src: &str) -> Vec<leek_diagnostics::Diagnostic> {
    let input = Input {
        source: SourceId::new(1).unwrap(),
        text: src.to_string().into(),
        version_byte: 4,
        strict: false,
        flags: leek_span::FeatureFlags::from_env(),
    };
    let db = leek_db::LeekDb::default();
    let file = leek_db::input_file(&db, String::new(), &input);
    leek_lint::query::lint_query(&db, file, leek_lint::LintGroups::default())
        .as_ref()
        .clone()
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

/// Regression: a trailing annotation used to claim the *next*
/// statement, because the newline in the whitespace between them was
/// ignored. `var dead = 1 // @allow(…)` now silences `dead` and leaves
/// the statement below it alone.
#[test]
fn trailing_allow_claims_the_statement_it_trails() {
    let src = "function f() {\nvar dead = 1 // @allow(L0001)\nvar other = 2\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    let unused: Vec<&str> = diags
        .iter()
        .filter(|d| d.code == leek_diagnostics::codes::UNUSED_VARIABLE)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(unused.len(), 1, "exactly one survivor; got {diags:?}");
    assert!(
        unused[0].contains("other"),
        "the annotation covers `dead`, not the statement below it: {unused:?}"
    );
}

/// The same comment one line down is a *leading* annotation again, so
/// the trailing rule doesn't leak into the ordinary form.
#[test]
fn leading_allow_still_claims_the_statement_below() {
    let src = "function f() {\nvar dead = 1\n// @allow(L0001)\nvar other = 2\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    let unused: Vec<&str> = diags
        .iter()
        .filter(|d| d.code == leek_diagnostics::codes::UNUSED_VARIABLE)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(unused.len(), 1, "exactly one survivor; got {diags:?}");
    assert!(
        unused[0].contains("dead"),
        "the annotation covers `other`: {unused:?}"
    );
}

/// A trailing block comment works the same way.
#[test]
fn trailing_block_comment_allow_claims_its_statement() {
    let src = "function f() {\nvar dead = 1 /* @allow(unused-variable) */\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != leek_diagnostics::codes::UNUSED_VARIABLE),
        "expected no L0001 findings, got {diags:?}"
    );
}

/// A typo in an annotation suppresses nothing, so it is reported —
/// with the name it probably meant.
#[test]
fn unknown_lint_name_is_reported() {
    let src = "function f() {\n// @allow(unused-varible)\nvar dead = 1\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    let unknown: Vec<_> = diags
        .iter()
        .filter(|d| d.code == leek_diagnostics::codes::UNKNOWN_LINT_IN_ALLOW)
        .collect();
    assert_eq!(unknown.len(), 1, "expected one W0600, got {diags:?}");
    let d = unknown[0];
    assert!(
        d.message.contains("unused-varible"),
        "message names the offending text: {}",
        d.message
    );
    assert!(
        d.notes.iter().any(|n| n.contains("unused-variable")),
        "expected a did-you-mean note, got {:?}",
        d.notes
    );
    // The caret points at the name itself, not at the whole comment.
    let at = usize::try_from(d.span.start).unwrap();
    assert_eq!(&src[at..at + "unused-varible".len()], "unused-varible");
    // And the typo really did suppress nothing.
    assert!(
        diags
            .iter()
            .any(|d| d.code == leek_diagnostics::codes::UNUSED_VARIABLE),
        "the finding the typo failed to suppress is still reported: {diags:?}"
    );
}

/// A code outside the lint range can never suppress a lint finding, so
/// it is reported too.
#[test]
fn non_lint_code_in_allow_is_reported() {
    let src = "function f() {\n// @allow(E0200)\nvar dead = 1\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .any(|d| d.code == leek_diagnostics::codes::UNKNOWN_LINT_IN_ALLOW),
        "expected a W0600 for `E0200`, got {diags:?}"
    );
}

/// Only the unresolvable name is reported; the ones beside it still
/// suppress.
#[test]
fn a_bad_name_does_not_spoil_its_neighbours() {
    let src = "function f() {\n// @allow(L0001, L001)\nvar dead = 1\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != leek_diagnostics::codes::UNUSED_VARIABLE),
        "`L0001` still suppresses: {diags:?}"
    );
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.code == leek_diagnostics::codes::UNKNOWN_LINT_IN_ALLOW)
            .count(),
        1,
        "exactly the one bad name is reported: {diags:?}"
    );
}

/// Well-spelled annotations are silent.
#[test]
fn known_names_report_nothing() {
    let src = "function f() {\n// @allow(L0001, unused-variable, all)\nvar dead = 1\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != leek_diagnostics::codes::UNKNOWN_LINT_IN_ALLOW),
        "no annotation findings expected, got {diags:?}"
    );
}

/// The `@lint:` prefix the module docs advertise really is accepted.
#[test]
fn lint_prefixed_synonym_suppresses() {
    let src = "function f() {\n// @lint:allow(L0001)\nvar dead = 1\nreturn 0\n}\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != leek_diagnostics::codes::UNUSED_VARIABLE),
        "expected no L0001 findings, got {diags:?}"
    );
}

/// `@allow-file(…)` in the header band covers every statement in the
/// file, not just the one under it.
#[test]
fn allow_file_covers_the_whole_file() {
    let src =
        "// @allow-file(unused-variable)\nfunction f() {\nvar dead = 1\nreturn 0\n}\nvar top = 2\n";
    let diags = lint_with_allows(src);
    assert!(
        diags
            .iter()
            .all(|d| d.code != leek_diagnostics::codes::UNUSED_VARIABLE),
        "both `dead` and `top` are covered, got {diags:?}"
    );
}

/// A statement-scoped annotation is still narrow — the file form is
/// the only one that reaches past its own statement.
#[test]
fn statement_allow_does_not_cover_the_whole_file() {
    let src = "// @allow(unused-variable)\nvar first = 1\nvar second = 2\n";
    let diags = lint_with_allows(src);
    let unused: Vec<&str> = diags
        .iter()
        .filter(|d| d.code == leek_diagnostics::codes::UNUSED_VARIABLE)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(unused.len(), 1, "only `first` is covered: {diags:?}");
    assert!(unused[0].contains("second"), "{unused:?}");
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
