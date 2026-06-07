//! End-to-end fixture test: lex an upstream `.leek` file and check basic
//! invariants. As the lexer grows, this should grow into a snapshot diff
//! against tokens dumped from the Java reference.

use leek_lexer::lex;
use leek_span::SourceId;
use leek_syntax::{SyntaxKind, SyntaxNode, Version, build_flat_tree, parse_pragmas};
use leek_test_corpus::upstream_fixture;

fn lex_upstream(rel: &str) -> Vec<SyntaxKind> {
    let text = upstream_fixture(rel);
    let src = SourceId::new(1).unwrap();
    let result = lex(&text, src, Version::LATEST);
    assert!(
        result.diagnostics.is_empty(),
        "unexpected lexer diagnostics for {}: {:?}",
        rel,
        result.diagnostics,
    );
    result.tokens.iter().map(|t| t.kind).collect()
}

#[test]
fn library_leek_lexes_cleanly() {
    let kinds = lex_upstream("library.leek");
    let last = *kinds.last().expect("non-empty token stream");
    assert_eq!(last, SyntaxKind::Eof);

    // Spot-check a few expected kinds appear somewhere in the stream.
    assert!(kinds.contains(&SyntaxKind::KwFunction));
    assert!(kinds.contains(&SyntaxKind::KwVar));
    assert!(kinds.contains(&SyntaxKind::KwFor));
    assert!(kinds.contains(&SyntaxKind::KwReturn));
}

#[test]
fn multiple_includes_leek_lexes_cleanly() {
    let kinds = lex_upstream("multiple_includes.leek");
    assert_eq!(*kinds.last().unwrap(), SyntaxKind::Eof);
    // Three include statements in source → three `KwInclude` tokens.
    let n_includes = kinds
        .iter()
        .filter(|k| **k == SyntaxKind::KwInclude)
        .count();
    assert_eq!(n_includes, 3);
}

#[test]
fn bonjour_leek_lexes_cleanly() {
    let kinds = lex_upstream("bonjour.leek");
    assert_eq!(*kinds.last().unwrap(), SyntaxKind::Eof);
}

/// Lossless round-trip: the green tree must reconstruct the source byte
/// for byte. This is the foundation invariant for the formatter, LSP,
/// and incremental reparsing.
fn assert_round_trips(rel: &str) {
    let text = upstream_fixture(rel);
    let src = SourceId::new(1).unwrap();
    let (pragmas, pragma_diags) = parse_pragmas(&text, src);
    let result = lex(&text, src, pragmas.version);
    let green = build_flat_tree(&text, &result.tokens);
    let node = SyntaxNode::new_root(green);

    assert_eq!(
        node.text().to_string(),
        text,
        "tree text != source for {rel}",
    );
    assert!(
        pragma_diags.is_empty() && result.diagnostics.is_empty(),
        "unexpected diagnostics in {rel}: pragma={:?} lex={:?}",
        pragma_diags,
        result.diagnostics,
    );
}

#[test]
fn round_trip_library() {
    assert_round_trips("library.leek");
}

#[test]
fn round_trip_bonjour() {
    assert_round_trips("bonjour.leek");
}

#[test]
fn round_trip_multiple_includes() {
    assert_round_trips("multiple_includes.leek");
}

#[test]
fn round_trip_array_keys() {
    assert_round_trips("array_keys.leek");
}

#[test]
fn round_trip_include_sub() {
    assert_round_trips("include_sub.leek");
}

#[test]
fn round_trip_include_multiple() {
    assert_round_trips("include_multiple.leek");
}
