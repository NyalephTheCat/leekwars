//! Tests for `format_range` — partial-document formatting used by
//! the LSP's `textDocument/rangeFormatting`.

use leek_fmt::{FormatOptions, format_range};
use leek_span::SourceId;
use leek_syntax::Version;

fn opts() -> FormatOptions {
    FormatOptions::default()
}

fn fmt_range(src: &str, start: u32, end: u32) -> Option<(std::ops::Range<u32>, String)> {
    let parsed = leek_parser::parse(src, SourceId::new(1).unwrap(), Version::V4);
    format_range(&parsed.green, &opts(), start..end)
}

/// Locate the byte offsets of `needle` in `haystack`. Panics if not
/// found — keeps tests terse.
fn span_of(haystack: &str, needle: &str) -> (u32, u32) {
    let start = haystack.find(needle).expect("substring not found");
    let s = u32::try_from(start).unwrap();
    let e = u32::try_from(start + needle.len()).unwrap();
    (s, e)
}

#[test]
fn formats_a_single_statement() {
    let src = "function f() {\n    var x   =1   ;\n    return x;\n}\n";
    let (s, e) = span_of(src, "var x   =1   ;");
    let (range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(range, s..e);
    assert_eq!(out, "var x = 1;");
}

#[test]
fn formats_a_function_with_proper_inner_indent() {
    let src = "function    f(  ) {\nreturn 1;\n}\n";
    let (s, e) = span_of(src, "function    f(  ) {\nreturn 1;\n}");
    let (range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(range, s..e);
    // Should produce well-indented function body.
    assert!(out.contains("function f() {"));
    assert!(out.contains("    return 1;"));
    assert!(out.ends_with('}'));
}

#[test]
fn re_indents_nested_block_to_match_source_column() {
    // Block sits at column 4; reformatted output's continuation
    // lines must also start at column 4 (or deeper for nested
    // content).
    let src = "function f() {\n    if (x) {\n        body  ;\n    }\n}\n";
    let (s, e) = span_of(src, "if (x) {\n        body  ;\n    }");
    let (_range, out) = fmt_range(src, s, e).expect("found");

    // The `}` should land at column 4 (the original column of `if`).
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(*lines.last().unwrap(), "    }", "got: {out:?}");
    // The body should land at column 8.
    assert!(
        lines.iter().any(|l| l.starts_with("        body")),
        "body re-indented: {out:?}"
    );
}

#[test]
fn returns_none_for_range_past_eof() {
    let src = "var x = 1;\n";
    let out = fmt_range(src, 0, 999);
    assert!(out.is_none());
}

#[test]
fn returns_none_if_range_covers_whole_source_file() {
    // SourceFile-level "range formatting" is degenerate; callers
    // should use `format` instead. We return None so the caller can
    // detect and fall back.
    let src = "var x = 1;\n";
    let out = fmt_range(src, 0, u32::try_from(src.len()).unwrap());
    assert!(out.is_none());
}

#[test]
fn idempotent_on_already_formatted_range() {
    // Formatting an already-formatted range should return the same
    // text the caller would replace.
    let src = "function f() {\n    var x = 1;\n    return x;\n}\n";
    let (s, e) = span_of(src, "var x = 1;");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "var x = 1;");
}
