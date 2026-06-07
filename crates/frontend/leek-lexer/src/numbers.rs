//! Numeric literal lexing — decimal integers and reals plus
//! `0x…` / `0b…` prefixed integers.

use leek_diagnostics::{Diagnostic, codes};
use leek_syntax::{SyntaxKind, Token};

use crate::Lexer;

impl Lexer<'_> {
    pub(crate) fn number_literal(&mut self, start: usize) {
        // 0x / 0b prefixes: integer-only, no fractional/exponent.
        if self.text[self.pos] == b'0' {
            match self.peek_at(1) {
                Some(b'x' | b'X') => return self.prefixed_int(start, is_hex_digit),
                Some(b'b' | b'B') => return self.prefixed_int(start, is_bin_digit),
                _ => {}
            }
        }

        // Integer part. Underscores between digits are allowed
        // (`1_000`, `1_000_000`), but two in a row (`1__000`) is an
        // error per upstream — track and emit at end.
        let mut multiple_sep = false;
        let mut last_was_underscore = false;
        while self.pos < self.text.len()
            && (self.text[self.pos].is_ascii_digit() || self.text[self.pos] == b'_')
        {
            if self.text[self.pos] == b'_' {
                if last_was_underscore {
                    multiple_sep = true;
                }
                last_was_underscore = true;
            } else {
                last_was_underscore = false;
            }
            self.pos += 1;
        }
        // Optional fractional part. `0.5` is real; `0.` (trailing
        // dot, no fractional digits) is also a real literal (`0.0`).
        // But `..` is the range op (`0..3`), and `0.foo` is member
        // access on an int — so only treat `.` as a fraction when the
        // char after it is NOT another `.` and NOT an identifier-start
        // (a digit, `]`, `,`, `)`, whitespace, operator, or EOF all
        // qualify).
        let mut is_real = false;
        if self.peek_at(0) == Some(b'.')
            && self.peek_at(1) != Some(b'.')
            && !self
                .peek_at(1)
                .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        {
            is_real = true;
            self.pos += 1; // consume '.'
            while self.pos < self.text.len()
                && (self.text[self.pos].is_ascii_digit() || self.text[self.pos] == b'_')
            {
                self.pos += 1;
            }
        }
        // Optional exponent: e/E or p/P, optional sign, digits.
        if let Some(c) = self.peek_at(0)
            && matches!(c, b'e' | b'E' | b'p' | b'P')
        {
            let saved = self.pos;
            self.pos += 1;
            if let Some(s) = self.peek_at(0)
                && matches!(s, b'+' | b'-')
            {
                self.pos += 1;
            }
            if self.peek_at(0).is_some_and(|c| c.is_ascii_digit()) {
                is_real = true;
                while self.pos < self.text.len() && self.text[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
            } else {
                // Rewind — wasn't really an exponent.
                self.pos = saved;
            }
        }
        // Trailing letter immediately after the number — `12345r`,
        // `0_x_ff` etc. The user almost certainly meant a typed
        // literal or hex literal; emit INVALID_NUMBER spanning the
        // whole token rather than letting the bad suffix become a
        // separate Ident.
        let mut bad_suffix = false;
        if self
            .peek_at(0)
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        {
            bad_suffix = true;
            while self.pos < self.text.len()
                && (self.text[self.pos].is_ascii_alphanumeric() || self.text[self.pos] == b'_')
            {
                self.pos += 1;
            }
        }
        let kind = if is_real {
            SyntaxKind::RealLiteral
        } else {
            SyntaxKind::IntLiteral
        };
        let span = self.span(start, self.pos);
        if bad_suffix {
            self.diagnostics.push(Diagnostic::error(
                codes::INVALID_NUMBER,
                span,
                "numeric literal has a non-digit suffix",
            ));
        } else if multiple_sep {
            self.diagnostics.push(Diagnostic::error(
                codes::MULTIPLE_NUMERIC_SEPARATORS,
                span,
                "numeric literal has consecutive `_` separators",
            ));
        }
        self.tokens.push(Token::new(kind, span));
    }

    /// Lex a `0x`/`0b`-prefixed integer literal. `is_digit` validates
    /// each base digit. The lexer consumes the `0x`/`0b` plus *any*
    /// alphanumeric run after it, so we can emit a diagnostic on
    /// invalid digits rather than silently truncating (matches
    /// `TestNumber` expectations for `0b101a` → `INVALID_NUMBER`).
    ///
    /// Underscores between digits are allowed and silently skipped
    /// (`0x_ff`, `0b1010_0011`, `0xff_ff`).
    pub(crate) fn prefixed_int(&mut self, start: usize, is_digit: fn(u8) -> bool) {
        self.pos += 2; // consume "0x" or "0b"
        let digits_start = self.pos;
        let mut bad: Option<u8> = None;
        let mut any_digit = false;
        // True only for `0x`-prefixed floats with a `.` or `p`/`P`
        // exponent — `0x1.p53`, `0xa.bcdp-42`, etc. (`0b` floats
        // are not a thing.)
        let mut is_hex_float = false;
        // `is_digit` is one of two `fn(u8) -> bool` items
        // (`is_hex_digit` / `is_bin_digit`) — compare by pointer to
        // decide which branch we're in without threading another flag.
        let is_hex = std::ptr::fn_addr_eq(is_digit, is_hex_digit as fn(u8) -> bool);
        while self.pos < self.text.len() {
            let c = self.text[self.pos];
            if c == b'_' {
                self.pos += 1;
            } else if is_digit(c) {
                self.pos += 1;
                any_digit = true;
            } else if is_hex
                && c == b'.'
                && self
                    .peek_at(1)
                    .is_some_and(|n| is_hex_digit(n) || n == b'p' || n == b'P')
            {
                // Hex float fractional part.
                is_hex_float = true;
                self.pos += 1;
                while self.pos < self.text.len()
                    && (is_hex_digit(self.text[self.pos]) || self.text[self.pos] == b'_')
                {
                    self.pos += 1;
                }
            } else if is_hex && (c == b'p' || c == b'P') {
                // Mandatory binary exponent on a hex float
                // (`0x<hex>[.<hex>]p[+|-]<dec>`).
                let saved = self.pos;
                self.pos += 1;
                if let Some(s) = self.peek_at(0)
                    && matches!(s, b'+' | b'-')
                {
                    self.pos += 1;
                }
                if self.peek_at(0).is_some_and(|c| c.is_ascii_digit()) {
                    is_hex_float = true;
                    while self.pos < self.text.len() && self.text[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                } else {
                    self.pos = saved;
                    break;
                }
                break;
            } else if c.is_ascii_alphanumeric() {
                if bad.is_none() {
                    bad = Some(c);
                }
                self.pos += 1;
            } else {
                break;
            }
        }
        let span = self.span(start, self.pos);
        if !any_digit && self.pos > digits_start && bad.is_none() {
            // Only underscores after the prefix — that's malformed.
            self.diagnostics.push(Diagnostic::error(
                codes::INVALID_NUMBER,
                span,
                "numeric literal has only `_` separators after prefix",
            ));
        } else if self.pos == digits_start {
            self.diagnostics.push(Diagnostic::error(
                codes::INVALID_NUMBER,
                span,
                "numeric literal has no digits after prefix".to_string(),
            ));
        } else if let Some(c) = bad {
            self.diagnostics.push(Diagnostic::error(
                codes::INVALID_NUMBER,
                span,
                format!("invalid digit {:?} in numeric literal", c as char),
            ));
        }
        let kind = if is_hex_float {
            SyntaxKind::RealLiteral
        } else {
            SyntaxKind::IntLiteral
        };
        self.tokens.push(Token::new(kind, span));
    }
}

fn is_hex_digit(c: u8) -> bool {
    c.is_ascii_digit() || matches!(c, b'a'..=b'f' | b'A'..=b'F')
}

fn is_bin_digit(c: u8) -> bool {
    matches!(c, b'0' | b'1')
}
