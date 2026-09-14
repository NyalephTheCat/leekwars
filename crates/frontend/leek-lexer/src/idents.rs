//! Identifier and keyword lexing.

use leek_syntax::kind::keyword_lookup;
use leek_syntax::{SyntaxKind, Token, is_ident_continue};

use crate::Lexer;

impl Lexer<'_> {
    pub(crate) fn ident_or_keyword(&mut self, start: usize) {
        // Consume the start character (may be multi-byte for Latin-1).
        if let Some(c) = self.peek_char() {
            self.pos += c.len_utf8();
        }
        // Consume continuation characters.
        while let Some(c) = self.peek_char() {
            if is_ident_continue(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        let word = std::str::from_utf8(&self.text[start..self.pos]).unwrap_or("");
        let kind = keyword_lookup(word, self.version).unwrap_or(SyntaxKind::Ident);
        self.tokens
            .push(Token::new(kind, self.span(start, self.pos)));
    }
}

/// Maps the special standalone identifier characters (∞ and π) to
/// their token kinds.
pub(crate) fn special_ident_kind(c: char) -> Option<SyntaxKind> {
    match c {
        '\u{221E}' => Some(SyntaxKind::Lemniscate),
        '\u{03C0}' => Some(SyntaxKind::Pi),
        _ => None,
    }
}
