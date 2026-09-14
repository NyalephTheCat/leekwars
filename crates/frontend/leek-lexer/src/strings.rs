//! String-literal lexing — `"…"` and `'…'` with backslash escapes.

use leek_diagnostics::{codes, diag};
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
        // EOF inside string. The token keeps running to EOF rather
        // than stopping at the first newline: upstream
        // (`LexicalParser.tryParseString`) scans through newlines by
        // design — "les strings peuvent contenir des newlines" — so a
        // closed literal may legitimately span lines, and truncating
        // the unclosed one at `\n` would put our token boundaries
        // somewhere upstream never puts them. The label on the opening
        // quote is what makes the diagnostic navigable instead.
        let span = self.span(start, self.pos);
        let quote = char::from(quote);
        self.diagnostics.push(diag!(
            codes::STRING_NOT_CLOSED,
            span,
            "string literal not closed before end of file";
            label = (self.span(start, start + 1), format!("unclosed `{quote}` opened here")),
            note = "a string may span newlines, so this literal runs to the end of the file",
        ));
        self.tokens
            .push(Token::new(SyntaxKind::StringLiteral, span));
    }
}
