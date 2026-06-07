//! Parser-level round-trip and shape checks against upstream fixtures.
//!
//! Only fixtures whose constructs the current parser-slice covers are
//! exercised. As the parser grows, more fixtures move from
//! "lexer-only" into this file.

use leek_parser::parse;
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, parse_pragmas};
use leek_test_corpus::upstream_fixture;

/// Parse fixture by relative path; assert round-trip and absence of
/// diagnostics.
fn assert_parses(rel: &str) {
    let text = upstream_fixture(rel);
    let src = SourceId::new(1).unwrap();
    let (pragmas, pragma_diags) = parse_pragmas(&text, src);
    let result = parse(&text, src, pragmas.version);
    let node = SyntaxNode::new_root(result.green);

    assert_eq!(
        node.text().to_string(),
        text,
        "round-trip mismatch in {rel}",
    );

    assert!(
        pragma_diags.is_empty() && result.diagnostics.is_empty(),
        "unexpected diagnostics in {rel}:\n  pragma: {:?}\n  parse:  {:?}",
        pragma_diags,
        result.diagnostics,
    );
}

#[test]
fn round_trip_bonjour() {
    // `return 'bonjour';` — simplest possible program.
    assert_parses("bonjour.leek");
}

// `library.leek`, `multiple_includes.leek`, `array_keys.leek`, etc. all
// use constructs not yet in the parser (function declarations, `include`,
// `for`, `push(...)` as a statement, etc.). They'll be moved here from
// lexer_fixtures.rs as the parser grows.
