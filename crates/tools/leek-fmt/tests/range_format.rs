//! Tests for `format_range` — partial-document formatting used by
//! the LSP's `textDocument/rangeFormatting`.

use std::fmt::Write as _;

use leek_fmt::{FormatOptions, IndentStyle, format_range, format_source};
use leek_span::SourceId;
use leek_syntax::Version;

fn opts() -> FormatOptions {
    FormatOptions::default()
}

fn tab_opts() -> FormatOptions {
    FormatOptions {
        indent_style: IndentStyle::Tabs,
        ..FormatOptions::default()
    }
}

fn fmt_range(src: &str, start: u32, end: u32) -> Option<(std::ops::Range<u32>, String)> {
    fmt_range_with(src, &opts(), start, end)
}

fn fmt_range_with(
    src: &str,
    opts: &FormatOptions,
    start: u32,
    end: u32,
) -> Option<(std::ops::Range<u32>, String)> {
    let parsed = leek_parser::parse_with_features(
        src,
        SourceId::new(1).unwrap(),
        Version::V4,
        leek_parser::ParseFeatures::default(),
    );
    format_range(&parsed.green, Version::V4, opts, start..end)
}

/// Apply the text-edit `format_range` describes, the way an LSP
/// client would: splice `replacement` over `range` in `src`. The
/// point of the level-based indent is that the result reads like the
/// document format, so several tests below check exactly that.
fn splice(src: &str, range: &std::ops::Range<u32>, replacement: &str) -> String {
    let start = usize::try_from(range.start).unwrap();
    let end = usize::try_from(range.end).unwrap();
    format!("{}{replacement}{}", &src[..start], &src[end..])
}

/// The text `format_range`'s edit replaces.
fn slice<'a>(src: &'a str, range: &std::ops::Range<u32>) -> &'a str {
    &src[usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap()]
}

/// The leading whitespace run of `line`.
fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
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

// ---- the replacement is printed at the target's indent level (#200) ----
//
// The subtree used to be printed at level 0 and every continuation
// line then padded out with `" ".repeat(byte column of the node)`.
// That wrote spaces into a tab-indented file, shifted every line when
// a multi-byte character sat ahead of the node, and used the node's
// physical column even when the node started mid-line. Printing the
// doc inside `indent(level, …)` hands all of it to the printer.

#[test]
fn re_indents_nested_block_to_its_logical_level() {
    // The `if` is one `Block` deep, so its own lines print at level
    // 1 and its body at level 2 — four and eight spaces under the
    // default options, which is where the document format puts them.
    let src = "function f() {\n    if (x) {\n        body  ;\n    }\n}\n";
    let (s, e) = span_of(src, "if (x) {\n        body  ;\n    }");
    let (range, out) = fmt_range(src, s, e).expect("found");

    assert_eq!(out, "if (x) {\n        body;\n    }");
    // Line one starts at column 0: the caller splices it in at the
    // node's start offset, which is already indented.
    assert_eq!(leading_ws(out.lines().next().unwrap()), "");
    // Splicing the edit in reproduces the whole-document format.
    assert_eq!(splice(src, &range, &out), fmt_all(src));
}

#[test]
fn an_off_region_node_keeps_the_whitespace_its_range_starts_with() {
    // The printer indents only right after a line break, so the
    // replacement's first line never needs trimming back to column 0
    // — and trimming it anyway would eat real content here. A node
    // inside a `// fmt: off` region is emitted from its own source
    // text, and the parser attaches the whitespace in front of a
    // node's first token *inside* the node: this `TypeRef`'s range
    // is `" integer"`, space included. Strip that space and the edit
    // splices back as `publicinteger`.
    let src = "// fmt: off\nclass A {\n    public integer x = 0;\n}\n";
    let (s, e) = span_of(src, "integer");
    let (range, out) = fmt_range(src, s, e).expect("found");

    assert_eq!(
        &src[usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap()],
        " integer"
    );
    assert_eq!(out, " integer");
    assert_eq!(splice(src, &range, &out), src);
}

