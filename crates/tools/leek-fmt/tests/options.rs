//! Tests for non-default [`FormatOptions`].

use leek_fmt::{FormatOptions, IndentStyle, TrailingComma, format_source};
use leek_span::SourceId;
use leek_syntax::Version;

fn fmt_with(opts: &FormatOptions, src: &str) -> String {
    format_source(src, SourceId::new(1).unwrap(), Version::V4, opts)
}

fn opts() -> FormatOptions {
    FormatOptions::default()
}

#[test]
fn tabs_indent_uses_tab_chars() {
    let mut o = opts();
    o.indent_style = IndentStyle::Tabs;
    let out = fmt_with(&o, "function f() { return 1; }\n");
    assert!(
        out.contains("\treturn 1"),
        "expected tab indent, got: {out:?}"
    );
}

#[test]
fn space_indent_uses_indent_width() {
    let mut o = opts();
    o.indent = 2;
    let out = fmt_with(&o, "function f() { return 1; }\n");
    assert!(
        out.contains("\n  return 1"),
        "expected 2-space indent, got: {out:?}"
    );
}

#[test]
fn trailing_comma_always_emits_on_break() {
    let mut o = opts();
    o.trailing_comma = TrailingComma::Always;
    o.max_line_length = 30; // force the list to break
    let src = "var x = [argument_one, argument_two, argument_three];\n";
    let out = fmt_with(&o, src);
    assert!(
        out.contains("argument_three,\n]"),
        "expected trailing comma on broken list, got: {out:?}"
    );
}

#[test]
fn trailing_comma_never_strips_existing() {
    let mut o = opts();
    o.trailing_comma = TrailingComma::Never;
    o.max_line_length = 30;
    let src = "var x = [argument_one, argument_two, argument_three,];\n";
    let out = fmt_with(&o, src);
    assert!(
        !out.contains("argument_three,\n]"),
        "expected no trailing comma, got: {out:?}"
    );
}

#[test]
fn trailing_comma_preserve_keeps_user_choice() {
    let mut o = opts();
    o.trailing_comma = TrailingComma::Preserve;
    o.max_line_length = 30;
    let with_comma = "var x = [argument_one, argument_two, argument_three,];\n";
    let no_comma = "var x = [argument_one, argument_two, argument_three];\n";
    assert!(fmt_with(&o, with_comma).contains("argument_three,\n]"));
    assert!(!fmt_with(&o, no_comma).contains("argument_three,\n]"));
}

#[test]
fn trailing_comma_never_in_flat_mode() {
    // Even with Always, flat (short) lists should not get a comma.
    let mut o = opts();
    o.trailing_comma = TrailingComma::Always;
    let out = fmt_with(&o, "var x = [1, 2, 3];\n");
    assert!(
        !out.contains("3,]"),
        "flat list should not have trailing comma, got: {out:?}"
    );
}

#[test]
fn max_blank_lines_zero_collapses_all() {
    let mut o = opts();
    o.max_blank_lines = 0;
    let src = "var a = 1;\n\n\n\nvar b = 2;\n";
    let out = fmt_with(&o, src);
    // No `\n\n` should appear inside the body (only at most the
    // single trailing `\n`).
    assert!(
        !out.contains("\n\n"),
        "expected no blank lines, got: {out:?}"
    );
}

#[test]
fn max_blank_lines_one_caps_runs() {
    let mut o = opts();
    o.max_blank_lines = 1;
    let src = "var a = 1;\n\n\n\nvar b = 2;\n";
    let out = fmt_with(&o, src);
    assert!(
        out.contains("\n\nvar b"),
        "expected one blank between items, got: {out:?}"
    );
    assert!(
        !out.contains("\n\n\n"),
        "expected only one blank, got: {out:?}"
    );
}

#[test]
fn space_before_call_paren() {
    let mut o = opts();
    o.space_before_call_paren = true;
    let out = fmt_with(&o, "var x = foo(1, 2);\n");
    assert!(
        out.contains("foo (1, 2)"),
        "expected space before (, got: {out:?}"
    );
}

