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
//!
//! ## Raw text
//!
//! [`Doc::Verbatim`] holds source the formatter reproduces rather
//! than lays out, so unlike [`Doc::Text`] it can carry its own
//! newlines. The printer therefore re-bases `col` on the run's last
//! line instead of adding the whole run's width, and [`fits`] treats
//! a run that breaks its own line the way it treats a hard line
//! (#198).

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

/// How many doc nodes past the end of a group [`fits`] will measure
/// before answering from the budget it has counted so far.
///
/// Measuring the rest of the line is unbounded work in principle — the
/// stack it walks holds the whole remainder of the document — so, like
/// [`FRAGMENT_CAP`], this is a stated cap rather than a guess at a
/// typical size. In practice the walk ends long before it: the first
/// line break past the group ends the line and the measurement, and
/// every statement is followed by one, so the cap only bounds a
/// pathological document that puts hundreds of nodes after a group on
/// a single line. Reaching it answers from the budget, which is what
/// running out of document does too.
const REST_NODE_CAP: usize = 256;

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
            Doc::Verbatim(s) => {
                if separator_needed(&out, s, version) {
                    out.push(' ');
                    col += 1;
                }
                out.push_str(s);
                // A raw run carries its own newlines, so the column
                // for what follows is the width of its last line, not
                // the width of the whole run (#198).
                col = match s.rsplit_once('\n') {
                    Some((_, tail)) => tail.chars().count(),
                    None => col + s.chars().count(),
                };
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
                // `stack` is the rest of the document: what the printer
                // will emit after this group, and so part of the line
                // the group has to fit on.
                let width = active.max_line_length.saturating_sub(col);
                let chose = if fits(inner, &stack, width) {
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

/// Take the next *document* frame from the printer's work stack,
/// stepping backwards from `*next` and skipping the frames that carry
/// no doc (the `PopOptions` sentinels). The stack is LIFO, so reading
/// it back to front reads the document forwards — the order [`fits`]
/// has to measure the rest of the line in.
fn next_rest_doc<'d>(rest: &[Frame<'d>], next: &mut usize) -> Option<(Mode, &'d Doc)> {
    while *next > 0 {
        *next -= 1;
        if let Frame::Doc(_, mode, doc) = &rest[*next] {
            return Some((*mode, *doc));
        }
    }
    None
}

/// Does the group `doc` fit in `width` columns when printed flat —
/// *including* whatever the printer will put after it on the same line?
///
/// Walks the group's own content in flat mode counting characters, then
/// keeps counting through `rest`, the printer's remaining work stack,
/// until something ends the line. Measuring that rest is what keeps
/// output inside `max_line_length`: the `;` closing a `var` statement,
/// the `) {` closing an `if` header and the trailing operand of a
/// binary expression all sit outside the group they follow, so a group
/// measured on its own can be chosen flat and then overflow by exactly
/// those few columns (#199).
///
/// The rest is measured with each frame's *recorded* mode rather than
/// flat — those frames already had their layout decided — and that is
/// also what ends the walk: a [`Doc::Line`]/[`Doc::SoftLine`] in break
/// mode, or any [`Doc::HardLine`]/[`Doc::BlankLine`], ends the line, so
/// the budget was never exhausted and the group does fit. Failing that,
/// [`REST_NODE_CAP`] bounds the walk.
///
/// Inside the group's own content a [`Doc::HardLine`] or
/// [`Doc::BlankLine`] instead answers "no", and that is not a
/// heuristic: flat mode has no effect on either node — the printer
/// emits their newline whatever mode it is in — so a group containing
/// one cannot be printed on a single line however much room is left. A
/// block-bodied lambda or a nested block inside an argument list is
/// exactly that case; choosing `Mode::Flat` for its group would join
/// the argument separators onto one line around a body that still
/// breaks (#199).
///
/// A [`Doc::Verbatim`] whose raw text carries a newline is the same
/// case for the same reason — flat mode cannot take a newline out of
/// source the formatter has promised to reproduce — with one
/// difference past the group: there the run's first newline ends the
/// line, so only the text in front of it is charged to the budget,
/// exactly as a hard line ends the walk with the budget intact
/// (#198).
fn fits(doc: &Doc, rest: &[Frame<'_>], width: usize) -> bool {
    let mut budget = isize::try_from(width).unwrap_or(isize::MAX);
    let mut stack: Vec<(Mode, &Doc)> = vec![(Mode::Flat, doc)];
    // Where `next_rest_doc` resumes, and whether the walk has left the
    // group's own content. `in_rest` only ever turns on: the rest is
    // entered when `stack` runs dry, so everything pushed after that
    // came out of the rest too.
    let mut next = rest.len();
    let mut in_rest = false;
    let mut rest_nodes: usize = 0;
    loop {
        if budget < 0 {
            return false;
        }
        let Some((mode, d)) = stack.pop() else {
            // The group's own content is measured; carry on through
            // what the printer will emit after it on the same line.
            let Some(frame) = next_rest_doc(rest, &mut next) else {
                break;
            };
            in_rest = true;
            stack.push(frame);
            continue;
        };
        if in_rest {
            rest_nodes += 1;
            if rest_nodes > REST_NODE_CAP {
                break;
            }
        }
        match d {
            Doc::Nil => {}
            Doc::Text(s) => budget -= isize::try_from(s.chars().count()).unwrap_or(isize::MAX),
            // A run that breaks its own line cannot be flattened, so
            // inside the group it answers "no" the way a hard line
            // does. Past the group it ends the line instead: only the
            // text in front of its first newline is spent on the line
            // being measured (#198).
            Doc::Verbatim(s) => match s.split_once('\n') {
                Some((head, _)) => {
                    if !in_rest {
                        return false;
                    }
                    budget -= isize::try_from(head.chars().count()).unwrap_or(isize::MAX);
                    return budget >= 0;
                }
                None => budget -= isize::try_from(s.chars().count()).unwrap_or(isize::MAX),
            },
            Doc::Line => match mode {
                Mode::Flat => budget -= 1,
                Mode::Break => return true, // line breaks reset width
            },
            Doc::SoftLine => match mode {
                Mode::Flat => {}
                Mode::Break => return true,
            },
            // Past the group its newline ends the line — and the
            // measurement — with the budget intact; inside the group it
            // means the group can never be a single line.
            Doc::HardLine | Doc::BlankLine => return in_rest,
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