#[test]
fn tab_indent_style_never_mixes_a_space_into_the_indent() {
    // The padding this replaced was always spaces, so a tab-indented
    // file came back with tabs inside the subtree and spaces in front
    // of them. Every leading whitespace run must now be tabs only.
    let src = "function f() {\n\tswitch (x) {\n\t\tcase 1:\n\t\t\tif (y) {\n\t\t\t\tg( ) ;\n\t\t\t}\n\t}\n}\n";
    let (s, e) = span_of(src, "if (y) {\n\t\t\t\tg( ) ;\n\t\t\t}");
    let (range, out) = fmt_range_with(src, &tab_opts(), s, e).expect("found");

    for line in out.lines() {
        assert!(
            !leading_ws(line).contains(' '),
            "space in the indent of {line:?}: {out:?}"
        );
    }
    assert_eq!(out, "if (y) {\n\t\t\t\tg();\n\t\t\t}");
    let all = format_source(src, SourceId::new(1).unwrap(), Version::V4, &tab_opts());
    assert_eq!(splice(src, &range, &out), all);
}

#[test]
fn a_node_starting_mid_line_is_indented_by_level_not_by_its_column() {
    // The block opens at column 11, but its contents belong at the
    // level of the `if` that holds it — the byte-column padding put
    // the closing brace under the `{` instead.
    let src = "function f() {\n    if (c) { longcall(aaaaaaaaaaaa, bbbbbbbbbbbbb, ccccccccccccc, ddddddddddddd, eeeeeeeeeeee); }\n}\n";
    let (s, e) = span_of(
        src,
        "{ longcall(aaaaaaaaaaaa, bbbbbbbbbbbbb, ccccccccccccc, ddddddddddddd, eeeeeeeeeeee); }",
    );
    let (_range, out) = fmt_range(src, s, e).expect("found");

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "{", "got: {out:?}");
    assert!(lines[1].starts_with("        longcall("), "got: {out:?}");
    assert_eq!(*lines.last().unwrap(), "    }", "got: {out:?}");
}

#[test]
fn a_multi_byte_char_ahead_of_the_node_does_not_shift_the_indent() {
    // `"eee"` is three bytes wider than it is columns, so the byte
    // column of the `if` overshot its visual column by three — and
    // the node's column was the wrong anchor to begin with.
    let src =
        "function f() {\n    g(\"\u{e9}\u{e9}\u{e9}\"); if (x) {\n        body  ;\n    }\n}\n";
    let (s, e) = span_of(src, "if (x) {\n        body  ;\n    }");
    let (_range, out) = fmt_range(src, s, e).expect("found");

    assert_eq!(out, "if (x) {\n        body;\n    }");
}

#[test]
fn a_statement_in_a_switch_arm_sits_two_levels_below_the_switch() {
    // `SwitchStmt` indents its arms and `SwitchCase` indents the
    // statements after its colon (#497), so both count — a missed one
    // is an off-by-one indent.
    let src = "function f() {\n    switch (x) {\n        case 1:\n            g( 1 );\n        default:\n            h( ) ;\n    }\n}\n";

    let (s, e) = span_of(src, "case 1:\n            g( 1 );");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    // The arm's label prints at level 2 and its body at level 3 —
    // exactly where the whole-document format puts them. (No splice
    // cross-check here: a `SwitchCase`'s text range starts at the
    // whitespace in front of `case`, so the edit would swallow the
    // line break the arm sits on.)
    assert_eq!(out, "case 1:\n            g(1);");
    assert!(fmt_all(src).contains("\n        case 1:\n            g(1);"));

    // A `default` arm has no label expression; every node child of it
    // is a body statement.
    let (s, e) = span_of(src, "h( ) ;");
    let (_range, out) = fmt_range(src, s, e).expect("found");
    assert_eq!(out, "h();");
}

#[test]
fn the_switch_scrutinee_does_not_get_the_arm_indent() {
    // The scrutinee rides in the `switch (…)` header, outside the
    // `indent` that holds the arms: counting `SwitchStmt` for it
    // would push its broken argument list a level too deep.
    let scrutinee = "someverylongfunctionname(aaaaaaaaaaaaaaaaaaaa, bbbbbbbbbbbbbbbbbbbb, cccccccccccccccccccc, dddddddddddddddddddd)";
    let src = format!(
        "function f() {{\n    switch ({scrutinee}) {{\n        case 1:\n            g();\n    }}\n}}\n"
    );
    let (s, e) = span_of(&src, scrutinee);
    let (range, out) = fmt_range(&src, s, e).expect("found");

    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[1].starts_with("        aaaa"), "got: {out:?}");
    assert_eq!(*lines.last().unwrap(), "    )", "got: {out:?}");
    assert_eq!(splice(&src, &range, &out), fmt_all(&src));
}