#[test]
fn fmt_off_region_preserved_verbatim() {
    let src = "\
function ok() { return 1; }
// fmt: off
function    weird(  x,y ){return x+y;}
// fmt: on
function nice() { return 2; }
";
    let out = fmt_with(&opts(), src);
    assert!(
        out.contains("function    weird(  x,y ){return x+y;}"),
        "off region not preserved verbatim: {out:?}"
    );
    assert!(out.contains("function ok() {\n    return 1;\n}"));
    assert!(out.contains("function nice() {\n    return 2;\n}"));
}

#[test]
fn fmt_off_without_on_runs_to_eof() {
    let src = "\
function ok() { return 1; }
// fmt: off
function    weird(  x){return x;}
function    also_weird(  y){return y;}
";
    let out = fmt_with(&opts(), src);
    assert!(
        out.contains("function    weird(  x){return x;}"),
        "first off-region function not verbatim: {out:?}"
    );
    assert!(
        out.contains("function    also_weird(  y){return y;}"),
        "subsequent off-region function not verbatim: {out:?}"
    );
}

#[test]
fn fmt_skip_applies_to_next_sibling_only() {
    let src = "\
var a   =  1;
// fmt-skip
var b   =   2;
var c   =   3;
";
    let out = fmt_with(&opts(), src);
    assert!(
        out.contains("var a = 1;"),
        "unmarked stmt should reformat: {out:?}"
    );
    assert!(
        out.contains("var b   =   2;"),
        "fmt-skip should preserve b: {out:?}"
    );
    assert!(
        out.contains("var c = 3;"),
        "unmarked stmt after skip should reformat: {out:?}"
    );
}

#[test]
fn alternate_skip_spelling_works() {
    let src = "// fmt: skip\nvar x   =  1;\n";
    let out = fmt_with(&opts(), src);
    assert!(
        out.contains("var x   =  1;"),
        "fmt: skip not honored: {out:?}"
    );
}

// ---- Local-override pragmas (set / push / pop) ----

#[test]
fn pragma_set_changes_trailing_comma() {
    // Source has no trailing comma; pragma forces one in broken mode.
    let mut o = opts();
    o.max_line_length = 30;
    let src = "\
// fmt: trailing_comma = always
var x = [one_argument, two_argument, three_argument];
";
    let out = fmt_with(&o, src);
    assert!(
        out.contains("three_argument,\n]"),
        "set trailing_comma=always didn't take: {out:?}"
    );
}

#[test]
fn pragma_push_pop_scopes_override() {
    let mut o = opts();
    o.trailing_comma = leek_fmt::TrailingComma::Never;
    o.max_line_length = 30;
    let src = "\
var a = [one_argument, two_argument, three_argument];
// fmt: push trailing_comma = always
var b = [one_argument, two_argument, three_argument];
// fmt: pop
var c = [one_argument, two_argument, three_argument];
";
    let out = fmt_with(&o, src);

    // a: never (from default), b: always (pushed), c: never (popped back).
    let lines: Vec<&str> = out.lines().collect();
    // Find each var's closing `]` and check whether the preceding
    // line ends with a comma.
    let a_end = lines.iter().position(|l| l == &"];").expect("a closing");
    let after_a: Vec<&str> = lines[a_end + 1..].to_vec();
    let b_end_off = after_a.iter().position(|l| l == &"];").expect("b closing");
    let b_end = a_end + 1 + b_end_off;
    let after_b: Vec<&str> = lines[b_end + 1..].to_vec();
    let c_end_off = after_b.iter().position(|l| l == &"];").expect("c closing");
    let c_end = b_end + 1 + c_end_off;

    assert!(
        !lines[a_end - 1].ends_with(','),
        "a should not have trailing comma: {out:?}"
    );
    assert!(
        lines[b_end - 1].ends_with(','),
        "b SHOULD have trailing comma: {out:?}"
    );
    assert!(
        !lines[c_end - 1].ends_with(','),
        "c should not have trailing comma after pop: {out:?}"
    );
}

