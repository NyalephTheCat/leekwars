//! Whitespace, line, and block comments. Each is emitted as a
//! trivia token covering the full whitespace/comment span.

use leek_syntax::{SyntaxKind, Token, Version};

use crate::Lexer;

impl Lexer<'_> {
    pub(crate) fn whitespace(&mut self, start: usize) {
        while self.pos < self.text.len() {
            match self.text[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                // Non-breaking space (NBSP) is two bytes in UTF-8.
                0xC2 if self.peek_at(1) == Some(0xA0) => self.pos += 2,
                _ => break,
            }
        }
        self.tokens.push(Token::new(
            SyntaxKind::Whitespace,
            self.span(start, self.pos),
        ));
    }

    pub(crate) fn line_comment(&mut self, start: usize) {
        self.pos += 2; // skip "//"
        while self.pos < self.text.len() && self.text[self.pos] != b'\n' {
            self.pos += 1;
        }
        self.tokens.push(Token::new(
            SyntaxKind::LineComment,
            self.span(start, self.pos),
        ));
    }

    pub(crate) fn block_comment(&mut self, start: usize) {
        self.pos += 2; // skip "/*"
        // v1 quirk (`LexicalParser.java:582`): if the char right
        // after `/*` is `/`, the comment ends there — `/*/` is a
        // complete 3-char block comment. So `/*// basic; */` lexes
        // as `/*/` (comment) + `/ basic; */ ...` (tokens), which
        // then produces an OPERATOR_UNEXPECTED at parse time.
        if self.version < Version::V2 && self.pos < self.text.len() && self.text[self.pos] == b'/' {
            self.pos += 1;
            self.tokens.push(Token::new(
                SyntaxKind::BlockComment,
                self.span(start, self.pos),
            ));
            return;
        }
        while self.pos + 1 < self.text.len() {
            if self.text[self.pos] == b'*' && self.text[self.pos + 1] == b'/' {
                self.pos += 2;
                self.tokens.push(Token::new(
                    SyntaxKind::BlockComment,
                    self.span(start, self.pos),
                ));
                return;
            }
            self.pos += 1;
        }
        // Unterminated block comment: consume to EOF and emit it as a
        // comment token covering every remaining byte — which is what
        // the formatter reads to decide the file ends unclosed
        // (#417/#419).
        //
        // No diagnostic. Upstream's `tryParseComments`
        // (`LexicalParser.java:586`) is
        // `while (stream.hasMore() && (peek() != '*' || peek(1) != '/')) next();`
        // — it runs off the end of the input and says nothing, so an
        // unterminated `/*` is *valid LeekScript*, used by upstream AI
        // fixtures (`code/french.leek`) to comment out a trailing block
        // of code. Warning about it was a false positive on a program
        // the reference compiler runs happily (#351).
        self.pos = self.text.len();
        let span = self.span(start, self.pos);
        self.tokens.push(Token::new(SyntaxKind::BlockComment, span));
    }
}
