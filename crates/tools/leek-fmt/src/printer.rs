//! Doc IR printer.
//!
//! Standard Wadler/Lindig algorithm. A queue of `(indent, mode, doc)`
//! triples is consumed left-to-right; on each `Group`, [`fits`]
//! peeks ahead to decide flat vs. broken mode.
//!
//! ## Per-region options
//!
//! [`Doc::WithOptions`] regions let `// fmt: push indent = 2`-style
//! pragmas change print-time settings (indent width, indent style,
//! max line length) for part of the document. The printer maintains
//! a stack of active option snapshots — entering a `WithOptions`
//! frame pushes; the matching exit pops. Exits are tracked by
//! pushing an internal `PopMarker` frame onto the main work stack
//! after the inner doc, so the pop happens at the natural moment
//! the region's last work item is consumed.

use leek_span::SourceId;
use leek_syntax::{SyntaxKind, Version};

use crate::doc::Doc;
use crate::{FormatOptions, IndentStyle};

/// Longest fragment either side of a boundary that [`needs_separator`]
/// will look at. Every token that can munch with a neighbour is an
/// identifier, a number or an operator, all far shorter than this; the
/// cap only stops a pathological unbroken line from making the check
/// quadratic.
const FRAGMENT_CAP: usize = 64;

/// Characters an identifier, keyword or number is built from. Two of
/// them either side of a boundary can merge into one token
/// (`not` + `true` → `nottrue`).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Characters operators are built from. Two of them either side of a
/// boundary can merge (`<` + `=>` → `<=` `>`).
fn is_op_char(c: char) -> bool {
    matches!(
        c,
        '<' | '='
            | '>'
            | '+'
            | '-'
            | '*'
            | '/'
            | '%'
            | '!'
            | '&'
            | '|'
            | '^'
            | '~'
            | '?'
            | ':'
            | '.'
    )
}

/// Cheap pre-filter: could these two characters possibly belong to the
/// same token? Everything else — a bracket, a comma, a quote, any
/// whitespace — is a token boundary no matter what sits next to it, so
/// the boundary needs no lexing at all.
fn may_merge(left: char, right: char) -> bool {
    (is_word_char(left) && is_word_char(right))
        || (is_op_char(left) && is_op_char(right))
        // A digit and a `.` merge into a real literal (`5` + `.` → `5.`).
        || (is_word_char(left) && right == '.')
        || (left == '.' && is_word_char(right))
}

/// The trailing slice of `out` that could belong to its last token:
/// the maximal run of characters in the same class as the final one.
///
/// Taking a class-run rather than a fixed-size suffix is what keeps
/// this safe — a run of word or operator characters can never start
/// inside a string literal or a comment (quotes and letters end the
/// respective runs), so the fragment always lexes the way its tail
/// does in the full document.
fn trailing_fragment(out: &str) -> &str {
    let Some(last) = out.chars().next_back() else {
        return "";
    };
    // `.` joins the word class so a real literal (`1.5`) stays whole;
    // splitting it would make `1.5` look like a bare `5`.
    let wordish = is_word_char(last) || last == '.';
    let mut start = out.len();
    for (i, c) in out.char_indices().rev().take(FRAGMENT_CAP) {
        let in_class = if wordish {
            is_word_char(c) || c == '.'
        } else {
            is_op_char(c)
        };
        if !in_class {
            break;
        }
        start = i;
    }
    &out[start..]
}

/// Mirror of [`trailing_fragment`] for the text about to be emitted.
fn leading_fragment(s: &str) -> &str {
    let Some(first) = s.chars().next() else {
        return "";
    };
    let wordish = is_word_char(first) || first == '.';
    let mut end = 0;
    for (i, c) in s.char_indices().take(FRAGMENT_CAP) {
        let in_class = if wordish {
            is_word_char(c) || c == '.'
        } else {
            is_op_char(c)
        };
        if !in_class {
            break;
        }
        end = i + c.len_utf8();
    }
    &s[..end]
}

/// Lex `s` into `(kind, text)` pairs, dropping the terminating `Eof`,
/// plus how many diagnostics say the *fragment itself* is malformed.
///
/// An unterminated block comment contributes nothing here, and must
/// not: a fragment is a maximal run of operator or word characters, so
/// the leading fragment of a `/* … */` comment is the bare `/*` —
/// unclosed on its own however well-formed the comment is in the
/// document. Counting it would make [`needs_separator`] bail on a
/// boundary it has to answer: printing `/` next to a block comment with
/// no space produces `//*`, which re-lexes as a line comment and
/// swallows the rest of the line. The lexer no longer reports the
/// unclosed form at all (upstream accepts it — #351), so no filter is
/// needed; every diagnostic that does reach here describes damage a
/// separator cannot repair.
fn tokens_of(s: &str, version: Version) -> (Vec<(SyntaxKind, &str)>, usize) {
    let src = SourceId::new(1).expect("1 is a valid SourceId");
    let res = leek_lexer::lex(s, src, version);
    let toks = res
        .tokens
        .iter()
        .filter(|t| t.kind != SyntaxKind::Eof)
        .map(|t| (t.kind, &s[t.span.range()]))
        .collect();
    (toks, res.diagnostics.len())
}

