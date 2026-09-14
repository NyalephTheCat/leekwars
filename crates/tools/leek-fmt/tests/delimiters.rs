//! Delimiter tokens belong to the parse, not to the printer.
//!
//! Two bugs of the same class: a construct formatter re-derived its
//! `(`/`)` or `<`/`>` from an assumed node shape instead of from the
//! tokens the parser actually consumed.
//!
//! - #416 `format_lambda` *dropped* the parens of `(x -> x + 1)`, which
//!   wrap the whole lambda, and let `format_param_list` re-add a pair in
//!   the wrong place. In callee position that re-parses the call into
//!   the lambda body: `(x -> x + 1)(2)` became `(x) -> x + 1(2)`.
//! - #418 `bracketed_list_with` *invented* a closing `>` for an angle
//!   set literal the parser never closed, so error-recovered input grew
//!   a token that is not in the source.
//!
//! Both are invisible to a token-only diff for #416 (identical stream,
//! different tree), which is why every case here goes through
//! `format_source_checked` as well as asserting the exact text.

use leek_fmt::{FormatOptions, format_source, format_source_checked};
use leek_span::SourceId;
use leek_syntax::Version;

fn src_id() -> SourceId {
    SourceId::new(1).unwrap()
}

fn fmt_with(opts: &FormatOptions, src: &str) -> String {
    format_source(src, src_id(), Version::V4, opts)
}

fn fmt(src: &str) -> String {
    fmt_with(&FormatOptions::default(), src)
}

/// Format, assert the safety net accepts the result, and assert a
/// second pass is a no-op.
fn fmt_checked(src: &str) -> String {
    let out = fmt(src);
    if let Err(e) = format_source_checked(src, src_id(), Version::V4, &FormatOptions::default()) {
        panic!("safety net rejected formatting of {src:?}: {e:?}");
    }
    assert_eq!(out, fmt(&out), "formatting {src:?} is not idempotent");
    out
}

// ---- #416: the parens of `(params -> body)` wrap the whole lambda ----

#[test]
fn inner_arrow_lambda_keeps_its_wrapping_parens() {
    // The exact repro from the issue. Peeling the parens re-parses the
    // call `(1.01)` as part of the lambda body.
    assert_eq!(
        fmt_checked("return (x -> x + 12.12)(1.01);\n"),
        "return (x -> x + 12.12)(1.01);\n"
    );
}

#[test]
fn inner_arrow_lambda_forms_round_trip() {
    for src in [
        "return (x -> x)(12);\n",
        "return (x, y -> x + y)(12, 5);\n",
        "return ( -> 12)();\n",
        "var f = (x -> x * 2);\n",
    ] {
        assert_eq!(fmt_checked(src), src, "input {src:?} must be preserved");
    }

    // A block body is reflowed, but the wrapping parens stay put.
    assert_eq!(
        fmt_checked("var g = (a, b -> { return a + b; });\n"),
        "var g = (a, b -> {\n    return a + b;\n});\n"
    );
}

#[test]
fn param_list_lambda_still_gets_synthetic_parens() {
    // The `(a, b) -> body` form owns delimiters that close *before* the
    // arrow: those are the parameter delimiters and `format_param_list`
    // is still the one to emit them. Double-wrapping (`((a, b))`) here
    // is the regression the fix must not cause.
    let out = fmt_checked("var g = (a, b) -> a + b;\n");
    assert_eq!(out, "var g = (a, b) -> a + b;\n");
    assert!(!out.contains("(("), "params must not double-wrap: {out:?}");

    // A bare parameter gains parens, as before.
    assert_eq!(fmt_checked("var f = x -> x;\n"), "var f = (x) -> x;\n");

    // And the `function (…) {…}` form is untouched.
    let anon = fmt_checked("var h = function (a) { return a; };\n");
    assert!(anon.contains("function(a)"), "got: {anon:?}");
}

#[test]
fn inner_arrow_lambda_survives_every_option_set() {
    let tight = FormatOptions {
        space_around_arrow: false,
        ..FormatOptions::default()
    };
    let out = fmt_with(&tight, "return (x -> x + 1)(2);\n");
    assert_eq!(out, "return (x->x + 1)(2);\n");
    assert_eq!(out, fmt_with(&tight, &out), "not idempotent: {out:?}");

    let peeling = FormatOptions {
        remove_redundant_parens: true,
        ..FormatOptions::default()
    };
    let out = fmt_with(&peeling, "return (x -> x + 1)(2);\n");
    assert_eq!(out, "return (x -> x + 1)(2);\n");
    assert_eq!(out, fmt_with(&peeling, &out), "not idempotent: {out:?}");
}

// ---- #418: never print a delimiter the parser did not consume ----

#[test]
fn unclosed_angle_set_does_not_gain_a_closer() {
    // `|x|` is not a length operator in this grammar (nor upstream), so
    // `while |n| < 1000 { … }` mis-parses: the `<` opens an angle set
    // literal that is never closed. The formatter used to print that set
    // with a synthesized `>`, inventing a token — the euler/pe025.leek
    // failure. Formatting broken input must stay token-faithful.
    let src = "while |n1.string()| < 1000 {\n    x();\n}\n";
    let out = fmt(src);
    assert!(
        !out.contains('>'),
        "formatter invented a closing `>`: {out:?}"
    );
    if let Err(e) = format_source_checked(src, src_id(), Version::V4, &FormatOptions::default()) {
        panic!("safety net rejected formatting of broken input: {e:?}");
    }
    assert_eq!(out, fmt(&out), "not idempotent: {out:?}");
}

#[test]
fn unclosed_array_literal_does_not_gain_a_closer() {
    let src = "var a = [1, 2\n";
    let out = fmt(src);
    assert!(
        !out.contains(']'),
        "formatter invented a closing `]`: {out:?}"
    );
    assert_eq!(out, fmt(&out), "not idempotent: {out:?}");
}

#[test]
fn well_formed_angle_set_still_formats() {
    // The guard must not fire on good input: angle sets keep their own
    // delimiters and still get normalized spacing.
    assert_eq!(fmt_checked("var s = <1,2,3>;\n"), "var s = <1, 2, 3>;\n");
    assert_eq!(fmt_checked("var s = <>;\n"), "var s = <>;\n");
}

#[test]
fn well_formed_collections_still_format() {
    assert_eq!(fmt_checked("var a = [1,2,3];\n"), "var a = [1, 2, 3];\n");
    assert_eq!(
        fmt_checked("var m = [1:2,3:4];\n"),
        "var m = [1: 2, 3: 4];\n"
    );
    assert_eq!(
        fmt_checked("var o = {a:1,b:2};\n"),
        "var o = {a: 1, b: 2};\n"
    );
}
