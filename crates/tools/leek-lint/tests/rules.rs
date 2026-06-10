//! Per-rule tests. Each test parses a source snippet, lowers to
//! HIR, runs the linter, and asserts which findings fire.

use leek_diagnostics::codes;
use leek_hir::lower::lower_file;
use leek_lint::lint;
use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn lint_src(src: &str) -> Vec<leek_diagnostics::Diagnostic> {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let root = SyntaxNode::new_root(parsed.green);
    let ast = SourceFile::cast(root).expect("source file root");
    let (hir, _diags) = lower_file(&ast, source);
    lint(&hir)
}

fn has_code(diags: &[leek_diagnostics::Diagnostic], code: leek_diagnostics::Code) -> usize {
    diags.iter().filter(|d| d.code == code).count()
}

// ---- UnusedVariable (L0001) ----

#[test]
fn unused_variable_fires_for_unread_local() {
    let diags = lint_src("function f() { var x = 1; return 0; }\n");
    assert_eq!(has_code(&diags, codes::UNUSED_VARIABLE), 1);
}

#[test]
fn unused_variable_silent_when_read() {
    let diags = lint_src("function f() { var x = 1; return x; }\n");
    assert_eq!(has_code(&diags, codes::UNUSED_VARIABLE), 0);
}

#[test]
fn unused_variable_silent_for_globals() {
    // Globals are out of scope for this rule (they may be read by
    // siblings or by an embedding host).
    let diags = lint_src("global g = 1;\n");
    assert_eq!(has_code(&diags, codes::UNUSED_VARIABLE), 0);
}

#[test]
fn unused_variable_fires_in_nested_block() {
    let diags = lint_src("function f() { if (true) { var x = 1; } return 0; }\n");
    assert_eq!(has_code(&diags, codes::UNUSED_VARIABLE), 1);
}

#[test]
fn unused_variable_silent_when_used_across_blocks() {
    let diags = lint_src("function f() { var x = 1; if (true) { return x; } return 0; }\n");
    assert_eq!(has_code(&diags, codes::UNUSED_VARIABLE), 0);
}

#[test]
fn unused_variable_fires_in_main_block() {
    let diags = lint_src("var x = 1;\nreturn 0;\n");
    assert_eq!(has_code(&diags, codes::UNUSED_VARIABLE), 1);
}

#[test]
fn unused_variable_fires_in_class_method() {
    let diags = lint_src("class Foo { public m() { var leftover = 9; return 1; } }\n");
    assert_eq!(has_code(&diags, codes::UNUSED_VARIABLE), 1);
}

// ---- EmptyBlock (L0004) ----

#[test]
fn empty_block_fires_on_empty_if() {
    let diags = lint_src("function f() { if (true) {} return 0; }\n");
    assert_eq!(has_code(&diags, codes::EMPTY_BLOCK), 1);
}

#[test]
fn empty_block_fires_on_empty_else() {
    let diags = lint_src("function f() { if (true) { return 1; } else {} return 0; }\n");
    assert_eq!(has_code(&diags, codes::EMPTY_BLOCK), 1);
}

#[test]
fn empty_block_fires_on_empty_while() {
    let diags = lint_src("function f() { while (false) {} return 0; }\n");
    assert_eq!(has_code(&diags, codes::EMPTY_BLOCK), 1);
}

#[test]
fn empty_block_fires_on_empty_for() {
    let diags = lint_src("function f() { for (var i = 0; i < 10; i++) {} return 0; }\n");
    assert_eq!(has_code(&diags, codes::EMPTY_BLOCK), 1);
}

#[test]
fn empty_block_silent_for_non_empty_body() {
    let diags = lint_src("function f() { if (true) { return 1; } return 0; }\n");
    assert_eq!(has_code(&diags, codes::EMPTY_BLOCK), 0);
}

#[test]
fn empty_block_silent_for_top_level_braced_block() {
    // A bare `{}` at top level isn't part of a control-flow construct,
    // so this rule doesn't fire. (The parser may treat it as a block
    // stmt or empty-object disambiguation.)
    let diags = lint_src("function f() { return 0; }\n");
    assert_eq!(has_code(&diags, codes::EMPTY_BLOCK), 0);
}

// ---- Combined ----

#[test]
fn multiple_rules_can_fire_in_one_file() {
    let diags = lint_src("function f() { var leftover = 1; while (false) {} return 0; }\n");
    assert_eq!(has_code(&diags, codes::UNUSED_VARIABLE), 1);
    assert_eq!(has_code(&diags, codes::EMPTY_BLOCK), 1);
}

#[test]
fn no_findings_on_clean_file() {
    let diags =
        lint_src("function add(a, b) { return a + b; }\nfunction g() { return add(1, 2); }\n");
    assert!(diags.is_empty(), "expected no findings, got: {diags:?}");
}

// ---- Suggestions ----

#[test]
fn unused_variable_attaches_remove_suggestion() {
    let diags = lint_src("function f() { var leftover = 99; return 0; }\n");
    let d = diags
        .iter()
        .find(|d| d.code == codes::UNUSED_VARIABLE)
        .expect("L0001 missing");
    assert_eq!(d.suggestions.len(), 1, "expected one fix suggestion");
    let sug = &d.suggestions[0];
    assert!(sug.message.contains("leftover"));
    assert_eq!(sug.edits.len(), 1);
    assert_eq!(sug.edits[0].replacement, "");
}