/// Would writing `left` and `right` back to back change how they lex?
///
/// This is the whole of the separator rule: a space goes in exactly
/// when the concatenation does *not* re-lex into the same tokens as
/// the two pieces do apart. Asking the lexer is the only definition of
/// "one token" that cannot drift away from the language — a hand-kept
/// list of keyword/operator pairs would rot on the next new operator.
fn needs_separator(left: &str, right: &str, version: Version) -> bool {
    let (l, l_diags) = tokens_of(left, version);
    let (r, r_diags) = tokens_of(right, version);
    if l.is_empty() || r.is_empty() {
        return false;
    }
    // A fragment that is already malformed on its own — an
    // unterminated string, a stray character — is some other bug's
    // territory (#417). A space would not repair it, so add none.
    if l_diags > 0 || r_diags > 0 {
        return false;
    }
    // A line comment swallows the rest of its line whatever we do.
    if l.iter()
        .any(|(k, _)| matches!(k, SyntaxKind::LineComment | SyntaxKind::StringLiteral))
    {
        return false;
    }

    let mut joined = String::with_capacity(left.len() + right.len());
    joined.push_str(left);
    joined.push_str(right);
    let (j, _) = tokens_of(&joined, version);

    let expected = l.len() + r.len();
    if j.len() != expected {
        return true;
    }
    j.iter()
        .zip(l.iter().chain(r.iter()))
        .any(|((jk, jt), (ek, et))| jk != ek || jt != et)
}

#[cfg(test)]
mod separator_tests {
    use super::{needs_separator, separator_needed, tokens_of};
    use leek_syntax::Version;

    /// The fragment a block comment hands [`separator_needed`] is its
    /// leading operator-character run, `/*` — an unterminated comment
    /// on its own whatever the full comment looks like in the document.
    /// If [`tokens_of`] read that as "this fragment is malformed", the
    /// separator rule would answer "no space" and the printer could
    /// emit `//*`, re-lexing a block comment as a line comment (#140
    /// meeting #412/#417). Since #351 the lexer reports nothing for the
    /// unclosed form, matching upstream — this pins that the count
    /// stays at zero rather than coming back through a new code.
    #[test]
    fn a_block_comment_fragment_is_not_treated_as_malformed() {
        assert_eq!(tokens_of("/*", Version::V4).1, 0);
        assert!(separator_needed("/", "/* c */", Version::V4));
    }

    /// The bail itself still holds for the damage it was written for:
    /// a fragment carrying a real lexer error gets no repairing space.
    #[test]
    fn a_genuinely_malformed_fragment_still_short_circuits() {
        assert!(tokens_of("\u{a7}", Version::V4).1 > 0);
        assert!(!needs_separator("+", "\u{a7}", Version::V4));
    }
}

