//! The formatter must never delete a comment.
//!
//! Comments can sit anywhere the lexer allows trivia — inside
//! expressions, argument lists, literals and declaration headers, not
//! just between statements. Constructs that don't place such comments
//! fall back to verbatim output; [`check_equivalence`] is the guard
//! callers run before writing formatted text.

use std::fmt::Write as _;
use std::path::PathBuf;

use leek_fmt::{
    ControlBraces, FormatOptions, QuoteStyle, Semicolons, TrailingComma, check_equivalence,
    format_source,
};
use leek_span::SourceId;
use leek_syntax::{SyntaxKind, Version};

fn fmt_with(opts: &FormatOptions, src: &str) -> String {
    format_source(src, SourceId::new(1).unwrap(), Version::V4, opts)
}

/// Default options plus two option sets that exercise every rewrite
/// the formatter may make (braces, parens, semicolons, commas, quotes).
fn option_sets() -> Vec<(&'static str, FormatOptions)> {
    let rewrite_heavy = FormatOptions {
        control_braces: ControlBraces::Always,
        semicolons: Semicolons::Always,
        trailing_comma: TrailingComma::Always,
        quote_style: QuoteStyle::Single,
        remove_redundant_parens: true,
        ..FormatOptions::default()
    };
    let compacting = FormatOptions {
        control_braces: ControlBraces::Never,
        trailing_comma: TrailingComma::Never,
        quote_style: QuoteStyle::Double,
        collapse_else_if: true,
        remove_redundant_parens: true,
        ..FormatOptions::default()
    };

    vec![
        ("default", FormatOptions::default()),
        ("rewrite-heavy", rewrite_heavy),
        ("compacting", compacting),
    ]
}

/// Format `src`; the guard must pass and the output must be idempotent.
/// Returns the formatted text, or a failure report.
fn check_safe(label: &str, opts: &FormatOptions, src: &str) -> Result<String, String> {
    let once = fmt_with(opts, src);
    if let Err(err) = check_equivalence(src, &once, Version::V4) {
        return Err(format!(
            "{label}: {err}\n--- input ---\n{src}\n--- output ---\n{once}"
        ));
    }
    let twice = fmt_with(opts, &once);
    if once != twice {
        return Err(format!(
            "{label}: not idempotent\n--- input ---\n{src}\n--- once ---\n{once}\n--- twice ---\n{twice}"
        ));
    }
    Ok(once)
}

fn assert_no_failures(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "{} failure(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn comments_inside_constructs_are_kept() {
    let cases: &[(&str, &str)] = &[
        ("binary line", "var x = 1 + // why\n    2\n"),
        ("binary block", "var x = 1 /* one */ + 2\n"),
        ("call args", "f(a, /* b */ c)\n"),
        ("call args line", "f(a, // first\n    b)\n"),
        ("array", "var arr = [1, // one\n    2]\n"),
        ("map", "var m = [1: /* k */ 2]\n"),
        ("object", "var o = {a: /* v */ 1}\n"),
        ("var decl", "var y = /* lit */ 5\n"),
        ("if condition", "if (/* cond */ x) { return 1 }\n"),
        ("while condition", "while (x /* c */) { x++ }\n"),
        ("param list", "function g(a /* p */, b) { return a }\n"),
        ("param default", "function h(a = /* d */ 1) { return a }\n"),
        ("return", "function r() { return /* r */ 1 }\n"),
        ("method chain", "var v = a.b() /* mid */ .c().d().e()\n"),
        ("lambda", "var f = (a, /* b */ b) => a + b\n"),
        ("ternary", "var t = x ? /* yes */ 1 : 2\n"),
        ("unary", "var n = - /* neg */ 1\n"),
        ("index", "var i = arr[/* idx */ 0]\n"),
        ("new", "var p = new /* c */ Foo()\n"),
        ("paren", "var q = (/* inner */ a)\n"),
        (
            "for header",
            "for (var i = 0; /* c */ i < 3; i++) { debug(i) }\n",
        ),
        (
            "foreach header",
            "for (var v /* c */ in arr) { debug(v) }\n",
        ),
        (
            "else",
            "if (x) { debug(1) } /* between */ else { debug(2) }\n",
        ),
        ("before block brace", "function f() /* c */ { return 1 }\n"),
        ("fn header", "function /* name */ k() { return 1 }\n"),
        (
            "comment-only block",
            "function e() {\n    // nothing yet\n}\n",
        ),
        ("comment-only loop body", "while (x) { // only comment\n}\n"),
        (
            "class trailing",
            "class A {\n    x = 1\n    // trailing\n}\n",
        ),
        ("class comment-only", "class B {\n    // todo\n}\n"),
        ("class field", "class C {\n    x = /* v */ 1\n}\n"),
    ];
    let mut failures = Vec::new();
    for (name, src) in cases {
        for (set, opts) in option_sets() {
            let out = match check_safe(&format!("{name} [{set}]"), &opts, src) {
                Ok(out) => out,
                Err(failure) => {
                    failures.push(failure);
                    continue;
                }
            };
            for comment in comment_texts(src) {
                if !out.contains(comment.trim_end()) {
                    failures.push(format!(
                        "{name} [{set}]: comment {comment:?} missing from output:\n{out}"
                    ));
                }
            }
        }
    }
    assert_no_failures(&failures);
}

