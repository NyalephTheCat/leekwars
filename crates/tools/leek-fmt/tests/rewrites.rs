//! Tests for the opt-in rewriting options: `control_braces`,
//! `remove_redundant_parens`, `semicolons`, `collapse_else_if`.
//!
//! Every rewrite is also checked for idempotence — formatting the
//! output again must be a no-op, otherwise `miku fmt` would never
//! converge.

use leek_fmt::{ControlBraces, FormatOptions, Semicolons, format_source};
use leek_span::SourceId;
use leek_syntax::Version;

fn fmt_with(opts: &FormatOptions, src: &str) -> String {
    format_source(src, SourceId::new(1).unwrap(), Version::V4, opts)
}

fn idempotent(opts: &FormatOptions, out: &str) {
    assert_eq!(out, fmt_with(opts, out), "rewrite must be idempotent");
}

fn opts() -> FormatOptions {
    FormatOptions::default()
}

// ---- control_braces = always ----

#[test]
fn control_braces_always_wraps_if_body() {
    let mut o = opts();
    o.control_braces = ControlBraces::Always;
    let out = fmt_with(&o, "if (x) return 1;\n");
    assert!(
        out.contains("if (x) {\n    return 1;\n}"),
        "expected braces added, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn control_braces_always_wraps_loop_bodies() {
    let mut o = opts();
    o.control_braces = ControlBraces::Always;
    let out = fmt_with(&o, "while (x) f();\nfor (var i = 0; i < 3; i++) g();\n");
    assert!(
        out.contains("while (x) {\n    f();\n}"),
        "while body braced, got: {out:?}"
    );
    assert!(
        out.contains("for (var i = 0; i < 3; i++) {\n    g();\n}"),
        "for body braced, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn control_braces_always_keeps_else_if_chain() {
    let mut o = opts();
    o.control_braces = ControlBraces::Always;
    let out = fmt_with(&o, "if (a) f(); else if (b) g(); else h();\n");
    assert!(
        out.contains("} else if (b) {"),
        "`else if` must not become `else {{ if`, got: {out:?}"
    );
    idempotent(&o, &out);
}

// ---- control_braces = never ----

#[test]
fn control_braces_never_unwraps_lone_simple_stmt() {
    let mut o = opts();
    o.control_braces = ControlBraces::Never;
    let out = fmt_with(&o, "if (x) { return 1; }\nwhile (y) { f(); }\n");
    assert!(
        out.contains("if (x) return 1;"),
        "if braces dropped, got: {out:?}"
    );
    assert!(
        out.contains("while (y) f();"),
        "while braces dropped, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn control_braces_never_keeps_multi_statement_blocks() {
    let mut o = opts();
    o.control_braces = ControlBraces::Never;
    let out = fmt_with(&o, "if (x) { f(); g(); }\n");
    assert!(
        out.contains("if (x) {"),
        "multi-stmt block must keep braces, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn control_braces_never_keeps_block_with_comment() {
    let mut o = opts();
    o.control_braces = ControlBraces::Never;
    let out = fmt_with(&o, "if (x) {\n    // why\n    f();\n}\n");
    assert!(
        out.contains("if (x) {"),
        "commented block must keep braces, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn control_braces_never_keeps_nested_if_dangling_else_safe() {
    let mut o = opts();
    o.control_braces = ControlBraces::Never;
    // Unwrapping `{ if (b) f(); }` would re-bind the `else` to the
    // inner `if` — the braces must survive.
    let out = fmt_with(&o, "if (a) { if (b) f(); } else g();\n");
    assert!(
        out.contains("if (a) {"),
        "nested-if block must keep braces, got: {out:?}"
    );
    idempotent(&o, &out);
}

// ---- remove_redundant_parens ----

#[test]
fn redundant_parens_removed_around_condition() {
    let mut o = opts();
    o.remove_redundant_parens = true;
    let out = fmt_with(&o, "if ((a && b)) { f(); }\nwhile (((x))) { g(); }\n");
    assert!(
        out.contains("if (a && b)"),
        "doubled condition parens removed, got: {out:?}"
    );
    assert!(
        out.contains("while (x)"),
        "all condition paren layers removed, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn redundant_parens_removed_around_return_operand() {
    let mut o = opts();
    o.remove_redundant_parens = true;
    let out = fmt_with(&o, "function f() { return (a + b); }\n");
    assert!(
        out.contains("return a + b;"),
        "return parens removed, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn redundant_parens_removed_around_primaries() {
    let mut o = opts();
    o.remove_redundant_parens = true;
    let out = fmt_with(&o, "var y = ((f(a)));\nvar z = (x) + 1;\n");
    assert!(out.contains("var y = f(a);"), "got: {out:?}");
    assert!(out.contains("var z = x + 1;"), "got: {out:?}");
    idempotent(&o, &out);
}

#[test]
fn meaningful_parens_are_kept() {
    let mut o = opts();
    o.remove_redundant_parens = true;
    let out = fmt_with(&o, "var r = (a + b) * c;\n");
    assert!(
        out.contains("(a + b) * c"),
        "precedence parens must survive, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn parens_preserved_by_default() {
    let out = fmt_with(&opts(), "var y = ((x));\n");
    assert!(
        out.contains("((x))"),
        "default preserves parens, got: {out:?}"
    );
}

// ---- semicolons = always ----

#[test]
fn semicolons_always_appends_missing_terminators() {
    let mut o = opts();
    o.semicolons = Semicolons::Always;
    let out = fmt_with(&o, "var x = 1\nf()\nfunction g() { return 2 }\n");
    assert!(
        out.contains("var x = 1;"),
        "var decl gets `;`, got: {out:?}"
    );
    assert!(out.contains("f();"), "expr stmt gets `;`, got: {out:?}");
    assert!(out.contains("return 2;"), "return gets `;`, got: {out:?}");
    idempotent(&o, &out);
}

#[test]
fn semicolons_always_does_not_double_existing() {
    let mut o = opts();
    o.semicolons = Semicolons::Always;
    let out = fmt_with(&o, "var x = 1;\n");
    assert!(!out.contains(";;"), "no doubled `;`, got: {out:?}");
}

#[test]
fn semicolons_always_leaves_for_headers_alone() {
    let mut o = opts();
    o.semicolons = Semicolons::Always;
    let out = fmt_with(&o, "for (var i = 0; i < 3; i++) { f() }\n");
    assert!(
        out.contains("for (var i = 0; i < 3; i++)"),
        "for header untouched, got: {out:?}"
    );
    assert!(
        out.contains("f();"),
        "loop body stmt gets `;`, got: {out:?}"
    );
    idempotent(&o, &out);
}

// ---- collapse_else_if ----

#[test]
fn collapse_else_if_flattens_block() {
    let mut o = opts();
    o.collapse_else_if = true;
    let out = fmt_with(
        &o,
        "if (a) {\n    f();\n} else {\n    if (b) {\n        g();\n    }\n}\n",
    );
    assert!(
        out.contains("} else if (b) {"),
        "expected collapsed `else if`, got: {out:?}"
    );
    idempotent(&o, &out);
}

#[test]
fn collapse_else_if_keeps_blocks_with_extra_statements() {
    let mut o = opts();
    o.collapse_else_if = true;
    let out = fmt_with(&o, "if (a) { f(); } else { if (b) { g(); } h(); }\n");
    assert!(
        !out.contains("else if"),
        "block with trailing stmt must not collapse, got: {out:?}"
    );
    idempotent(&o, &out);
}
