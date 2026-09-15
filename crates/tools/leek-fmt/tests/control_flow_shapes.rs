//! Control-flow shapes the typed-AST formatters have to get right.
//!
//! `if`/`while`/`do … while` are formatted off
//! `leek_parser::ast::{IfStmt, WhileStmt, DoWhileStmt}` (#196). A
//! positional accessor and the token stream can disagree in exactly
//! two places, and both are covered here:
//!
//! - **The empty statement.** `while (x);` has no body *node* — the
//!   parser bumps the bare `;` into the loop — so a `None` from a
//!   body accessor is not proof the body is missing.
//! - **Error recovery.** `p.expect` consumes nothing on a mismatch,
//!   so a missing `(`, `)` or body simply is not in the tree.
//!   Printing one anyway would manufacture a token the source never
//!   had (#415, #417), so an unrecognised shape round-trips instead.
//!
//! `do … while` carries both plus its own optional terminator, which
//! is why most of the cases below are do-whiles.

use leek_fmt::{FormatOptions, Semicolons, format_source, format_source_checked};
use leek_span::SourceId;
use leek_syntax::Version;

fn fmt_with(src: &str, opts: &FormatOptions) -> String {
    let out = format_source_checked(src, SourceId::new(1).unwrap(), Version::V4, opts)
        .unwrap_or_else(|e| panic!("formatter changed the program for {src:?}: {e}"));
    let again = format_source(&out, SourceId::new(1).unwrap(), Version::V4, opts);
    assert_eq!(out, again, "formatting is not idempotent for {src:?}");
    out
}

fn fmt(src: &str) -> String {
    fmt_with(src, &FormatOptions::default())
}

fn fmt_semicolons_always(src: &str) -> String {
    let opts = FormatOptions {
        semicolons: Semicolons::Always,
        ..FormatOptions::default()
    };
    fmt_with(src, &opts)
}

// ---- (a) a comment between the `}` and the `while` ----

#[test]
fn a_block_comment_between_the_body_and_while_survives() {
    let out = fmt("do { n = n + 1; } /* wrap */ while (n < 5);\n");
    assert!(out.contains("/* wrap */"), "comment dropped: {out:?}");
    assert_eq!(out.matches("while").count(), 1, "{out:?}");
}

#[test]
fn a_line_comment_between_the_body_and_while_survives() {
    let out = fmt("do { n = n + 1; } // wrap\nwhile (n < 5);\n");
    assert!(out.contains("// wrap"), "comment dropped: {out:?}");
    assert!(out.contains("while (n < 5)"), "{out:?}");
}

// ---- (b) a missing terminating semicolon ----

#[test]
fn a_do_while_without_a_terminator_does_not_gain_one() {
    // `semicolons = preserve` (the default): the source wrote no `;`,
    // so neither does the output.
    let out = fmt("do { n = n + 1; } while (n < 5)\n");
    assert_eq!(out, "do {\n    n = n + 1;\n} while (n < 5)\n", "{out:?}");
}

#[test]
fn a_do_while_without_a_terminator_gains_exactly_one_when_asked() {
    let out = fmt_semicolons_always("do { n = n + 1; } while (n < 5)\n");
    assert_eq!(out, "do {\n    n = n + 1;\n} while (n < 5);\n", "{out:?}");
}

#[test]
fn an_empty_body_semicolon_is_not_the_terminator() {
    // `do ; while (x)` holds exactly one `;`, and it is the *body* —
    // the empty statement. Counting semicolons rather than placing
    // them left the statement's own terminator out under
    // `semicolons = always`.
    let out = fmt_semicolons_always("do ; while (n < 5)\n");
    assert_eq!(out, "do; while (n < 5);\n", "{out:?}");
}

#[test]
fn a_do_while_terminator_is_not_duplicated() {
    let out = fmt_semicolons_always("do { n = n + 1; } while (n < 5);\n");
    assert!(!out.contains(";;"), "terminator doubled: {out:?}");
}

// ---- (c) a missing closing paren ----

#[test]
fn an_unclosed_do_while_condition_keeps_its_shape() {
    let src = "do { n = n + 1; } while (n < 5\n";
    let out = fmt(src);
    assert_eq!(
        out.matches(')').count(),
        src.matches(')').count(),
        "a `)` was manufactured: {out:?}"
    );
    assert!(!out.contains("5;"), "a `;` was manufactured: {out:?}");
}

#[test]
fn an_unclosed_do_while_condition_gains_nothing_under_semicolons_always() {
    let src = "do { n = n + 1; } while (n < 5\n";
    let out = fmt_semicolons_always(src);
    assert_eq!(
        out.matches(')').count(),
        src.matches(')').count(),
        "a `)` was manufactured: {out:?}"
    );
    assert!(
        !out.trim_end().ends_with(';'),
        "a terminator was manufactured past an unclosed condition: {out:?}"
    );
}

#[test]
fn a_do_while_missing_its_opening_paren_keeps_its_shape() {
    let src = "do { n = n + 1; } while n < 5);\n";
    let out = fmt(src);
    assert_eq!(
        out.matches('(').count(),
        src.matches('(').count(),
        "a `(` was manufactured: {out:?}"
    );
}

#[test]
fn a_do_while_missing_its_while_keeps_its_shape() {
    let src = "do { n = n + 1; }\n";
    let out = fmt(src);
    assert!(
        !out.contains("while"),
        "a `while` was manufactured: {out:?}"
    );
}

// ---- empty statements stay formatted ----

#[test]
fn empty_control_bodies_are_still_formatted() {
    // The `;` is the whole body and has no statement node, but the
    // header around it is still the formatter's to lay out.
    assert_eq!(fmt("while(x)  ;\n"), "while (x);\n");
    assert_eq!(fmt("do  ;  while(x)  ;\n"), "do; while (x);\n");
    assert_eq!(fmt("if(x)  ;  else  ;\n"), "if (x); else;\n");
    assert_eq!(
        fmt("if(x)  ;  else  { b(); }\n"),
        "if (x); else {\n    b();\n}\n"
    );
}

// ---- if/while recovery round-trips rather than inventing parens ----

#[test]
fn a_paren_less_if_is_left_alone() {
    let src = "for (var i = 0; i < 3; i++) { if i % 2 { continue; } }\n";
    let out = fmt(src);
    assert_eq!(
        out.matches('(').count(),
        src.matches('(').count(),
        "a `(` was manufactured: {out:?}"
    );
    assert!(!out.contains("ifi"), "{out:?}");
}

#[test]
fn an_unclosed_while_condition_keeps_its_shape() {
    let src = "while (x { a(); }\n";
    let out = fmt(src);
    assert_eq!(
        out.matches(')').count(),
        src.matches(')').count(),
        "a `)` was manufactured: {out:?}"
    );
}

#[test]
fn a_dangling_else_keeps_its_shape() {
    let out = fmt("if (x) { a(); } else\n");
    assert!(out.contains("else"), "{out:?}");
    assert_eq!(out.matches("else").count(), 1, "{out:?}");
}