#[test]
fn pragma_pop_with_empty_stack_is_noop() {
    // A stray `// fmt: pop` should not crash or corrupt later
    // formatting.
    let src = "\
// fmt: pop
var x = 1;
";
    let out = fmt_with(&opts(), src);
    assert!(
        out.contains("var x = 1;"),
        "stray pop broke formatter: {out:?}"
    );
}

#[test]
fn pragma_set_max_blank_lines_zero() {
    let src = "\
// fmt: max_blank_lines = 0
var a = 1;


var b = 2;
";
    let out = fmt_with(&opts(), src);
    assert!(
        !out.contains("\n\n"),
        "max_blank_lines = 0 should collapse blanks: {out:?}"
    );
}

#[test]
fn pragma_comment_itself_is_hidden_from_output() {
    let src = "\
// fmt: trailing_comma = always
var x = 1;
";
    let out = fmt_with(&opts(), src);
    assert!(
        !out.contains("// fmt:"),
        "pragma comment should be suppressed: {out:?}"
    );
}

#[test]
fn quoted_and_unquoted_values_equivalent() {
    // String enum values should accept both `tabs` and `"tabs"`.
    let src_quoted = "// fmt: trailing_comma = \"always\"\nvar x = [\n  a,\n  b,\n  c\n];\n";
    let src_unquoted = "// fmt: trailing_comma = always\nvar x = [\n  a,\n  b,\n  c\n];\n";
    assert_eq!(
        fmt_with(&opts(), src_quoted),
        fmt_with(&opts(), src_unquoted)
    );
}

#[test]
fn unknown_pragma_key_is_ignored() {
    // No panic, formatter still runs to completion.
    let src = "// fmt: nonexistent_key = whatever\nvar x = 1;\n";
    let out = fmt_with(&opts(), src);
    assert!(out.contains("var x = 1;"));
}

#[test]
fn set_method_parses_known_keys() {
    let mut o = FormatOptions::default();
    o.set("indent", "2").unwrap();
    o.set("indent_style", "tabs").unwrap();
    o.set("trailing_comma", "always").unwrap();
    o.set("max_blank_lines", "3").unwrap();
    o.set("space_before_call_paren", "true").unwrap();
    o.set("max_line_length", "120").unwrap();
    assert_eq!(o.indent, 2);
    assert_eq!(o.indent_style, leek_fmt::IndentStyle::Tabs);
    assert_eq!(o.trailing_comma, leek_fmt::TrailingComma::Always);
    assert_eq!(o.max_blank_lines, 3);
    assert!(o.space_before_call_paren);
    assert_eq!(o.max_line_length, 120);
}

#[test]
fn set_method_rejects_unknown_key() {
    let mut o = FormatOptions::default();
    assert!(o.set("nonexistent", "x").is_err());
}

// ---- `// fmt: next` pragma (single-item scoped overrides) ----

#[test]
fn pragma_next_applies_to_only_one_item() {
    let mut o = opts();
    o.trailing_comma = leek_fmt::TrailingComma::Never;
    o.max_line_length = 30;
    let src = "\
var a = [one_argument, two_argument, three_argument];
// fmt: next trailing_comma = always
var b = [one_argument, two_argument, three_argument];
var c = [one_argument, two_argument, three_argument];
";
    let out = fmt_with(&o, src);
    let lines: Vec<&str> = out.lines().collect();
    let a_end = lines.iter().position(|l| l == &"];").unwrap();
    let b_end = a_end + 1 + lines[a_end + 1..].iter().position(|l| l == &"];").unwrap();
    let c_end = b_end + 1 + lines[b_end + 1..].iter().position(|l| l == &"];").unwrap();
    assert!(!lines[a_end - 1].ends_with(','), "a: {out:?}");
    assert!(
        lines[b_end - 1].ends_with(','),
        "b should have trailing comma: {out:?}"
    );
    assert!(
        !lines[c_end - 1].ends_with(','),
        "c should be back to Never: {out:?}"
    );
}

