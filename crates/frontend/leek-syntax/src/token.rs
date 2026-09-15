//! Tokens emitted by the lexer.

use crate::SyntaxKind;
use leek_span::Span;

#[derive(salsa::Update, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: SyntaxKind, span: Span) -> Self {
        Self { kind, span }
    }
}
