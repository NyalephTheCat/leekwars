//! Leekscript tokenizer.
//!
//! Hand-written for the first slice. We'll port to `winnow` once we
//! have enough test coverage to make the rewrite safe. Spec:
//! `doc/lexical.md`.
//!
//! ## What this slice covers
//!
//! - ASCII + Latin-1 identifiers (per `LexicalParser.java:432–434`).
//! - Decimal, hex (`0x…`), and binary (`0b…`) integer literals.
//! - Real literals with optional exponent.
//! - String literals with `"`/`'`, escape passthrough.
//! - Line and block comments (preserved as trivia tokens).
//! - The operator and punctuation subset enumerated in
//!   [`SyntaxKind`].
//! - Keyword lookup gated by [`Version`].
//! - Multi-byte safety: the catch-all advances by a full UTF-8
//!   character so we never split a code point.
//!
//! Module layout:
//! - [`comments`] — whitespace, line/block comments
//! - [`idents`] — identifiers and keyword lookup
//! - [`numbers`] — decimal/hex/binary numeric literals
//! - [`strings`] — string literals
//! - [`operators`] — multi-character operator dispatch

use leek_diagnostics::{Diagnostic, codes};
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, Token, Version};

mod comments;
mod idents;
mod numbers;
mod operators;
mod strings;

pub mod pipeline;

/// Test-only probe: counts how many times `lex_query` actually executed
/// (vs being served from the salsa cache). The mutex serializes the
/// few tests that read the counter so concurrent execution doesn't
/// blur their measurements.
#[cfg(all(test, feature = "salsa"))]
pub(crate) mod salsa_probe {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    pub(crate) static LEX_QUERY_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub(crate) static SERIAL: Mutex<()> = Mutex::new(());
}

use idents::{is_ident_start, special_ident_kind};