/// Does a space have to go between what has been printed and `next`?
///
/// Answers "no" without lexing for the overwhelming majority of
/// boundaries: only two adjacent word characters, two adjacent
/// operator characters, or a digit/`.` pair can ever merge.
fn separator_needed(out: &str, next: &str, version: Version) -> bool {
    let (Some(l), Some(r)) = (out.chars().next_back(), next.chars().next()) else {
        return false;
    };
    if !may_merge(l, r) {
        return false;
    }
    needs_separator(trailing_fragment(out), leading_fragment(next), version)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

/// Internal work-item discriminant. Most frames carry a `&Doc`;
/// `PopOptions` is a sentinel inserted alongside the inner doc of
/// a [`Doc::WithOptions`] node so the printer knows when to restore
/// the previous options.
enum Frame<'d> {
    Doc(usize, Mode, &'d Doc),
    PopOptions,
}

/// Render a [`Doc`] to a `String`.
///
/// Indent levels in [`Doc::Indent`] are *levels*, not columns; they
/// are scaled by the active `opts.indent` when emitting whitespace.
///
/// Before each piece of text lands in the buffer, [`separator_needed`]
/// decides whether a space has to go in first so the two sides keep
/// lexing as the tokens they came from (#412). The check is exactly
/// the complement of "this boundary already round-trips", so output
/// that formats correctly today is unchanged byte for byte.
pub fn print(doc: &Doc, version: Version, opts: &FormatOptions) -> String {
    let mut out = String::new();
    let mut col: usize = 0;
    let mut active = opts.clone();
    let mut opts_stack: Vec<FormatOptions> = Vec::new();
    let mut stack: Vec<Frame<'_>> = vec![Frame::Doc(0, Mode::Break, doc)];

    while let Some(frame) = stack.pop() {
        let (lvl, mode, doc) = match frame {
            Frame::PopOptions => {
                if let Some(prev) = opts_stack.pop() {
                    active = prev;
                }
                continue;
            }
            Frame::Doc(l, m, d) => (l, m, d),
        };
        match doc {
            Doc::Nil => {}
            Doc::Text(s) => {
                if separator_needed(&out, s, version) {
                    out.push(' ');
                    col += 1;
                }
                out.push_str(s);
                col += s.chars().count();
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    out.push(' ');
                    col += 1;
                }
                Mode::Break => {
                    col = newline(&mut out, lvl, &active);
                }
            },
            Doc::SoftLine => match mode {
                Mode::Flat => {}
                Mode::Break => {
                    col = newline(&mut out, lvl, &active);
                }
            },
            Doc::HardLine => {
                col = newline(&mut out, lvl, &active);
            }
            Doc::BlankLine => {
                out.push('\n');
                col = newline(&mut out, lvl, &active);
            }
            Doc::Indent(n, inner) => {
                let new_lvl = lvl.checked_add_signed(*n).unwrap_or(0);
                stack.push(Frame::Doc(new_lvl, mode, inner));
            }
            Doc::Group(inner) => {
                let chose = if fits(inner, active.max_line_length.saturating_sub(col)) {
                    Mode::Flat
                } else {
                    Mode::Break
                };
                stack.push(Frame::Doc(lvl, chose, inner));
            }
            Doc::IfBreak { flat, broken } => {
                let pick = match mode {
                    Mode::Flat => flat,
                    Mode::Break => broken,
                };
                stack.push(Frame::Doc(lvl, mode, pick));
            }
            Doc::Concat(items) => {
                // Push in reverse so the first item ends up on top.
                for item in items.iter().rev() {
                    stack.push(Frame::Doc(lvl, mode, item));
                }
            }
            Doc::WithOptions(new_opts, inner) => {
                // Push a pop sentinel BEFORE the inner so that the
                // sentinel fires after all of the inner's frames
                // have been consumed (LIFO).
                opts_stack.push(active.clone());
                active = (**new_opts).clone();
                stack.push(Frame::PopOptions);
                stack.push(Frame::Doc(lvl, mode, inner));
            }
        }
    }

    out
}

/// Emit a `\n` and the indent for the given level. Returns the
/// resulting column position (measured assuming tabs occupy
/// `opts.indent` columns).
fn newline(out: &mut String, lvl: usize, opts: &FormatOptions) -> usize {
    out.push('\n');
    match opts.indent_style {
        IndentStyle::Spaces => {
            let cols = lvl * opts.indent;
            for _ in 0..cols {
                out.push(' ');
            }
            cols
        }
        IndentStyle::Tabs => {
            for _ in 0..lvl {
                out.push('\t');
            }
            lvl * opts.indent
        }
    }
}

/// Cheap "does this doc fit in `width` columns when flat?" check.
///
/// Walks the head of the doc in flat mode, counting characters. Stops
/// as soon as `width` is exceeded or a [`Doc::HardLine`]/
/// [`Doc::BlankLine`] is encountered (those force a break, so the
/// answer becomes "no" — except they may also legitimately end this
/// group's flat measurement; we conservatively report "no").
fn fits(doc: &Doc, width: usize) -> bool {
    let mut budget = isize::try_from(width).unwrap_or(isize::MAX);
    let mut stack: Vec<(Mode, &Doc)> = vec![(Mode::Flat, doc)];
    while let Some((mode, d)) = stack.pop() {
        if budget < 0 {
            return false;
        }
        match d {
            Doc::Nil => {}
            Doc::Text(s) => budget -= isize::try_from(s.chars().count()).unwrap_or(isize::MAX),
            Doc::Line => match mode {
                Mode::Flat => budget -= 1,
                Mode::Break => return true, // line breaks reset width
            },
            Doc::SoftLine => match mode {
                Mode::Flat => {}
                Mode::Break => return true,
            },
            Doc::HardLine | Doc::BlankLine => return true,
            Doc::Indent(_, inner) => stack.push((mode, inner)),
            Doc::Group(inner) => stack.push((Mode::Flat, inner)),
            Doc::IfBreak { flat, broken } => {
                let pick = match mode {
                    Mode::Flat => flat,
                    Mode::Break => broken,
                };
                stack.push((mode, pick));
            }
            Doc::Concat(items) => {
                for item in items.iter().rev() {
                    stack.push((mode, item));
                }
            }
            // WithOptions doesn't change the structural width
            // measurement — just descend the inner doc with the
            // surrounding mode.
            Doc::WithOptions(_, inner) => stack.push((mode, inner)),
        }
    }
    budget >= 0
}
