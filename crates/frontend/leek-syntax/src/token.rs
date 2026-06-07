//! Tokens emitted by the lexer.

use crate::SyntaxKind;
use leek_span::Span;

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: SyntaxKind, span: Span) -> Self {
        Self { kind, span }
    }
}
