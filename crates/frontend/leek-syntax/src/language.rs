//! Rowan integration: lossless syntax tree.
//!
//! - [`LeekLanguage`] is the marker type implementing [`rowan::Language`].
//! - [`SyntaxNode`], [`SyntaxToken`], [`SyntaxElement`] are convenience
//!   aliases tied to that language.
//! - [`build_flat_tree`] consumes a token list and returns a [`GreenNode`]
//!   with one root and every token as a direct child. Trivia is preserved
//!   as leaf tokens; this is enough for round-trip testing and is what a
//!   future error-resilient parser will replace.
//!
//! The `unsafe` in [`LeekLanguage::kind_from_raw`] is the only place in
//! the workspace where we opt out of the global `unsafe_code = "deny"`
//! lint; the alternative would be a ~100-arm match over [`SyntaxKind`].
//! The safety invariant is documented inline and guarded by an assert.

#![allow(unsafe_code)]

use crate::{SyntaxKind, Token};

use rowan::Language as _;
pub use rowan::{GreenNode, NodeOrToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LeekLanguage {}

impl rowan::Language for LeekLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        // SAFETY: We only produce `rowan::SyntaxKind` from valid
        // `SyntaxKind` discriminants, and `SyntaxKind` is `#[repr(u16)]`.
        // The cast is bounded by the largest discriminant (`SourceFile`
        // at the moment); a panic guards against future drift.
        assert!(
            raw.0 <= SyntaxKind::ErrorNode as u16,
            "invalid raw SyntaxKind: {}",
            raw.0,
        );
        // Safe because of the assert + repr(u16).
        unsafe { std::mem::transmute::<u16, SyntaxKind>(raw.0) }
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

pub type SyntaxNode = rowan::SyntaxNode<LeekLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<LeekLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<LeekLanguage>;

/// Build a flat green tree: one [`SourceFile`](SyntaxKind::SourceFile)
/// node containing every token (and trivia) as a direct child.
///
/// The `Eof` sentinel is dropped — green trees don't need it because
/// the tree's extent already says where the source ends. All other
/// kinds are emitted, in order, with their source slice attached.
pub fn build_flat_tree(text: &str, tokens: &[Token]) -> GreenNode {
    use rowan::GreenNodeBuilder;
    let mut builder = GreenNodeBuilder::new();
    builder.start_node(LeekLanguage::kind_to_raw(SyntaxKind::SourceFile));
    for tok in tokens {
        if tok.kind == SyntaxKind::Eof {
            continue;
        }
        let slice = &text[tok.span.range()];
        builder.token(LeekLanguage::kind_to_raw(tok.kind), slice);
    }
    builder.finish_node();
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKind as S;
    use leek_span::{SourceId, Span};

    fn tok(kind: S, start: u32, end: u32) -> Token {
        Token::new(kind, Span::new(SourceId::new(1).unwrap(), start, end))
    }

    #[test]
    fn language_round_trip_kind() {
        for k in [
            S::Whitespace,
            S::Ident,
            S::IntLiteral,
            S::StringLiteral,
            S::KwVar,
            S::KwFunction,
            S::Eq,
            S::EqEq,
            S::EqEqEq,
            S::SourceFile,
            S::Eof,
            S::Error,
        ] {
            let raw = LeekLanguage::kind_to_raw(k);
            assert_eq!(LeekLanguage::kind_from_raw(raw), k);
        }
    }

    #[test]
    fn flat_tree_preserves_text() {
        let text = "var x = 1;";
        let tokens = vec![
            tok(S::KwVar, 0, 3),
            tok(S::Whitespace, 3, 4),
            tok(S::Ident, 4, 5),
            tok(S::Whitespace, 5, 6),
            tok(S::Eq, 6, 7),
            tok(S::Whitespace, 7, 8),
            tok(S::IntLiteral, 8, 9),
            tok(S::Semicolon, 9, 10),
            tok(S::Eof, 10, 10),
        ];
        let green = build_flat_tree(text, &tokens);
        let node = SyntaxNode::new_root(green);
        assert_eq!(node.kind(), S::SourceFile);
        // Children count: every non-Eof token (8).
        assert_eq!(node.children_with_tokens().count(), 8);
        // Reconstructed text exactly equals input.
        assert_eq!(node.text().to_string(), text);
    }
}
