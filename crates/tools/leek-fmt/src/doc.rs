//! Doc IR — the intermediate representation the per-construct
//! formatters build and the printer consumes.
//!
//! Modeled on Wadler / Prettier. Each [`Doc`] node represents either
//! literal text or a layout decision; a [`Doc::Group`] tells the
//! printer "try to fit this on one line; if not, switch every
//! [`Doc::Line`] / [`Doc::SoftLine`] inside me to a newline".

use std::borrow::Cow;

use crate::FormatOptions;

#[derive(Debug, Clone)]
pub enum Doc {
    Nil,
    Text(Cow<'static, str>),
    /// `" "` in flat mode, newline+indent in broken mode.
    Line,
    /// `""` in flat mode, newline+indent in broken mode.
    SoftLine,
    /// Always a newline+indent.
    HardLine,
    /// Newline+blank-line+indent. Always breaks the enclosing group.
    BlankLine,
    /// Adjust indentation by `n` columns inside the doc.
    Indent(isize, Box<Doc>),
    /// Try flat; fall back to broken if flat overflows.
    Group(Box<Doc>),
    /// Pick `flat` in flat mode, `broken` in broken mode. Useful for
    /// trailing-comma behavior.
    IfBreak {
        flat: Box<Doc>,
        broken: Box<Doc>,
    },
    Concat(Vec<Doc>),
    /// Swap the printer's active [`FormatOptions`] for the duration
    /// of `inner`. Lets `// fmt: push indent = 2` change print-time
    /// settings (indent width, indent style, max line length) for a
    /// region without rebuilding the whole document.
    WithOptions(Box<FormatOptions>, Box<Doc>),
}

impl Doc {
    pub fn is_nil(&self) -> bool {
        matches!(self, Doc::Nil)
    }
}

// ---- Helper constructors ----

pub fn nil() -> Doc {
    Doc::Nil
}

pub fn text(s: impl Into<Cow<'static, str>>) -> Doc {
    Doc::Text(s.into())
}

pub fn line() -> Doc {
    Doc::Line
}

pub fn softline() -> Doc {
    Doc::SoftLine
}

pub fn hardline() -> Doc {
    Doc::HardLine
}

pub fn blank_line() -> Doc {
    Doc::BlankLine
}

pub fn indent(n: isize, d: Doc) -> Doc {
    Doc::Indent(n, Box::new(d))
}

pub fn group(d: Doc) -> Doc {
    Doc::Group(Box::new(d))
}

pub fn ifbreak(flat: Doc, broken: Doc) -> Doc {
    Doc::IfBreak {
        flat: Box::new(flat),
        broken: Box::new(broken),
    }
}

/// Wrap `inner` so the printer applies `opts` for its duration.
/// Equivalent to `inner` when `opts` matches the surrounding
/// settings; the helper exists to keep callers from instantiating
/// the variant directly.
pub fn with_options(opts: FormatOptions, inner: Doc) -> Doc {
    Doc::WithOptions(Box::new(opts), Box::new(inner))
}

pub fn concat<I: IntoIterator<Item = Doc>>(items: I) -> Doc {
    let v: Vec<Doc> = items.into_iter().filter(|d| !d.is_nil()).collect();
    if v.is_empty() {
        Doc::Nil
    } else if v.len() == 1 {
        v.into_iter().next().unwrap()
    } else {
        Doc::Concat(v)
    }
}

/// Join `items` with `sep` between each consecutive pair.
pub fn join<I: IntoIterator<Item = Doc>>(sep: &Doc, items: I) -> Doc {
    let mut out: Vec<Doc> = Vec::new();
    for (i, d) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(sep.clone());
        }
        out.push(d);
    }
    concat(out)
}

/// `a + " " + b` shortcut (always a space, never breaks).
pub fn space() -> Doc {
    Doc::Text(Cow::Borrowed(" "))
}
