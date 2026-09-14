//! Regression tests for the token-separation and dropped-token bugs:
//! #412 (adjacent tokens glue and re-lex as one), #413 (`not in` loses
//! its `in`), #414 (`static` dropped from an unclassifiable class
//! member).
//!
//! Each test is the minimal repro from its issue. They guard the
//! fixes independently of the corpus ratchet.

use leek_fmt::{FormatOptions, format_source};
use leek_span::SourceId;
use leek_syntax::Version;

fn fmt(src: &str) -> String {
    format_source(
        src,
        SourceId::new(1).unwrap(),
        Version::V4,
        &FormatOptions::default(),
    )
}

// ---- #412: adjacent tokens must not glue ----

#[test]
fn not_keyword_keeps_a_space_before_its_operand() {
    let out = fmt("return not true;\n");
    assert!(out.contains("not true"), "got {out:?}");
}

#[test]
fn leading_return_type_on_a_class_method_keeps_its_space() {
    let out = fmt("class C { public static Array execute(Array t) { return t; } }\n");
    assert!(out.contains("static Array"), "got {out:?}");
    assert!(!out.contains("staticArray"), "got {out:?}");
}

#[test]
fn paren_less_if_keeps_a_space_before_its_condition() {
    let out = fmt("for (var i = 0; i < 3; i++) { if i % 2 { continue; } }\n");
    assert!(!out.contains("ifi"), "got {out:?}");
}

#[test]
fn function_type_arrow_does_not_munch_with_the_angle_bracket() {
    let out = fmt("Function< => integer> g = f;\n");
    assert!(!out.contains("<=>"), "got {out:?}");
}

#[test]
fn comma_less_object_literal_keeps_its_entries_apart() {
    let out = fmt("var o = {a: 12 b: 5};\n");
    assert!(!out.contains("12b"), "got {out:?}");
}

// ---- #413: `not in` keeps its `in` ----

#[test]
fn not_in_keeps_both_operator_tokens() {
    let out = fmt("return 3 not in [1, 2];\n");
    assert!(out.contains("not in"), "got {out:?}");
}

// ---- #414: `static` survives on an unclassifiable class member ----

#[test]
fn stray_static_modifier_is_not_dropped_from_a_class_body() {
    let out = fmt("class A { static for (var i = 0; i < 1; i++) {} }\n");
    assert!(out.contains("static"), "got {out:?}");
}

// ---- #140: an unterminated `/*` must not make the separator rule
// give up on a `/` next to a block comment ----

/// `needs_separator` bails out when either fragment is malformed on
/// its own, because a space would not repair it (#417). The fragments
/// it compares are maximal operator-character runs, so a block
/// comment's leading fragment is the bare `/*` — an unterminated
/// comment however well-formed the real comment is. If that counted as
/// malformed, a `/` printed next to a comment would get no space and
/// the two would re-lex as `//`, turning the block comment into a line
/// comment and swallowing the rest of the line.
#[test]
fn slash_before_a_block_comment_still_gets_its_space() {
    let out = fmt("var x = 6 / /* two */ 3;\n");
    assert!(!out.contains("//*"), "got {out:?}");
    assert!(out.contains("/* two */"), "got {out:?}");
}

/// The file-level case from #419: an unterminated comment is still
/// left exactly as written — it is valid LeekScript (#351), and the
/// formatter must not "fix" it by closing it.
#[test]
fn an_unterminated_block_comment_is_left_alone() {
    let src = "var x = 1;\n/* trailing";
    assert_eq!(fmt(src), src);
}
