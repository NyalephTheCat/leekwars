//! The formatter must never invent syntax.
//!
//! Every case here is a minimal repro from a filed defect where the
//! formatter either manufactured a token the source did not contain
//! (#415, #417) or failed to reach a fixed point (#417, #419, #420).
//! The shared rule under test: on input the parser did not fully
//! understand, emit it unchanged rather than guess.
//!
//! Each test asserts three things, because each catches a different
//! kind of regression:
//!
//! 1. `format_source_checked` succeeds — the equivalence safety net
//!    agrees no token or comment was added, dropped or changed.
//! 2. The specific invented text is absent from the output.
//! 3. Formatting the output again is a byte-for-byte no-op.

use leek_fmt::{FormatOptions, format_source, format_source_checked};
use leek_span::SourceId;
use leek_syntax::Version;

fn opts() -> FormatOptions {
    FormatOptions::default()
}

fn fmt_at(src: &str, version: Version) -> String {
    format_source(src, SourceId::new(1).unwrap(), version, &opts())
}

/// Format `src`, assert the equivalence net accepts the result, and
/// assert a second pass changes nothing. Returns the output.
fn fmt_safe_at(src: &str, version: Version) -> String {
    let out = format_source_checked(src, SourceId::new(1).unwrap(), version, &opts())
        .unwrap_or_else(|e| panic!("formatter changed the program for {src:?}: {e}"));
    let again = fmt_at(&out, version);
    assert_eq!(out, again, "formatting is not idempotent for {src:?}");
    out
}

fn fmt_safe(src: &str) -> String {
    fmt_safe_at(src, Version::V4)
}

// ---- #415: ranges and slices shredded, with a stray `]` ----

#[test]
fn slice_below_v4_keeps_its_brackets() {
    // Slice syntax is v4-only. The v1-v3 recovery used to bail into
    // the index production, stop at the `:`, and let `format_index`
    // manufacture the `]` — leaving `: 5 ]` as three top-level
    // statements.
    let src = "return [1, 2, 3, 4, 5, 6, 7, 8][0:5]\n";
    for version in [Version::V1, Version::V2, Version::V3] {
        let out = fmt_safe_at(src, version);
        assert!(
            out.contains("[0:5]"),
            "slice was shredded at {version:?}: {out:?}"
        );
        assert_eq!(
            out.matches(']').count(),
            src.matches(']').count(),
            "bracket count changed at {version:?}: {out:?}"
        );
    }
}

#[test]
fn slice_at_v4_is_unchanged() {
    let out = fmt_safe("return a[1:3];\n");
    assert!(out.contains("a[1:3]"), "{out:?}");
}

#[test]
fn statement_initial_interval_is_not_eaten_as_a_subscript() {
    // A `[` that holds a top-level `..` is an interval literal, never
    // a subscript — there is no subscript syntax containing `..`.
    // Before the guard, the interval after an expression-shaped line
    // was consumed as `previous[2]`, the parse stopped at the `..`,
    // and a `]` was invented.
    let src = "var x = [1]\n[2..10000].filter(i -> i)\n";
    let out = fmt_safe(src);
    assert!(out.contains("[2..10000]"), "interval shredded: {out:?}");
}

#[test]
fn interval_literals_still_parse_and_format() {
    for src in [
        "var r = [1..10];\n",
        "var r = [1..10[;\n",
        "var r = ]1..10];\n",
        "var r = ]1..10[;\n",
        "var r = [..10];\n",
        "var r = [1..];\n",
    ] {
        fmt_safe(src);
    }
}

#[test]
fn subscript_without_a_range_is_still_a_subscript() {
    let out = fmt_safe("var t = [[1, 2], [3, 4]]\nreturn t[0][1]\n");
    assert!(out.contains("t[0][1]"), "{out:?}");
}

#[test]
fn an_unclosed_subscript_invents_nothing() {
    let out = fmt_safe("var y = a[0\n");
    assert!(!out.contains(']'), "invented a closing bracket: {out:?}");
}

// ---- #417: tokens invented past the end of the file ----

#[test]
fn nested_angle_sets_do_not_grow_closers() {
    // The lexer fuses `>>` into one token, so neither set literal gets
    // a `>` child; both formatters used to print one from a constant,
    // four in total across the expression.
    let src = "return <'a', <1, 2>> == <'a', <1, 2>>\n";
    let out = fmt_safe(src);
    assert_eq!(
        out.matches('>').count(),
        src.matches('>').count(),
        "closer count changed: {out:?}"
    );
}

#[test]
fn a_ternary_without_a_colon_does_not_get_one() {
    let src = "[][0] !? 6\n";
    let out = fmt_safe(src);
    assert!(!out.contains(':'), "invented a ternary colon: {out:?}");
}

#[test]
fn a_complete_ternary_is_still_formatted() {
    let out = fmt_safe("var r = true ? 'a' : 'b'\n");
    assert!(out.contains("true ? 'a' : 'b'"), "{out:?}");
}

