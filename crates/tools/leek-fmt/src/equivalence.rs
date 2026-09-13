//! Formatter safety net: token and comment equivalence.
//!
//! The formatter rebuilds source text from the CST, so a per-construct
//! formatter that forgets a token or a comment silently deletes user
//! code. [`check_equivalence`] re-lexes the input and the output and
//! verifies that nothing was lost. Callers that write formatted text
//! back (`miku fmt`, the LSP formatting handlers) run it first and
//! refuse to apply output that fails it.
//!
//! "Equivalent" deliberately tolerates the rewrites the formatter is
//! allowed to make:
//!
//! - whitespace and line endings;
//! - `;`, `,`, `(` / `)` and `{` / `}` — optional semicolons, trailing
//!   commas, redundant parentheses and control-statement braces;
//! - string-literal quote style (literals are compared in a canonical
//!   double-quoted form);
//! - keyword spelling (keywords are compared by kind);
//! - comment layout — `//x` → `// x` padding and re-indented block
//!   comment continuation lines;
//! - `// fmt: …` pragma comments, which the formatter consumes.
//!
//! Every other token must survive, in order, and the multiset of
//! comments must be unchanged.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use leek_span::SourceId;
use leek_syntax::{SyntaxKind as S, Version};

use crate::{FmtPragma, QuoteStyle};

/// Why formatted output was judged unsafe to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EquivalenceError {
    /// Comments present in the input are missing from the output
    /// (`missing`), or the output has comments the input lacks
    /// (`unexpected`). Entries are normalized comment bodies.
    CommentMismatch {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    /// The significant token streams diverge at the `index`-th compared
    /// token. `None` means that stream ended first.
    TokenMismatch {
        index: usize,
        original: Option<String>,
        formatted: Option<String>,
    },
    /// An edit range passed to [`check_edit_equivalence`] does not lie
    /// on character boundaries inside the original text.
    EditOutOfRange { range: Range<u32>, len: usize },
}

impl fmt::Display for EquivalenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CommentMismatch {
                missing,
                unexpected,
            } => {
                if !missing.is_empty() {
                    write!(
                        f,
                        "formatting would drop {} comment(s): {}",
                        missing.len(),
                        preview_list(missing)
                    )?;
                }
                if !unexpected.is_empty() {
                    if !missing.is_empty() {
                        f.write_str("; ")?;
                    }
                    write!(
                        f,
                        "formatting would introduce {} comment(s): {}",
                        unexpected.len(),
                        preview_list(unexpected)
                    )?;
                }
                Ok(())
            }
            Self::TokenMismatch {
                index,
                original,
                formatted,
            } => write!(
                f,
                "formatting would change token #{index}: `{}` becomes `{}`",
                original.as_deref().unwrap_or("<end of file>"),
                formatted.as_deref().unwrap_or("<end of file>"),
            ),
            Self::EditOutOfRange { range, len } => write!(
                f,
                "edit range {}..{} is outside the {len}-byte document",
                range.start, range.end
            ),
        }
    }
}

impl std::error::Error for EquivalenceError {}

/// Verify that `formatted` is a faithful reformatting of `original`:
/// same comments (as a multiset) and the same significant tokens, up to
/// the layout-only rewrites listed in the [module docs](self).
///
/// Both texts are lexed with `version`, which must be the language
/// version the formatter parsed `original` with.
pub fn check_equivalence(
    original: &str,
    formatted: &str,
    version: Version,
) -> Result<(), EquivalenceError> {
    let before = Summary::of(original, version);
    let after = Summary::of(formatted, version);
    compare_comments(&before.comments, &after.comments)?;
    compare_tokens(&before.tokens, &after.tokens)
}

/// [`check_equivalence`] for a partial edit: replaces the byte `range`
/// of `original` with `replacement` and checks the resulting document
/// against `original`. Used by range / on-type formatting, whose
/// replacement text is only a fragment.
pub fn check_edit_equivalence(
    original: &str,
    range: Range<u32>,
    replacement: &str,
    version: Version,
) -> Result<(), EquivalenceError> {
    let out_of_range = || EquivalenceError::EditOutOfRange {
        range: range.clone(),
        len: original.len(),
    };
    let start = usize::try_from(range.start).map_err(|_| out_of_range())?;
    let end = usize::try_from(range.end).map_err(|_| out_of_range())?;
    let (Some(head), Some(tail)) = (original.get(..start), original.get(end..)) else {
        return Err(out_of_range());
    };
    if start > end {
        return Err(out_of_range());
    }
    let edited = [head, replacement, tail].concat();
    check_equivalence(original, &edited, version)
}

/// The comparable content of one text.
struct Summary {
    /// Normalized significant tokens, in source order.
    tokens: Vec<String>,
    /// Normalized comment bodies (pragmas excluded), in source order.
    comments: Vec<String>,
}