#[test]
fn a_class_member_is_indented_by_its_class_body() {
    let src = "class A {\n    public add(integer n) -> integer {\nreturn n;\n}\n}\n";
    let (s, e) = span_of(src, "public add(integer n) -> integer {\nreturn n;\n}");
    let (range, out) = fmt_range(src, s, e).expect("found");

    assert_eq!(
        out,
        "public add(integer n) -> integer {\n        return n;\n    }"
    );
    assert_eq!(splice(src, &range, &out), fmt_all(src));
}

#[test]
fn returns_none_for_range_past_eof() {
    let src = "var x = 1;\n";
    let out = fmt_range(src, 0, 999);
    assert!(out.is_none());
}

// ---- a selection several top-level items wide (#200) ----
//
// Nothing below the `SourceFile` root contains such a range, so
// `smallest_enclosing_node` used to answer `None` — "degenerate,
// callers should use `format` instead". Neither LSP caller falls
// back: both turn `None` into an empty edit list, so "Format
// Selection" over two statements silently did nothing. The target is
// now the document, and the edit is narrowed to the lines the
// document format actually rewrites.

#[test]
fn formats_a_selection_spanning_two_statements() {
    let src = "var x   =1;\nvar y=2   ;\nvar z = 3;\n";
    let (s, _) = span_of(src, "var x   =1;");
    let (_, e) = span_of(src, "var y=2   ;");
    let (range, out) = fmt_range(src, s, e).expect("an edit for a two-statement selection");

    assert_eq!(out, "var x = 1;\nvar y = 2;\n");
    // Narrowed to the selected lines: `var z` is already formatted,
    // so it stays out of the edit.
    assert_eq!(slice(src, &range), "var x   =1;\nvar y=2   ;\n");
    assert_eq!(splice(src, &range, &out), fmt_all(src));
}

#[test]
fn a_whole_file_range_formats_the_document() {
    let src = "var x   =1;\nfunction f() {\nreturn 2;\n}\nvar last = 3;\n";
    let (range, out) = fmt_range(src, 0, u32::try_from(src.len()).unwrap()).expect("an edit");

    assert_eq!(splice(src, &range, &out), fmt_all(src));
    // Still not one edit spanning the buffer: the trailing lines the
    // format leaves alone are left out of it.
    assert_eq!(
        slice(src, &range),
        "var x   =1;\nfunction f() {\nreturn 2;\n"
    );
    assert_eq!(out, "var x = 1;\nfunction f() {\n    return 2;\n");
}

#[test]
fn returns_none_when_no_line_of_the_range_changes() {
    // The differing span is empty, and `None` is the answer — both
    // LSP handlers read it as "no edits", and `rangeFormatting` would
    // have dropped an edit whose replacement equals the original
    // slice on its own anyway.
    let src = "var x = 1;\nvar y = 2;\n";
    assert!(fmt_range(src, 0, u32::try_from(src.len()).unwrap()).is_none());
}

#[test]
fn a_formatted_selection_does_not_pick_up_a_malformed_line_outside_it() {
    // What keeps a range format a *range* format: `var x` above is
    // malformed, the selection does not cover it, so there is no edit
    // to send — not the whole-document format.
    let src = "var x   =1;\nvar y = 2;\nvar z = 3;\n";
    let (s, _) = span_of(src, "var y = 2;");
    assert!(fmt_range(src, s, u32::try_from(src.len()).unwrap()).is_none());
}

#[test]
fn the_edit_is_narrowed_to_the_changed_lines_of_a_long_file() {
    // Returning the whole file as one edit would destroy the client's
    // cursor and selection, so the edit has to stop at the lines that
    // change: forty formatted lines around three malformed ones.
    let mut src = String::new();
    for i in 0..20 {
        writeln!(src, "var a{i} = {i};").unwrap();
    }
    let selected = "var b   =1;\nfunction g( ) {\nreturn 2;\n}\n";
    src.push_str(selected);
    for i in 0..20 {
        writeln!(src, "var c{i} = {i};").unwrap();
    }
    let (s, e) = span_of(&src, selected);
    let (range, out) = fmt_range(&src, s, e).expect("an edit");

    // Inside the selection, and stopping at its last *changed* line:
    // the `}` already sits where the formatter wants it.
    assert!(
        range.start >= s && range.end <= e,
        "{range:?} outside {s}..{e}"
    );
    assert_eq!(
        slice(&src, &range),
        "var b   =1;\nfunction g( ) {\nreturn 2;\n"
    );
    assert_eq!(out, "var b = 1;\nfunction g() {\n    return 2;\n");
    assert_eq!(splice(&src, &range, &out), fmt_all(&src));
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
