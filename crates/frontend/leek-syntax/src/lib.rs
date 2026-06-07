//! Syntax surface for Leekscript.
//!
//! - [`SyntaxKind`] enumerates token (and eventually node) kinds.
//! - [`Version`] selects the active language version, gating keywords.
//! - [`Token`] is what the lexer emits.
//!
//! Rowan integration lands in a later slice; for now the lexer emits a
//! flat `Vec<Token>`.

pub mod doc;
pub mod kind;
pub mod language;
pub mod pipeline;
pub mod pragma;
pub mod token;
pub mod version;

pub use kind::SyntaxKind;
pub use language::{LeekLanguage, SyntaxElement, SyntaxNode, SyntaxToken, build_flat_tree};
pub use pragma::{Pragmas, parse_pragmas};
pub use token::Token;
pub use version::Version;

use leek_span::{SourceId, Span};

/// [`Span`] of a CST node within `source`.
#[must_use]
pub fn node_span(node: &SyntaxNode, source: SourceId) -> Span {
    range_span(node.text_range(), source)
}

/// [`Span`] of a CST token within `source`.
#[must_use]
pub fn token_span(tok: &SyntaxToken, source: SourceId) -> Span {
    range_span(tok.text_range(), source)
}

fn range_span(range: rowan::TextRange, source: SourceId) -> Span {
    Span::new(source, u32::from(range.start()), u32::from(range.end()))
}
