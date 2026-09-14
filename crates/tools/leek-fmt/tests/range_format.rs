//! Tests for `format_range` — partial-document formatting used by
//! the LSP's `textDocument/rangeFormatting`.

use leek_fmt::{FormatOptions, format_range, format_source};
use leek_span::SourceId;
use leek_syntax::Version;

fn opts() -> FormatOptions {
    FormatOptions::default()
}

fn fmt_range(src: &str, start: u32, end: u32) -> Option<(std::ops::Range<u32>, String)> {
    let parsed = leek_parser::parse_with_features(
        src,
        SourceId::new(1).unwrap(),
        Version::V4,
        leek_parser::ParseFeatures::default(),
    );
    format_range(&parsed.green, Version::V4, &opts(), start..end)
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

// ---- `// fmt:` pragmas reach a range format (#203) ----
//
// A whole-document format walks every pragma comment on its way to
// the target, so its options are whatever the user set them to. A
// range format jumps straight to the target and used to start from an
// unmodified `FormatOptions` — so on-type formatting (which fires on
// every `;` and `}`) silently disagreed with format-on-save about the
// user's own settings. These pin the replay.

/// Whole-document format of `src`, for agreement cross-checks.
fn fmt_all(src: &str) -> String {
    format_source(src, SourceId::new(1).unwrap(), Version::V4, &opts())
}

#[test]
fn file_level_pragma_applies_to_a_range_format() {
    let src = "// fmt: space_before_call_paren = true\nfunction f() {\n    g(1);\n}\n";
    let (s, e) = span_of(src, "g(1);");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "g (1);");
    // …and that is what a whole-document format produces too.
    let all = fmt_all(src);
    assert!(all.contains("g (1);"), "{all:?}");
}

#[test]
fn later_set_pragma_wins_over_an_earlier_one() {
    // Replay order matters: pragmas are applied in document order, so
    // the last `set` before the target is the one in force.
    let src = "// fmt: space_before_call_paren = true\n// fmt: space_before_call_paren = false\nfunction f() {\n    g(1);\n}\n";
    let (s, e) = span_of(src, "g(1);");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "g(1);");
}

#[test]
fn print_time_pragma_reaches_the_printer() {
    // `indent` is a print-time option: it rides into the printer on a
    // `Doc::WithOptions` wrapper rather than changing the Doc IR.
    let src = "// fmt: indent = 2\nfunction f() {\nif (x) {\nbody;\n}\n}\n";
    let (s, e) = span_of(src, "function f() {\nif (x) {\nbody;\n}\n}");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "function f() {\n  if (x) {\n    body;\n  }\n}");
}

#[test]
fn next_pragma_immediately_before_target_applies() {
    let src = "function f() {\n    // fmt: next space_before_call_paren = true\n    g(1);\n}\n";
    let (s, e) = span_of(src, "g(1);");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "g (1);");
}

#[test]
fn stacked_next_pragmas_all_apply_to_the_target() {
    // Documented `next` semantics: several stack onto the same item.
    let src = "function f() {\n    // fmt: next space_before_call_paren = true\n    // fmt: next space_after_comma = false\n    g(1, 2);\n}\n";
    let (s, e) = span_of(src, "g(1, 2);");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "g (1,2);");
}

#[test]
fn next_pragma_survives_an_intervening_plain_comment() {
    // The sibling walker holds a queued `next` across trivia and hands
    // it to the first real item it reaches; the replay matches that,
    // or range and whole-document formatting disagree again.
    let src = "function f() {\n    // fmt: next space_before_call_paren = true\n    // just a note\n    g(1);\n}\n";
    let (s, e) = span_of(src, "g(1);");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "g (1);");
    let all = fmt_all(src);
    assert!(all.contains("g (1);"), "{all:?}");
}

#[test]
fn next_pragma_further_back_does_not_apply() {
    // `h(1);` consumed the override; `g(1);` must be formatted with
    // the plain options. Applying it here would silently mis-format.
    let src = "function f() {\n    // fmt: next space_before_call_paren = true\n    h(1);\n    g(1);\n}\n";
    let (s, e) = span_of(src, "g(1);");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "g(1);");
    // The whole-document format scopes it to `h(1);` the same way.
    let all = fmt_all(src);
    assert!(all.contains("h (1);"), "{all:?}");
    assert!(all.contains("\n    g(1);"), "{all:?}");
}

#[test]
fn unmatched_push_before_target_is_still_active() {
    let src = "// fmt: push space_before_call_paren = true\nfunction f() {\n    g(1);\n}\n";
    let (s, e) = span_of(src, "g(1);");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "g (1);");
}

#[test]
fn push_popped_before_target_is_no_longer_active() {
    // The mirror of the test above: `pop` restores the pushed
    // snapshot, so the target sees the original options again.
    let src =
        "// fmt: push space_before_call_paren = true\n// fmt: pop\nfunction f() {\n    g(1);\n}\n";
    let (s, e) = span_of(src, "g(1);");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "g(1);");
}
