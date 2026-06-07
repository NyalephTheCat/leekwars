//! Identifier and keyword lexing.

use leek_syntax::kind::keyword_lookup;
use leek_syntax::{SyntaxKind, Token};

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

/// Identifier-start character. Matches `LexicalParser.java:432–434`:
/// ASCII letters, underscore, plus the Latin-1 letter blocks.
pub(crate) fn is_ident_start(c: char) -> bool {
    if c.is_ascii_alphabetic() || c == '_' {
        return true;
    }
    matches!(
        c,
        '\u{00C0}'..='\u{00D6}' // À–Ö
        | '\u{00D8}'..='\u{00DD}' // Ø–Ý
        | '\u{00E0}'..='\u{00F6}' // à–ö
        | '\u{00F8}'..='\u{00FD}' // ø–ý
        | '\u{0152}'..='\u{0153}' // Œ–œ
        | '\u{00FF}'                // ÿ
    )
}

pub(crate) fn is_ident_continue(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
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