/// Lex `text` into a flat token list with attached diagnostics.
///
/// Trivia (whitespace, comments) are emitted as tokens; downstream
/// stages decide whether to skip them. An `Eof` token always
/// terminates the stream.
pub fn lex(text: &str, source: SourceId, version: Version) -> LexResult {
    Lexer::new(text, source, version).run()
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexResult {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Shared lexer state. Methods spread across submodules
/// (`comments`, `idents`, `numbers`, `strings`, `operators`)
/// mutate the same struct via `impl Lexer` blocks.
pub(crate) struct Lexer<'a> {
    pub(crate) text: &'a [u8],
    pub(crate) source: SourceId,
    pub(crate) version: Version,
    pub(crate) pos: usize,
    pub(crate) tokens: Vec<Token>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str, source: SourceId, version: Version) -> Self {
        Self {
            text: text.as_bytes(),
            source,
            version,
            pos: 0,
            tokens: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> LexResult {
        while self.pos < self.text.len() {
            self.next_token();
        }
        let eof_span = self.span(self.pos, self.pos);
        self.tokens.push(Token::new(SyntaxKind::Eof, eof_span));
        LexResult {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
        }
    }

    fn next_token(&mut self) {
        let start = self.pos;
        let c = self.text[self.pos];
        match c {
            // Whitespace incl. NBSP first byte (0xC2 0xA0).
            b' ' | b'\t' | b'\n' | b'\r' => self.whitespace(start),
            0xC2 if self.peek_at(1) == Some(0xA0) => self.whitespace(start),

            b'/' => match self.peek_at(1) {
                Some(b'/') => self.line_comment(start),
                Some(b'*') => self.block_comment(start),
                _ => self.op_one(SyntaxKind::Slash, start, |c| {
                    matches!(c, b'=')
                        .then_some((SyntaxKind::SlashEq, 2))
                        .or(None)
                }),
            },

            b'"' | b'\'' => self.string_literal(start, c),

            b'0'..=b'9' => self.number_literal(start),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.ident_or_keyword(start),
            // Multi-byte: Latin-1 letters or special idents (∞, π).
            // Decide on the first code point at the cursor.
            _ if c >= 0x80 => {
                let ch = self.peek_char();
                if matches!(ch, Some(c) if is_ident_start(c)) {
                    self.ident_or_keyword(start);
                } else if let Some(special_kind) = ch.and_then(special_ident_kind) {
                    let ch = ch.unwrap();
                    self.pos += ch.len_utf8();
                    self.push(special_kind, start, ch.len_utf8());
                } else {
                    self.bad_char(start);
                }
            }

            b'(' => self.single(SyntaxKind::LParen, start),
            b')' => self.single(SyntaxKind::RParen, start),
            b'[' => self.single(SyntaxKind::LBracket, start),
            b']' => self.single(SyntaxKind::RBracket, start),
            b'{' => self.single(SyntaxKind::LBrace, start),
            b'}' => self.single(SyntaxKind::RBrace, start),
            b',' => self.single(SyntaxKind::Comma, start),
            b';' => self.single(SyntaxKind::Semicolon, start),
            b':' => self.single(SyntaxKind::Colon, start),
            b'~' => self.single(SyntaxKind::Tilde, start),
            b'@' => self.single(SyntaxKind::At, start),

            b'.' => self.dot_or_dotdot(start),

            b'+' => self.op_plus(start),
            b'-' => self.op_minus(start),
            b'*' => self.op_star(start),
            b'\\' => self.op_backslash(start),
            b'%' => self.op_percent(start),
            b'=' => self.op_eq(start),
            b'!' => self.op_bang(start),
            b'<' => self.op_lt(start),
            b'>' => self.op_gt(start),
            b'&' => self.op_amp(start),
            b'|' => self.op_pipe(start),
            b'^' => self.op_caret(start),
            b'?' => self.op_question(start),

            _ => self.bad_char(start),
        }
    }

    /// Emit an `Error` token covering one full UTF-8 character
    /// starting at `start`. Multi-byte safe — single-byte advance
    /// would split a code point and panic on the next `&str` slice.
    fn bad_char(&mut self, start: usize) {
        let ch = self.peek_char().unwrap_or('\0');
        let n = ch.len_utf8().max(1);
        self.pos += n;
        let span = self.span(start, self.pos);
        self.diagnostics.push(Diagnostic::error(
            codes::UNEXPECTED_CHAR,
            span,
            format!("unexpected character: {ch:?}"),
        ));
        self.tokens.push(Token::new(SyntaxKind::Error, span));
    }

    // ---- Tiny cursor / token helpers used by every submodule ----

    /// Decode the UTF-8 character at the cursor without advancing.
    ///
    /// Fast-path ASCII without touching `from_utf8` at all; for
    /// multi-byte sequences validate at most 4 bytes (the UTF-8
    /// max). The previous implementation passed the entire
    /// remaining slice to `from_utf8`, which made `ident_or_keyword`
    /// quadratic over file length.
    pub(crate) fn peek_char(&self) -> Option<char> {
        let bytes = &self.text[self.pos..];
        let b = *bytes.first()?;
        if b < 0x80 {
            return Some(b as char);
        }
        // Decode *exactly* the character at the cursor. The UTF-8 sequence
        // length is fixed by the lead byte; validating a wider window (e.g. a
        // flat 4-byte slice) can straddle into the *next* character and fail
        // `from_utf8` even though the char at the cursor is valid — which used
        // to drop a multi-byte char onto the single-byte `bad_char` path,
        // splitting it and producing non-char-aligned spans (a downstream
        // `&str` slice then panics).
        let len = match b {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            // A continuation or otherwise invalid lead byte: not a decodable
            // char here, so `from_utf8` of the single byte fails → `None`.
            _ => 1,
        };
        if bytes.len() < len {
            return None;
        }
        std::str::from_utf8(&bytes[..len])
            .ok()
            .and_then(|s| s.chars().next())
    }

    pub(crate) fn peek_at(&self, offset: usize) -> Option<u8> {
        self.text.get(self.pos + offset).copied()
    }

    pub(crate) fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.source, leek_span::offset(start), leek_span::offset(end))
    }

    /// Emit a single-byte token and advance one byte.
    pub(crate) fn single(&mut self, kind: SyntaxKind, start: usize) {
        self.pos += 1;
        self.tokens
            .push(Token::new(kind, self.span(start, self.pos)));
    }

    /// Emit a token of `len` bytes starting at `start`. The cursor
    /// is set to `start + len`.
    pub(crate) fn push(&mut self, kind: SyntaxKind, start: usize, len: usize) {
        self.pos = start + len;
        self.tokens
            .push(Token::new(kind, self.span(start, self.pos)));
    }

    /// Branchy single-char operator with optional follow-up; used
    /// by `/`.
    fn op_one(
        &mut self,
        single_kind: SyntaxKind,
        start: usize,
        follow: impl Fn(u8) -> Option<(SyntaxKind, usize)>,
    ) {
        if let Some(c) = self.peek_at(1)
            && let Some((k, len)) = follow(c)
        {
            self.push(k, start, len);
            return;
        }
        self.single(single_kind, start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_syntax::SyntaxKind as S;

    fn lex_kinds(text: &str) -> Vec<S> {
        let src = SourceId::new(1).unwrap();
        let result = lex(text, src, Version::LATEST);
        result
            .tokens
            .iter()
            .map(|t| t.kind)
            .filter(|k| !k.is_trivia() && *k != S::Eof)
            .collect()
    }

    #[test]
    fn empty_input_just_eof() {
        let src = SourceId::new(1).unwrap();
        let result = lex("", src, Version::LATEST);
        assert_eq!(result.tokens.len(), 1);
        assert_eq!(result.tokens[0].kind, S::Eof);
    }

    #[test]
    fn whitespace_is_trivia() {
        let src = SourceId::new(1).unwrap();
        let result = lex("   \t\n  ", src, Version::LATEST);
        assert_eq!(result.tokens[0].kind, S::Whitespace);
        assert_eq!(result.tokens[1].kind, S::Eof);
    }

    #[test]
    fn ident_vs_keyword() {
        assert_eq!(lex_kinds("var"), [S::KwVar]);
        assert_eq!(lex_kinds("foo"), [S::Ident]);
        assert_eq!(lex_kinds("_underscore"), [S::Ident]);
    }

    #[test]
    fn number_literal_int_and_real() {
        assert_eq!(lex_kinds("42"), [S::IntLiteral]);
        assert_eq!(lex_kinds("3.14"), [S::RealLiteral]);
        assert_eq!(lex_kinds("1e3"), [S::RealLiteral]);
        // 1..10 is NOT 1.0, it's int + dotdot + int
        assert_eq!(
            lex_kinds("1..10"),
            [S::IntLiteral, S::DotDot, S::IntLiteral]
        );
        // Trailing-dot reals: `0.` is the literal `0.0`. The dot is
        // part of the number when it isn't `..` (range) and isn't
        // followed by an identifier (`0.foo` = member access).
        assert_eq!(lex_kinds("0."), [S::RealLiteral]);
        assert_eq!(lex_kinds("42."), [S::RealLiteral]);
        assert_eq!(lex_kinds("0.]"), [S::RealLiteral, S::RBracket]);
        assert_eq!(lex_kinds("0.,"), [S::RealLiteral, S::Comma]);
        // Member access on an int literal stays int + dot + ident.
        assert_eq!(lex_kinds("0.foo"), [S::IntLiteral, S::Dot, S::Ident]);
    }

    #[test]
    fn string_literals() {
        assert_eq!(lex_kinds("\"hello\""), [S::StringLiteral]);
        assert_eq!(lex_kinds("'single'"), [S::StringLiteral]);
        assert_eq!(lex_kinds("\"with \\\" escape\""), [S::StringLiteral]);
    }

    #[test]
    fn unterminated_string_diag() {
        let src = SourceId::new(1).unwrap();
        let result = lex("\"oops", src, Version::LATEST);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, codes::STRING_NOT_CLOSED);
    }

    #[test]
    fn line_comment_consumed_as_trivia() {
        let src = SourceId::new(1).unwrap();
        let result = lex("// note\nvar", src, Version::LATEST);
        let kinds: Vec<_> = result.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, [S::LineComment, S::Whitespace, S::KwVar, S::Eof]);
    }

    #[test]
    fn multichar_operators() {
        assert_eq!(lex_kinds("==="), [S::EqEqEq]);
        assert_eq!(lex_kinds("=="), [S::EqEq]);
        assert_eq!(lex_kinds("="), [S::Eq]);
        assert_eq!(lex_kinds("!=="), [S::NotEqEq]);
        assert_eq!(lex_kinds("!="), [S::NotEq]);
        assert_eq!(lex_kinds("**="), [S::StarStarEq]);
        assert_eq!(lex_kinds("**"), [S::StarStar]);
        assert_eq!(lex_kinds("??="), [S::QuestionQuestionEq]);
        assert_eq!(lex_kinds("??"), [S::QuestionQuestion]);
        assert_eq!(lex_kinds("->"), [S::Arrow]);
        assert_eq!(lex_kinds("=>"), [S::FatArrow]);
        assert_eq!(lex_kinds(".."), [S::DotDot]);
    }

    #[test]
    fn shift_operators() {
        assert_eq!(lex_kinds("<<"), [S::ShiftLeft]);
        assert_eq!(lex_kinds("<<="), [S::ShiftLeftEq]);
        assert_eq!(lex_kinds(">>"), [S::ShiftRight]);
        assert_eq!(lex_kinds(">>="), [S::ShiftRightEq]);
        assert_eq!(lex_kinds(">>>"), [S::UShiftRight]);
        assert_eq!(lex_kinds(">>>="), [S::UShiftRightEq]);
        // Adjacent comparison must still parse correctly.
        assert_eq!(lex_kinds("< <"), [S::Lt, S::Lt]);
        assert_eq!(lex_kinds("<= <="), [S::Le, S::Le]);
    }

    #[test]
    fn hex_and_binary_literals() {
        assert_eq!(lex_kinds("0xff"), [S::IntLiteral]);
        assert_eq!(lex_kinds("0xDEADBEEF"), [S::IntLiteral]);
        assert_eq!(lex_kinds("0b1010"), [S::IntLiteral]);
        assert_eq!(lex_kinds("0X1A"), [S::IntLiteral]);
        assert_eq!(lex_kinds("0B11"), [S::IntLiteral]);
    }

    #[test]
    fn invalid_hex_digit_diags() {
        let src = SourceId::new(1).unwrap();
        let r = lex("0b102", src, Version::LATEST);
        assert_eq!(r.diagnostics.len(), 1);
        assert_eq!(r.diagnostics[0].code, codes::INVALID_NUMBER);
        // Still emits exactly one IntLiteral token covering the
        // full span.
        let non_eof: Vec<_> = r.tokens.iter().filter(|t| t.kind != S::Eof).collect();
        assert_eq!(non_eof.len(), 1);
        assert_eq!(non_eof[0].kind, S::IntLiteral);
    }

    #[test]
    fn version_gating_for_keywords() {
        // `class` is not a keyword in v1.
        let src = SourceId::new(1).unwrap();
        let v1 = lex("class", src, Version::V1);
        let v2 = lex("class", src, Version::V2);
        assert_eq!(v1.tokens[0].kind, S::Ident);
        assert_eq!(v2.tokens[0].kind, S::KwClass);

        // `switch` requires v3.
        let v2 = lex("switch", src, Version::V2);
        let v3 = lex("switch", src, Version::V3);
        assert_eq!(v2.tokens[0].kind, S::Ident);
        assert_eq!(v3.tokens[0].kind, S::KwSwitch);
    }

    #[test]
    fn library_leek_fixture() {
        // Mirrors `official-generator/.../ai/library.leek`.
        let src = r"
function arrayKeys(array) {
    var keys = [];
    for (var k : var _ in array) push(keys, k);
    return keys;
}
";
        let kinds = lex_kinds(src);
        assert_eq!(
            kinds,
            [
                S::KwFunction,
                S::Ident,
                S::LParen,
                S::Ident,
                S::RParen,
                S::LBrace,
                S::KwVar,
                S::Ident,
                S::Eq,
                S::LBracket,
                S::RBracket,
                S::Semicolon,
                S::KwFor,
                S::LParen,
                S::KwVar,
                S::Ident,
                S::Colon,
                S::KwVar,
                S::Ident,
                S::KwIn,
                S::Ident,
                S::RParen,
                S::Ident,
                S::LParen,
                S::Ident,
                S::Comma,
                S::Ident,
                S::RParen,
                S::Semicolon,
                S::KwReturn,
                S::Ident,
                S::Semicolon,
                S::RBrace,
            ]
        );
    }
}