#[test]
fn pragma_next_stacks_multiple_overrides() {
    // Two `next` pragmas in a row apply to the same following item.
    let mut o = opts();
    o.trailing_comma = leek_fmt::TrailingComma::Never;
    o.max_blank_lines = 1;
    o.max_line_length = 30;
    let src = "\
// fmt: next trailing_comma = always
// fmt: next max_blank_lines = 0
var b = [one_argument, two_argument, three_argument];
";
    let out = fmt_with(&o, src);
    assert!(
        out.contains("three_argument,\n]"),
        "next trailing_comma=always should take effect: {out:?}"
    );
}

#[test]
fn pragma_next_without_following_item_is_silent() {
    // `next` at end of file should not panic or carry over.
    let src = "var a = 1;\n// fmt: next trailing_comma = always\n";
    let out = fmt_with(&opts(), src);
    assert!(out.contains("var a = 1;"));
}

#[test]
fn pragma_next_does_not_leak_to_subsequent_items() {
    // The override must not persist across multiple items.
    let mut o = opts();
    o.max_line_length = 30;
    let src = "\
// fmt: next trailing_comma = always
var a = [one_argument, two_argument, three_argument];
var b = [one_argument, two_argument, three_argument];
";
    let out = fmt_with(&o, src);
    let lines: Vec<&str> = out.lines().collect();
    let a_end = lines.iter().position(|l| l == &"];").unwrap();
    let b_end = a_end + 1 + lines[a_end + 1..].iter().position(|l| l == &"];").unwrap();
    assert!(
        lines[a_end - 1].ends_with(','),
        "a should have trailing comma: {out:?}"
    );
    assert!(
        !lines[b_end - 1].ends_with(','),
        "b should NOT inherit: {out:?}"
    );
}

#[test]
fn pragma_next_works_in_class_body() {
    let mut o = opts();
    o.max_line_length = 40;
    let src = "\
class Foo {
    public a() { return [one_argument, two_argument, three_argument]; }
    // fmt: next trailing_comma = always
    public b() { return [one_argument, two_argument, three_argument]; }
}
";
    let out = fmt_with(&o, src);
    assert!(
        out.contains("three_argument,\n        ]"),
        "next pragma should work in class body: {out:?}"
    );
}

// ---- Per-region print-time pragmas (indent / indent_style /
//      max_line_length) — the gap closed alongside Lint v0.2.

#[test]
fn pragma_push_indent_affects_only_region() {
    // First function uses default indent=4; second uses indent=2
    // via `push`. The `pop` restores the default for any later
    // code.
    let src = "\
function outer() {
    return 1;
}
// fmt: push indent = 2
function inner() {
    return 2;
}
// fmt: pop
function after() {
    return 3;
}
";
    let out = fmt_with(&opts(), src);
    // `outer` and `after` should use 4-space indent, `inner` should
    // use 2-space indent.
    assert!(
        out.contains("\n    return 1"),
        "outer should keep 4-space indent: {out:?}"
    );
    assert!(
        out.contains("\n  return 2"),
        "inner should switch to 2-space indent: {out:?}"
    );
    assert!(
        out.contains("\n    return 3"),
        "after-pop should restore 4-space indent: {out:?}"
    );
}

#[test]
fn pragma_push_indent_style_tabs_affects_only_region() {
    let src = "\
function spaces() {
    return 1;
}
// fmt: push indent_style = tabs
function tabs() {
    return 2;
}
// fmt: pop
";
    let out = fmt_with(&opts(), src);
    assert!(
        out.contains("\n    return 1"),
        "first body keeps spaces: {out:?}"
    );
    assert!(
        out.contains("\n\treturn 2"),
        "second body switches to tabs: {out:?}"
    );
}

