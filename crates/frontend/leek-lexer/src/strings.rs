//! String-literal lexing — `"…"` and `'…'` with backslash escapes.

use leek_diagnostics::{Diagnostic, codes};
use leek_syntax::{SyntaxKind, Token};

use crate::Lexer;

impl Lexer<'_> {
    pub(crate) fn string_literal(&mut self, start: usize, quote: u8) {
        self.pos += 1; // consume opening quote
        let mut escaped = false;
        while self.pos < self.text.len() {
            let c = self.text[self.pos];
            if escaped {
                escaped = false;
                self.pos += 1;
                continue;
            }
            if c == b'\\' {
                escaped = true;
                self.pos += 1;
                continue;
            }
            if c == quote {
                self.pos += 1;
                self.tokens.push(Token::new(
                    SyntaxKind::StringLiteral,
                    self.span(start, self.pos),
                ));
                return;
            }
            self.pos += 1;
        }
        // EOF inside string.
        let span = self.span(start, self.pos);
        self.diagnostics.push(Diagnostic::error(
            codes::STRING_NOT_CLOSED,
            span,
            "string literal not closed before end of file",
        ));
        self.tokens
            .push(Token::new(SyntaxKind::StringLiteral, span));
    }
}
