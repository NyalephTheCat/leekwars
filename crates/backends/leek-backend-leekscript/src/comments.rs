//! Comment preservation.
//!
//! The HIR drops comments, but every node carries a source [`Span`]. We
//! re-lex the original source to recover the comment trivia, then flush
//! comments by byte position at statement / block / item boundaries so
//! they land back in their original order. Exact original whitespace is
//! not recoverable from the normalized HIR, so comments are re-emitted on
//! their own line at the current indentation. Compact mode drops them
//! entirely (the side-table is simply never built).

use leek_lexer::lex;
use leek_span::SourceId;
use leek_syntax::{SyntaxKind, Version};

use crate::writer::LsWriter;

struct CommentTok {
    start: u32,
    text: String,
    block: bool,
}

pub(crate) struct Comments {
    items: Vec<CommentTok>,
    cursor: usize,
}

impl Comments {
    /// Build the comment side-table from `text`. Returns an empty table
    /// when there is no text (compact mode passes `None`).
    pub(crate) fn build(text: Option<&str>, source: SourceId, version: Version) -> Self {
        let mut items = Vec::new();
        if let Some(text) = text {
            for tok in lex(text, source, version).tokens {
                let block = match tok.kind {
                    SyntaxKind::LineComment => false,
                    SyntaxKind::BlockComment => true,
                    _ => continue,
                };
                let raw = &text[tok.span.range()];
                items.push(CommentTok {
                    start: tok.span.start,
                    text: raw.trim_end().to_string(),
                    block,
                });
            }
        }
        Self { items, cursor: 0 }
    }

    /// Emit every not-yet-flushed comment that begins before `pos`, each
    /// on its own line at the current indentation.
    pub(crate) fn flush_before(&mut self, w: &mut LsWriter, pos: u32) {
        while self.cursor < self.items.len() && self.items[self.cursor].start < pos {
            let c = &self.items[self.cursor];
            if c.block {
                // Block comments may span lines; emit verbatim.
                for (i, line) in c.text.lines().enumerate() {
                    if i > 0 {
                        w.newline();
                    }
                    w.token(line.trim_start());
                }
            } else {
                w.token(&c.text);
            }
            w.newline();
            self.cursor += 1;
        }
    }

    /// Flush any remaining comments (after the last node, at EOF).
    pub(crate) fn flush_rest(&mut self, w: &mut LsWriter) {
        self.flush_before(w, u32::MAX);
    }
}