#[test]
fn pragma_push_max_line_length_breaks_region() {
    // 1000-col budget by default; force a 20-col budget mid-file
    // so a wide expression breaks where it normally wouldn't.
    let mut o = opts();
    o.max_line_length = 1000;
    let src = "\
function wide() { return [aaa, bbb, ccc, ddd]; }
// fmt: push max_line_length = 20
function narrow() { return [aaa, bbb, ccc, ddd]; }
// fmt: pop
";
    let out = fmt_with(&o, src);
    // The default region keeps the array on one line.
    assert!(
        out.contains("return [aaa, bbb, ccc, ddd]"),
        "wide region stayed on one line: {out:?}"
    );
    // The pushed region's array should break (multi-line).
    let narrow_idx = out.find("function narrow").expect("narrow fn present");
    let narrow_body = &out[narrow_idx..];
    assert!(
        narrow_body.contains("aaa,\n"),
        "narrow region's array should break: {narrow_body:?}"
    );
}

// ---- space_inside_brackets / space_inside_parens ----

#[test]
fn space_inside_brackets_pads_collections() {
    let mut o = opts();
    o.space_inside_brackets = true;
    let out = fmt_with(&o, "var x = [1, 2, 3];\n");
    assert!(
        out.contains("[ 1, 2, 3 ]"),
        "expected padded array, got: {out:?}"
    );
}

#[test]
fn space_inside_brackets_off_by_default() {
    let out = fmt_with(&opts(), "var x = [1, 2, 3];\n");
    assert!(
        out.contains("[1, 2, 3]"),
        "default should be tight, got: {out:?}"
    );
}

#[test]
fn space_inside_parens_pads_call_arguments() {
    let mut o = opts();
    o.space_inside_parens = true;
    let out = fmt_with(&o, "foo(a, b);\n");
    assert!(
        out.contains("foo( a, b )"),
        "expected padded call, got: {out:?}"
    );
}

#[test]
fn space_inside_parens_pads_paren_expr() {
    let mut o = opts();
    o.space_inside_parens = true;
    let out = fmt_with(&o, "var x = (a + b);\n");
    assert!(
        out.contains("( a + b )"),
        "expected padded paren, got: {out:?}"
    );
}

// ---- brace_style ----

#[test]
fn brace_style_next_line_for_functions() {
    let mut o = opts();
    o.brace_style = leek_fmt::BraceStyle::NextLine;
    let out = fmt_with(&o, "function f() { return 1; }\n");
    // The opening brace goes on its own line under `function f()`.
    assert!(
        out.contains("function f()\n{"),
        "expected Allman brace, got: {out:?}"
    );
}

#[test]
fn brace_style_next_line_for_if_else() {
    let mut o = opts();
    o.brace_style = leek_fmt::BraceStyle::NextLine;
    let src = "function f() { if (x) { return 1; } else { return 2; } }\n";
    let out = fmt_with(&o, src);
    assert!(out.contains("if (x)\n"), "if brace on next line: {out:?}");
    // `else` starts its own line after the closing brace.
    assert!(out.contains("}\n    else\n"), "else on own line: {out:?}");
}

#[test]
fn brace_style_default_is_same_line() {
    let out = fmt_with(&opts(), "function f() { return 1; }\n");
    assert!(out.contains("function f() {"), "default K&R: {out:?}");
}

#[test]
fn brace_style_next_line_is_idempotent() {
    let mut o = opts();
    o.brace_style = leek_fmt::BraceStyle::NextLine;
    let src = "class C extends B {\n    m() {\n        for (var i = 0; i < 3; i++) {\n            x();\n        }\n    }\n}\n";
    let once = fmt_with(&o, src);
    let twice = fmt_with(&o, &once);
    assert_eq!(once, twice, "next_line formatting must be idempotent");
}

// ---- space_after_comma ----

