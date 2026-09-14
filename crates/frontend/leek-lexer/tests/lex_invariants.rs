//! The deterministic, regression-pinned twin of the `lex_invariants`
//! fuzz target (`fuzz/fuzz_targets/lex_invariants.rs`).
//!
//! Same four invariants, run over a fixed list of inputs at every
//! language version so they are checked on every `cargo test` rather
//! than only under `cargo +nightly fuzz`:
//!
//!   1. lexing never panics;
//!   2. token spans tile the input and end in an `Eof` at `text.len()`;
//!   3. every token, diagnostic and label span is sliceable — inside
//!      the text and on UTF-8 character boundaries;
//!   4. every label span sits inside its diagnostic's own span.
//!
//! The cases that motivated it (#140) are the lexer's three
//! end-of-input edges: an unterminated string, an unterminated block
//! comment, and a run of characters that starts no token.

use leek_lexer::lex;
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, Version};

const VERSIONS: [Version; 4] = [Version::V1, Version::V2, Version::V3, Version::V4];

fn assert_sliceable(text: &str, span: Span, what: &str) {
    let range = span.range();
    assert!(range.start <= range.end, "inverted {what} span in {text:?}");
    assert!(range.end <= text.len(), "{what} span past EOF in {text:?}");
    assert!(
        text.is_char_boundary(range.start) && text.is_char_boundary(range.end),
        "{what} span {range:?} splits a code point in {text:?}",
    );
}

fn check(text: &str, version: Version) {
    let source = SourceId::new(1).expect("1 is a valid source id");
    let result = lex(text, source, version);

    let last = result.tokens.last().expect("a token stream is never empty");
    assert_eq!(last.kind, SyntaxKind::Eof, "stream must end in Eof");

    let mut cursor = 0usize;
    for token in &result.tokens {
        assert_sliceable(text, token.span, "token");
        assert_eq!(
            token.span.range().start,
            cursor,
            "gap/overlap at {token:?} in {text:?}",
        );
        cursor = token.span.range().end;
    }
    assert_eq!(cursor, text.len(), "tokens do not cover {text:?}");

    for diagnostic in &result.diagnostics {
        assert_sliceable(text, diagnostic.span, "diagnostic");
        for label in &diagnostic.labels {
            assert_sliceable(text, label.span, "label");
            assert!(
                label.span.start >= diagnostic.span.start && label.span.end <= diagnostic.span.end,
                "label escapes its diagnostic in {text:?}",
            );
        }
    }
}

#[test]
fn end_of_input_edges_hold_the_invariants() {
    const CASES: &[&str] = &[
        "",
        "\"",
        "'",
        "\"unterminated",
        "'unterminated",
        "var x = \"a\nb\nc",
        "\"escaped quote at eof \\\"",
        "\"trailing backslash \\",
        "/*",
        "/* unterminated block",
        "/*/",
        "/*/ return 1",
        "/**",
        "/* a */ /* b",
        "//",
        "// trailing comment",
        "\u{a7}",
        "\u{a7}\u{a7}\u{a7}",
        "\u{a7} \u{a7}",
        "var \u{6f22}\u{5b57}x = 1",
        "\u{1f980}\u{1f980}\u{1f980}",
        "é à ü \u{6f22}\u{5b57} \u{1f980}",
        "var \u{0}x = 1",
        "\u{221e}\u{3c0}",
        "0x 0b 1.2.3 1e",
        "@#$%^&",
    ];
    for case in CASES {
        for version in VERSIONS {
            check(case, version);
        }
    }
}

/// A run of unlexable characters is one token and one diagnostic, at
/// every version — and the span still tiles (checked by `check`).
#[test]
fn bad_char_runs_collapse_at_every_version() {
    let source = SourceId::new(1).expect("1 is a valid source id");
    for version in VERSIONS {
        let result = lex("\u{a7}\u{a7}\u{a7}", source, version);
        assert_eq!(
            result
                .tokens
                .iter()
                .filter(|t| t.kind == SyntaxKind::Error)
                .count(),
            1,
            "{version:?}",
        );
        assert_eq!(result.diagnostics.len(), 1, "{version:?}");
    }
}
