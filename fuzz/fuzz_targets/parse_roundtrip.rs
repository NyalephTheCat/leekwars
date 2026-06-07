//! Fuzz the parser for two invariants on arbitrary input:
//!   1. it never panics (recursion guard, error recovery), and
//!   2. the CST losslessly round-trips — the text reconstructed from the green
//!      tree equals the input byte-for-byte.
//!
//! Run:  cargo +nightly fuzz run parse_roundtrip
//! (the in-tree `leek-parser` test `fuzz_roundtrip` is the deterministic
//! regression-pinned version of this.)
#![no_main]

use libfuzzer_sys::fuzz_target;

use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn check(text: &str, version: Version) {
    let source = SourceId::new(1).unwrap();
    let result = parse(text, source, version);
    let node = SyntaxNode::new_root(result.green);
    assert_eq!(
        node.text().to_string(),
        text,
        "round-trip mismatch ({version:?}) for {text:?}",
    );
}

fuzz_target!(|data: &[u8]| {
    // The parser takes `&str`; skip non-UTF-8 inputs (the lexer is byte-based
    // but its public entry is `&str`).
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Exercise every language version — each has distinct lexer/grammar rules.
    for version in [Version::V1, Version::V2, Version::V3, Version::V4] {
        check(text, version);
    }
});