#[test]
fn space_after_comma_off_tightens_lists() {
    let mut o = opts();
    o.space_after_comma = false;
    let out = fmt_with(&o, "var x = [1, 2, 3];\n");
    assert!(
        out.contains("[1,2,3]"),
        "expected tight commas, got: {out:?}"
    );
}

#[test]
fn space_after_comma_default_on() {
    let out = fmt_with(&opts(), "foo(a,b,c);\n");
    assert!(out.contains("foo(a, b, c)"), "default spaced, got: {out:?}");
}

// ---- space_after_control_keyword ----

#[test]
fn control_keyword_paren_can_be_tight() {
    let mut o = opts();
    o.space_after_control_keyword = false;
    let out = fmt_with(
        &o,
        "function f() { if (x) { return 1; } while (y) { return 2; } }\n",
    );
    assert!(out.contains("if(x)"), "expected if(x), got: {out:?}");
    assert!(out.contains("while(y)"), "expected while(y), got: {out:?}");
}

#[test]
fn control_keyword_paren_default_spaced() {
    let out = fmt_with(&opts(), "function f() { if (x) { return 1; } }\n");
    assert!(out.contains("if (x)"), "default spaced, got: {out:?}");
}

// ---- space_around_arrow ----

#[test]
fn arrow_spacing_can_be_tight() {
    let mut o = opts();
    o.space_around_arrow = false;
    let out = fmt_with(&o, "var f = x -> x;\n");
    // The lambda param is parenthesised; the point is the arrow is tight.
    assert!(out.contains(")->x"), "expected tight arrow, got: {out:?}");
    assert!(!out.contains("-> x"), "no space after arrow, got: {out:?}");
}

#[test]
fn return_arrow_default_spaced() {
    let out = fmt_with(&opts(), "function f() -> integer { return 1; }\n");
    assert!(out.contains("-> integer"), "default spaced, got: {out:?}");
}

// ---- map colon spacing ----

#[test]
fn space_before_colon_pads_map_keys() {
    let mut o = opts();
    o.space_before_colon = true;
    let out = fmt_with(&o, "var m = [a: 1, b: 2];\n");
    assert!(out.contains("a : 1"), "expected `a : 1`, got: {out:?}");
}

#[test]
fn space_after_colon_off_tightens_map() {
    let mut o = opts();
    o.space_after_colon = false;
    let out = fmt_with(&o, "var m = [a: 1, b: 2];\n");
    assert!(out.contains("a:1"), "expected `a:1`, got: {out:?}");
}

// ---- pad_line_comments ----

#[test]
fn pad_line_comments_adds_space() {
    let mut o = opts();
    o.pad_line_comments = true;
    let out = fmt_with(&o, "//hello\nvar x = 1;\n");
    assert!(
        out.contains("// hello"),
        "expected padded comment, got: {out:?}"
    );
}

#[test]
fn pad_line_comments_leaves_doc_comments() {
    let mut o = opts();
    o.pad_line_comments = true;
    let out = fmt_with(&o, "///doc\nvar x = 1;\n");
    // `///` is a doc comment; it keeps its form (no extra space inserted
    // after the third slash).
    assert!(
        out.contains("///doc"),
        "doc comment untouched, got: {out:?}"
    );
}

#[test]
fn pad_line_comments_off_by_default() {
    let out = fmt_with(&opts(), "//tight\nvar x = 1;\n");
    assert!(out.contains("//tight"), "default preserves, got: {out:?}");
}

// ---- combined config idempotence ----

#[test]
fn combined_nondefault_config_is_idempotent() {
    let mut o = opts();
    o.space_inside_brackets = true;
    o.space_after_control_keyword = false;
    o.space_before_colon = true;
    o.brace_style = leek_fmt::BraceStyle::NextLine;
    let src = "function f(a, b) { if (a) { return [x: 1, y: 2]; } }\n";
    let once = fmt_with(&o, src);
    let twice = fmt_with(&o, &once);
    assert_eq!(once, twice, "combined config must be idempotent");
}