impl Summary {
    fn of(text: &str, version: Version) -> Self {
        // The source id only labels lexer diagnostics, which are unused.
        let source = SourceId::new(1).expect("1 is a valid source id");
        let lexed = leek_lexer::lex(text, source, version);
        let mut tokens = Vec::new();
        let mut comments = Vec::new();
        for tok in &lexed.tokens {
            let raw = text.get(tok.span.range()).unwrap_or_default();
            match tok.kind {
                S::Whitespace | S::Eof => {}
                S::LineComment | S::BlockComment => {
                    if crate::parse_fmt_pragma(raw) == FmtPragma::None {
                        comments.push(comment_key(raw));
                    }
                }
                // Layout punctuation the formatter may add or remove.
                S::Semicolon | S::Comma | S::LParen | S::RParen | S::LBrace | S::RBrace => {}
                S::StringLiteral => {
                    tokens.push(crate::format::normalize_quotes(raw, QuoteStyle::Double));
                }
                kind if kind.is_keyword() => tokens.push(format!("{kind:?}")),
                _ => tokens.push(raw.to_owned()),
            }
        }
        Self { tokens, comments }
    }
}

/// Normalize a comment so layout-only changes compare equal: line
/// comments lose `//` and surrounding whitespace (`//x` ≡ `// x`);
/// block comments are compared line by line with each line trimmed
/// and one leading `*` stripped (doc-comment re-indentation).
fn comment_key(raw: &str) -> String {
    if let Some(body) = raw.strip_prefix("//") {
        return body.trim().to_owned();
    }
    raw.lines()
        .map(|line| {
            let line = line.trim();
            line.strip_prefix('*').map_or(line, str::trim_start)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compare_comments(before: &[String], after: &[String]) -> Result<(), EquivalenceError> {
    let missing = unmatched(before, after);
    let unexpected = unmatched(after, before);
    if missing.is_empty() && unexpected.is_empty() {
        Ok(())
    } else {
        Err(EquivalenceError::CommentMismatch {
            missing,
            unexpected,
        })
    }
}

/// Entries of `from` left over after pairing each with an equal entry
/// of `against` (multiset difference, preserving `from`'s order).
fn unmatched(from: &[String], against: &[String]) -> Vec<String> {
    let mut available: BTreeMap<&str, usize> = BTreeMap::new();
    for c in against {
        *available.entry(c).or_default() += 1;
    }
    from.iter()
        .filter(|c| match available.get_mut(c.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .cloned()
        .collect()
}

fn compare_tokens(before: &[String], after: &[String]) -> Result<(), EquivalenceError> {
    let index = before
        .iter()
        .zip(after)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| before.len().min(after.len()));
    if index == before.len() && index == after.len() {
        return Ok(());
    }
    Err(EquivalenceError::TokenMismatch {
        index,
        original: before.get(index).cloned(),
        formatted: after.get(index).cloned(),
    })
}

/// Render up to three entries as short one-line previews.
fn preview_list(items: &[String]) -> String {
    const SHOWN: usize = 3;
    const WIDTH: usize = 40;
    let mut out: Vec<String> = items
        .iter()
        .take(SHOWN)
        .map(|item| {
            let line = item.lines().next().unwrap_or_default();
            let short: String = line.chars().take(WIDTH).collect();
            if short.len() < item.len() {
                format!("`{short}…`")
            } else {
                format!("`{short}`")
            }
        })
        .collect();
    if items.len() > SHOWN {
        out.push(format!("and {} more", items.len() - SHOWN));
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(before: &str, after: &str) -> Result<(), EquivalenceError> {
        check_equivalence(before, after, Version::V4)
    }

    #[test]
    fn detects_a_dropped_comment() {
        let err = check("var x = 1 + // why\n    2\n", "var x = 1 + 2\n").unwrap_err();
        assert_eq!(
            err,
            EquivalenceError::CommentMismatch {
                missing: vec!["why".to_owned()],
                unexpected: Vec::new(),
            }
        );
    }

    #[test]
    fn detects_a_dropped_token() {
        let err = check("var x = a + b\n", "var x = a\n").unwrap_err();
        assert!(
            // `var`, `x`, `=`, `a` match; `+` is the first loss.
            matches!(err, EquivalenceError::TokenMismatch { index: 4, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn detects_a_commented_out_token() {
        // A `;` swallowed by a line comment changes the comment text.
        let err = check("var x = 1 // c\n;\n", "var x = 1 // c;\n").unwrap_err();
        assert!(
            matches!(err, EquivalenceError::CommentMismatch { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn tolerates_layout_rewrites() {
        let before =
            "var s = 'it\\'s'\nif(x)return (y)//hi\n/**\n   *doc\n */\nfunction f(a,b,){}\n";
        let after = "var s = \"it's\";\nif (x) {\n    return y; // hi\n}\n/**\n *doc\n */\nfunction f(a, b) {}\n";
        check(before, after).unwrap();
    }

    #[test]
    fn ignores_fmt_pragma_comments() {
        check("// fmt: indent = 2\nvar x = 1\n", "var x = 1\n").unwrap();
    }

    #[test]
    fn edit_is_checked_against_the_whole_document() {
        let src = "var a = 1 /* c */ + 2\nvar b = 3\n";
        check_edit_equivalence(src, 0..21, "var a = 1 /* c */ + 2", Version::V4).unwrap();
        let err = check_edit_equivalence(src, 0..21, "var a = 1 + 2", Version::V4).unwrap_err();
        assert!(matches!(err, EquivalenceError::CommentMismatch { .. }));
        let err = check_edit_equivalence(src, 0..999, "", Version::V4).unwrap_err();
        assert!(matches!(err, EquivalenceError::EditOutOfRange { .. }));
    }
}