/// Raw comment texts of `src`, in order.
fn comment_texts(src: &str) -> Vec<&str> {
    leek_lexer::lex(src, SourceId::new(1).unwrap(), Version::V4)
        .tokens
        .iter()
        .filter(|t| matches!(t.kind, SyntaxKind::LineComment | SyntaxKind::BlockComment))
        .map(|t| &src[t.span.range()])
        .collect()
}

fn fixture_inputs() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".in.leek"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no fixtures under {}", dir.display());
    out
}

/// A block comment in front of every significant token.
fn inject_block_comments(src: &str) -> String {
    let tokens = leek_lexer::lex(src, SourceId::new(1).unwrap(), Version::V4).tokens;
    let mut out = String::with_capacity(src.len() * 3);
    let mut pos = 0;
    for (i, t) in tokens
        .iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != SyntaxKind::Eof)
        .enumerate()
    {
        let range = t.span.range();
        out.push_str(&src[pos..range.start]);
        write!(out, " /*c{i}*/ ").unwrap();
        pos = range.start;
    }
    out.push_str(&src[pos..]);
    out
}

/// A line comment at the end of every line that ends in whitespace
/// trivia (so never inside a string or block comment).
fn inject_line_comments(src: &str) -> String {
    let tokens = leek_lexer::lex(src, SourceId::new(1).unwrap(), Version::V4).tokens;
    let mut out = String::with_capacity(src.len() * 2);
    let mut pos = 0;
    let mut n = 0;
    for t in tokens.iter().filter(|t| t.kind == SyntaxKind::Whitespace) {
        let range = t.span.range();
        for (offset, _) in src[range.clone()].match_indices('\n') {
            let at = range.start + offset;
            out.push_str(&src[pos..at]);
            write!(out, " // l{n}").unwrap();
            n += 1;
            pos = at;
        }
    }
    out.push_str(&src[pos..]);
    out
}

#[test]
fn fixtures_pass_the_equivalence_guard() {
    // No false positives: every curated fixture formats cleanly under
    // every option set.
    let mut failures = Vec::new();
    for (name, src) in fixture_inputs() {
        for (set, opts) in option_sets() {
            if let Err(failure) = check_safe(&format!("{name} [{set}]"), &opts, &src) {
                failures.push(failure);
            }
        }
    }
    assert_no_failures(&failures);
}

#[test]
fn injected_comments_survive_every_fixture() {
    // Property check: comments in front of every token, or at the end
    // of every line, must all survive formatting.
    let mut failures = Vec::new();
    for (name, src) in fixture_inputs() {
        let variants = [
            ("block comments", inject_block_comments(&src)),
            ("line comments", inject_line_comments(&src)),
        ];
        for (kind, variant) in variants {
            for (set, opts) in option_sets() {
                let label = format!("{name} with {kind} [{set}]");
                if let Err(failure) = check_safe(&label, &opts, &variant) {
                    failures.push(failure);
                }
            }
        }
    }
    assert_no_failures(&failures);
}