#[test]
fn an_unterminated_string_does_not_gain_a_newline() {
    // The literal runs to EOF, so the file's own trailing newline is
    // *inside* the token; the unconditional `hardline()` at the end of
    // the document appended a second one, and one more on every pass.
    for src in [
        "'\n",
        "\"\n",
        "\"unclosed\n",
        "'unclosed\n",
        // No trailing newline at all: the document terminator must not
        // be written into the literal either.
        "return 'unclosed",
        "return \"unclosed",
        "'",
    ] {
        let out = fmt_safe(src);
        assert_eq!(out, src, "unterminated string rewritten: {out:?}");
    }
}

#[test]
fn an_unterminated_comment_at_eof_does_not_gain_a_newline() {
    for src in ["var a = 1\n/*", "/* eof", "var a = 1\n/* eof"] {
        let out = fmt_safe(src);
        assert!(
            out.ends_with(src.trim_start_matches("var a = 1\n")),
            "terminator written into the comment: {out:?}"
        );
    }
}

#[test]
fn a_closed_string_or_comment_at_eof_still_gets_a_terminator() {
    for src in ["var s = 'ok';", "var x = 1; /* done */"] {
        let out = fmt_safe(src);
        assert!(out.ends_with('\n'), "missing terminator: {out:?}");
    }
}

// ---- #419: an unterminated block comment grows a line per pass ----

#[test]
fn an_unterminated_block_comment_does_not_grow() {
    let src = "var a = 1\n/*\nvar b = 2\n";
    let out = fmt_safe(src);
    assert_eq!(out, src, "unterminated comment rewritten: {out:?}");
}

#[test]
fn an_unterminated_block_comment_keeps_its_blank_lines() {
    // Guard against "fix" by trimming: those newlines are characters
    // inside the comment token, not document padding.
    let src = "var a = 1\n/*\n\n\n";
    let out = fmt_safe(src);
    assert_eq!(out, src, "comment body was trimmed: {out:?}");
}

#[test]
fn an_unterminated_block_comment_inside_a_block_does_not_grow() {
    // The block has no `}` either, so this exercises the missing-closer
    // guard and the end-of-file guard together.
    let src = "function f() { /* eof\n";
    let out = fmt_safe(src);
    assert!(!out.contains('}'), "invented a closing brace: {out:?}");
    assert!(!out.contains(" *\n"), "comment was re-laid-out: {out:?}");
}

#[test]
fn a_terminated_block_comment_is_still_reflowed() {
    let out = fmt_safe("/*\n * docs\n */\nvar x = 1;\n");
    assert!(out.contains("/*\n * docs\n */"), "{out:?}");
}

#[test]
fn a_well_formed_file_still_ends_in_exactly_one_newline() {
    for src in [
        "var x = 1;\n",
        "var x = 1;",
        "function f() {\n    return 1;\n}\n",
    ] {
        let out = fmt_safe(src);
        assert!(out.ends_with('\n'), "missing terminator: {out:?}");
        assert!(!out.ends_with("\n\n"), "doubled terminator: {out:?}");
    }
}

#[test]
fn an_off_region_running_to_eof_still_ends_in_one_newline() {
    let src = "// fmt: off\nfunction    weird(  x){return x;}\n";
    let out = fmt_safe(src);
    assert!(out.ends_with("}\n"), "terminator lost or doubled: {out:?}");
}

#[test]
fn crlf_still_terminates_the_last_line() {
    let mut o = opts();
    o.line_ending = leek_fmt::LineEnding::Crlf;
    let out = format_source("var x = 1;\n", SourceId::new(1).unwrap(), Version::V4, &o);
    assert!(out.ends_with("\r\n"), "last line not CRLF: {out:?}");
    assert!(
        !out.replace("\r\n", "").contains('\n'),
        "stray bare LF: {out:?}"
    );
}

// ---- #420: annotations gain a space per pass ----

#[test]
fn repeated_annotations_reach_a_fixed_point() {
    // Only the first annotation is preceded by a trivia flush, so every
    // later one carries the separating space inside its own node. The
    // parent walker adds a separator too, so re-emitting the node text
    // raw grew the gap by one space on every pass.
    for src in [
        "@pure @unused function helper() { return 1; }\n",
        "@unused @deprecated var x = 5;\n",
        "@todo @unused function stub() { return 0; }\n",
    ] {
        let out = fmt_safe(src);
        assert!(!out.contains("  @"), "annotation separator grew: {out:?}");
    }
}

#[test]
fn an_already_widened_annotation_list_is_normalized() {
    let out = fmt_safe("@pure   @unused function helper() { return 1; }\n");
    assert!(out.contains("@pure @unused function helper()"), "{out:?}");
}

#[test]
fn an_annotation_with_arguments_keeps_them() {
    let out = fmt_safe("@deprecated('use g') function f() { return 1; }\n");
    assert!(out.contains("@deprecated('use g')"), "{out:?}");
}

#[test]
fn a_line_broken_annotation_list_is_joined() {
    let out = fmt_safe("@pure\n@unused\nfunction helper() { return 1; }\n");
    assert!(
        out.starts_with("@pure @unused function helper()"),
        "{out:?}"
    );
}
