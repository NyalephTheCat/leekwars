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

use crate::doc::Doc;
use crate::{FormatOptions, IndentStyle};

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
pub fn print(doc: &Doc, opts: &FormatOptions) -> String {
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
