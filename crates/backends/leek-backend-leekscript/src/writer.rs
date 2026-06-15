//! Output buffer for emitted LeekScript.
//!
//! `LsWriter` accumulates text with two responsibilities:
//!
//! 1. Indentation / newlines in pretty mode (suppressed in compact mode).
//! 2. A **token-merge guard**: whenever two separately-emitted tokens
//!    would fuse into a different token if written adjacently — two
//!    identifier characters (`return` + `x` → `returnx`) or two operator
//!    characters (`-` + `-x` → `--x`) — a single space is inserted. This
//!    keeps compact output correct without the caller reasoning about
//!    every adjacency, and makes the `- -x` / `a - -b` cases fall out for
//!    free.

/// Characters that can begin/continue a multi-character operator token.
/// Two adjacent operator characters from *separate* tokens are always
/// separated, which is safe because real multi-character operators are
/// emitted as one [`LsWriter::token`] call.
fn is_op_char(c: char) -> bool {
    matches!(
        c,
        '+' | '-'
            | '*'
            | '/'
            | '\\'
            | '%'
            | '<'
            | '>'
            | '='
            | '!'
            | '&'
            | '|'
            | '^'
            | '~'
            | '?'
            | ':'
            | '@'
    )
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub(crate) struct LsWriter {
    out: String,
    indent_level: u32,
    indent_unit: String,
    compact: bool,
    /// Last character written, for the token-merge guard.
    last: Option<char>,
    /// True right after a newline so the next `token` emits indentation.
    at_line_start: bool,
}

impl LsWriter {
    pub(crate) fn new(indent_unit: String, compact: bool) -> Self {
        Self {
            out: String::new(),
            indent_level: 0,
            indent_unit,
            compact,
            last: None,
            at_line_start: false,
        }
    }

    fn push_raw(&mut self, s: &str) {
        if let Some(c) = s.chars().last() {
            self.last = Some(c);
        }
        self.out.push_str(s);
    }

    /// Emit a token, guarding against accidental fusion with the previous
    /// token and flushing pending indentation.
    pub(crate) fn token(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.at_line_start {
            self.at_line_start = false;
            for _ in 0..self.indent_level {
                self.out.push_str(&self.indent_unit);
            }
            // Indentation resets the merge state.
            self.last = self.indent_unit.chars().last().or(Some('\n'));
        }
        let first = s.chars().next().unwrap();
        if let Some(prev) = self.last
            && needs_separator(prev, first)
        {
            self.out.push(' ');
            self.last = Some(' ');
        }
        self.push_raw(s);
    }

    /// A space. In pretty mode always emitted; in compact mode dropped
    /// (the merge guard inserts spaces only where required).
    pub(crate) fn space(&mut self) {
        if self.compact {
            return;
        }
        if !self.at_line_start {
            self.out.push(' ');
            self.last = Some(' ');
        }
    }

    /// A hard space that survives compact mode (used after keywords where
    /// the merge guard already covers the common case, but kept explicit
    /// for readability). Equivalent to [`token`] of a space when needed.
    pub(crate) fn newline(&mut self) {
        if self.compact {
            return;
        }
        self.out.push('\n');
        self.last = Some('\n');
        self.at_line_start = true;
    }

    pub(crate) fn indent(&mut self) {
        self.indent_level += 1;
    }

    pub(crate) fn dedent(&mut self) {
        self.indent_level = self.indent_level.saturating_sub(1);
    }

    pub(crate) fn into_string(mut self) -> String {
        if !self.compact && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out
    }
}

fn needs_separator(prev: char, next: char) -> bool {
    (is_word_char(prev) && is_word_char(next)) || (is_op_char(prev) && is_op_char(next))
}