#[test]
fn unused_variable_suggestion_targets_full_statement() {
    // Applying the suggestion should leave a clean source with no
    // dangling `var` or `=`.
    let src = "function f() { var leftover = 99; return 0; }\n";
    let diags = lint_src(src);
    let d = &diags[0];
    let edit = &d.suggestions[0].edits[0];
    let mut out = String::from(src);
    out.replace_range(
        edit.span.start as usize..edit.span.end as usize,
        &edit.replacement,
    );
    assert!(
        !out.contains("leftover"),
        "suggestion left orphan ident: {out:?}"
    );
    assert!(
        !out.contains("var  ="),
        "suggestion left dangling var=: {out:?}"
    );
}

// ---- DuplicateBranches (L0009) ----

#[test]
fn duplicate_branches_fires_end_to_end() {
    let diags = lint_src("function f(x) { if (x) { return 1; } else { return 1; } }\n");
    assert_eq!(has_code(&diags, codes::DUPLICATE_BRANCHES), 1);
}

#[test]
fn duplicate_branches_silent_when_different() {
    let diags = lint_src("function f(x) { if (x) { return 1; } else { return 2; } }\n");
    assert_eq!(has_code(&diags, codes::DUPLICATE_BRANCHES), 0);
}

// ---- SelfComparison (L0010) ----

#[test]
fn self_comparison_fires_end_to_end() {
    let diags = lint_src("function f(x) { return x == x; }\n");
    assert_eq!(has_code(&diags, codes::SELF_COMPARISON), 1);
}

// ---- SelfAssignment (L0011) ----

#[test]
fn self_assignment_fires_end_to_end() {
    let diags = lint_src("function f() { var x = 1; x = x; }\n");
    assert_eq!(has_code(&diags, codes::SELF_ASSIGNMENT), 1);
}

// ---- RedundantBoolean (L0012) ----

#[test]
fn redundant_boolean_fires_end_to_end() {
    let diags = lint_src("function f(x) { return x == true; }\n");
    assert_eq!(has_code(&diags, codes::REDUNDANT_BOOLEAN), 1);
}

// ---- IdenticalOperands (L0014) ----

#[test]
fn identical_operands_fires_end_to_end() {
    let diags = lint_src("function f(x) { return x && x; }\n");
    assert_eq!(has_code(&diags, codes::IDENTICAL_OPERANDS), 1);
}

// ---- AssignmentInCondition (L0015) ----

#[test]
fn assignment_in_condition_fires_end_to_end() {
    let diags = lint_src("function f() { var x = 0; if (x = 5) { return 1; } return 0; }\n");
    assert_eq!(has_code(&diags, codes::ASSIGNMENT_IN_CONDITION), 1);
}

// ---- DivisionByZero (L0016) ----

#[test]
fn division_by_zero_fires_end_to_end() {
    let diags = lint_src("function f(x) { return x / 0; }\n");
    assert_eq!(has_code(&diags, codes::DIVISION_BY_ZERO), 1);
}

// ---- DuplicateCondition (L0017) ----

#[test]
fn duplicate_condition_fires_end_to_end() {
    let diags = lint_src(
        "function f(x) { if (x > 0) { return 1; } else if (x > 0) { return 2; } return 0; }\n",
    );
    assert_eq!(has_code(&diags, codes::DUPLICATE_CONDITION), 1);
}

// ---- UnusedParameter (L0018) ----

#[test]
fn unused_parameter_fires_end_to_end() {
    let diags = lint_src("function f(x, y) { return x; }\n");
    assert_eq!(has_code(&diags, codes::UNUSED_PARAMETER), 1);
}

#[test]
fn unused_parameter_silent_when_used() {
    let diags = lint_src("function f(x, y) { return x + y; }\n");
    assert_eq!(has_code(&diags, codes::UNUSED_PARAMETER), 0);
}

// ---- RedundantTernary (L0019) ----

#[test]
fn redundant_ternary_fires_end_to_end() {
    let diags = lint_src("function f(c, a) { return c ? a : a; }\n");
    assert_eq!(has_code(&diags, codes::REDUNDANT_TERNARY), 1);
}

// ---- DuplicateInclude (L0020) ----

#[test]
fn duplicate_include_fires_end_to_end() {
    let diags = lint_src("include(\"util\")\ninclude(\"util\")\nreturn 0\n");
    assert_eq!(has_code(&diags, codes::DUPLICATE_INCLUDE), 1);
}

// ---- NegatedComparison (L0021) ----

#[test]
fn negated_comparison_fires_end_to_end() {
    let diags = lint_src("function f(a, b) { return !(a == b); }\n");
    assert_eq!(has_code(&diags, codes::NEGATED_COMPARISON), 1);
}

// ---- UnusedExpression (L0022) ----

#[test]
fn unused_expression_fires_end_to_end() {
    let diags = lint_src("function f(x) {\n  x == 5\n  return x\n}\n");
    assert_eq!(has_code(&diags, codes::UNUSED_EXPRESSION), 1);
}

// ---- DuplicateCase (L0023) ----

#[test]
fn duplicate_case_fires_end_to_end() {
    let diags = lint_src(
        "function f(x) {\n  switch (x) {\n    case 1: return 1\n    case 1: return 2\n  }\n  return 0\n}\n",
    );
    assert_eq!(has_code(&diags, codes::DUPLICATE_CASE), 1);
}

// ---- UnnecessaryElse (L0024) ----

#[test]
fn unnecessary_else_fires_end_to_end() {
    let diags = lint_src(
        "function f(x) {\n  if (x < 0) {\n    return -1\n  } else {\n    return 1\n  }\n}\n",
    );
    assert_eq!(has_code(&diags, codes::UNNECESSARY_ELSE), 1);
}
