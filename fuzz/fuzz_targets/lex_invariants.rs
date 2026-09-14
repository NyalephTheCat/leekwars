//! Fuzz the lexer directly, for the invariants every stage above it
//! relies on and the parser target cannot isolate (#140):
//!
//!   1. it never panics — including on unterminated strings and block
//!      comments, and on runs of characters that start no token;
//!   2. token spans tile the input: they start at 0, are contiguous,
//!      and the stream ends with an `Eof` at `text.len()`;
//!   3. every token *and diagnostic* span boundary is a UTF-8
//!      character boundary inside the text, so slicing the source by
//!      one (which every renderer and the formatter both do) cannot
//!      panic — this is what the multi-byte `bad_char` path and the
//!      run-merging in `extend_bad_char_run` have to preserve;
//!   4. every label span sits inside its diagnostic's own span.
//!
//! The in-tree `leek-lexer` test `lex_invariants` is the
//! deterministic, regression-pinned version of this.
//!
//! Run:  cargo +nightly fuzz run lex_invariants
#![no_main]

use libfuzzer_sys::fuzz_target;

use leek_lexer::lex;
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, Version};

fn assert_sliceable(text: &str, span: Span, what: &str) {
    let range = span.range();
    assert!(range.start <= range.end, "inverted {what} span in {text:?}");
    assert!(range.end <= text.len(), "{what} span past EOF in {text:?}");
    assert!(
        text.is_char_boundary(range.start) && text.is_char_boundary(range.end),
        "{what} span splits a code point in {text:?}",
    );
}

fn check(text: &str, version: Version) {
    let source = SourceId::new(1).unwrap();
    let result = lex(text, source, version);

    let last = result
        .tokens
        .last()
        .unwrap_or_else(|| panic!("empty token stream for {text:?}"));
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
                label.span.start >= diagnostic.span.start
                    && label.span.end <= diagnostic.span.end,
                "label escapes its diagnostic in {text:?}",
            );
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    for version in [Version::V1, Version::V2, Version::V3, Version::V4] {
        check(text, version);
    }
});
