//! Formatter safety net: comment, token, parse and shape equivalence.
//!
//! The formatter rebuilds source text from the CST, so a per-construct
//! formatter that forgets a token or a comment silently deletes user
//! code — and one that drops a brace or a parenthesis silently changes
//! what the code *means*. [`check_equivalence`] re-parses the input and
//! the output and verifies that neither happened. Callers that write
//! formatted text back (`miku fmt`, `leekc --emit fmt`, the LSP
//! formatting handlers) run it first and refuse to apply output that
//! fails it.
//!
//! Four layers, cheapest first:
//!
//! 1. the multiset of comments is unchanged;
//! 2. the significant token stream is unchanged, in order;
//! 3. formatting introduced no parse error into text that parsed
//!    cleanly;
//! 4. the *shape* — the nesting of CST nodes around those tokens — is
//!    unchanged.
//!
//! Layer 2 on its own is structure-blind: `(a + b) * c` and
//! `a + b * c` have the same significant tokens once the layout
//! punctuation the formatter may add or remove is dropped, and so do
//! `if (x) { a(); b(); }` and `if (x) a(); b();`. Layer 4 is what makes
//! those visible.
//!
//! "Equivalent" deliberately tolerates the rewrites the formatter is
//! allowed to make:
//!
//! - whitespace and line endings;
//! - `;`, `,`, `(` / `)` and `{` / `}` — optional semicolons, trailing
//!   commas, redundant parentheses and control-statement braces. The
//!   shape, not the punctuation, decides whether removing one changed
//!   the program;
//! - string-literal quote style (literals are compared in a canonical
//!   double-quoted form);
//! - keyword spelling (keywords are compared by kind);
//! - comment layout — `//x` → `// x` padding and re-indented block
//!   comment continuation lines.
//!
//! `// fmt: …` pragma comments are *not* exempt: the formatter keeps
//! them in its output like any other comment (#367).
//!
//! Text that already fails to parse gets layers 1 and 2 only — its CST
//! is whatever error recovery produced, so comparing shapes would
//! measure the recovery rather than the formatter.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use leek_diagnostics::Severity;
use leek_span::SourceId;
use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken, Version};

use crate::QuoteStyle;

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
    /// The output no longer parses cleanly, though the input did.
    ParseRegression { errors: Vec<String> },
    /// The tokens survived but the tree around them did not: the shape
    /// summaries diverge at the `index`-th step. `None` means that
    /// summary ended first.
    ShapeMismatch {
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
            Self::ParseRegression { errors } => write!(
                f,
                "formatting would introduce {} parse error(s): {}",
                errors.len(),
                preview_list(errors)
            ),
            Self::ShapeMismatch {
                index,
                original,
                formatted,
            } => write!(
                f,
                "formatting would change the parse tree at step #{index}: {} becomes {}",
                original.as_deref().unwrap_or("<end of tree>"),
                formatted.as_deref().unwrap_or("<end of tree>"),
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
/// the same comments (as a multiset), the same significant tokens, no
/// new parse errors and the same tree shape around those tokens, up to
/// the layout-only rewrites listed in the [module docs](self).
///
/// Both texts are parsed with `version`, which must be the language
/// version the formatter parsed `original` with.
pub fn check_equivalence(
    original: &str,
    formatted: &str,
    version: Version,
) -> Result<(), EquivalenceError> {
    let before = Summary::of(original, version);
    let after = Summary::of(formatted, version);
    compare_comments(&before.comments, &after.comments)?;
    compare_tokens(&before.tokens, &after.tokens)?;
    if !before.parse_errors.is_empty() {
        // Garbage in, garbage out: the input's own tree is error
        // recovery, so its shape says nothing about the formatter.
        return Ok(());
    }
    if !after.parse_errors.is_empty() {
        return Err(EquivalenceError::ParseRegression {
            errors: after.parse_errors,
        });
    }
    compare_shapes(&before.shape, &after.shape)
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

/// One step of a [`Summary::shape`] walk: entering a CST node, leaving
/// it, or a significant token inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ShapeItem {
    Enter(S),
    Leave,
    Token(String),
}

impl ShapeItem {
    /// One-line rendering for [`EquivalenceError::ShapeMismatch`].
    fn label(&self) -> String {
        match self {
            Self::Enter(kind) => format!("enter {kind:?}"),
            Self::Leave => "leave".to_owned(),
            Self::Token(t) => format!("token `{t}`"),
        }
    }
}

/// The comparable content of one text.
struct Summary {
    /// Normalized significant tokens, in source order.
    tokens: Vec<String>,
    /// Normalized comment bodies, in source order.
    comments: Vec<String>,
    /// The same tokens with normalized node boundaries between them.
    shape: Vec<ShapeItem>,
    /// Messages of the error-severity parse diagnostics, if any.
    parse_errors: Vec<String>,
}

impl Summary {
    fn of(text: &str, version: Version) -> Self {
        // The source id only labels diagnostics, whose spans are unused.
        let source = SourceId::new(1).expect("1 is a valid source id");
        // Same env read as `format_source`: the safety net has to parse
        // the same grammar the formatter did, experimental toggles and
        // all, or it would report a spurious token mismatch.
        let parsed = leek_parser::parse_with_features(
            text,
            source,
            version,
            leek_parser::ParseFeatures::from_env(),
        );
        let mut summary = Self {
            tokens: Vec::new(),
            comments: Vec::new(),
            shape: Vec::new(),
            parse_errors: parsed
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.clone())
                .collect(),
        };
        summary.walk(&SyntaxNode::new_root(parsed.green));
        summary
    }

    /// Append `node` and its descendants to the summary, normalizing
    /// away the tree edits the formatter is allowed to make:
    ///
    /// - a `ParenExpr` is transparent, so `remove_redundant_parens` is
    ///   invisible — but the operand nesting the parens *caused* stays
    ///   in the tree, so peeling a load-bearing paren still shows up;
    /// - a control statement's bare body is wrapped in a synthetic
    ///   `Block`, so `control_braces` (and `collapse_else_if`, which
    ///   only unwraps such a block) is invisible — but a brace that
    ///   takes a sibling statement with it is not.
    fn walk(&mut self, node: &SyntaxNode) {
        let transparent = node.kind() == S::ParenExpr;
        if !transparent {
            self.shape.push(ShapeItem::Enter(node.kind()));
        }
        for el in node.children_with_tokens() {
            match el {
                NodeOrToken::Token(t) => self.token(&t),
                NodeOrToken::Node(child) => {
                    let implied_block = has_body_slot(node.kind())
                        && is_body_kind(child.kind())
                        && child.kind() != S::Block;
                    if implied_block {
                        self.shape.push(ShapeItem::Enter(S::Block));
                    }
                    self.walk(&child);
                    if implied_block {
                        self.shape.push(ShapeItem::Leave);
                    }
                }
            }
        }
        if !transparent {
            self.shape.push(ShapeItem::Leave);
        }
    }

    /// Record one token: as a comment, as a significant token (in both
    /// `tokens` and `shape`), or not at all.
    fn token(&mut self, t: &SyntaxToken) {
        let raw = t.text();
        let normalized = match t.kind() {
            S::Whitespace | S::Eof => return,
            S::LineComment | S::BlockComment => {
                self.comments.push(comment_key(raw));
                return;
            }
            // Layout punctuation the formatter may add or remove. The
            // shape records what it delimited, so nothing is lost.
            S::Semicolon | S::Comma | S::LParen | S::RParen | S::LBrace | S::RBrace => return,
            S::StringLiteral => crate::format::normalize_quotes(raw, QuoteStyle::Double),
            kind if kind.is_keyword() => format!("{kind:?}"),
            _ => raw.to_owned(),
        };
        self.shape.push(ShapeItem::Token(normalized.clone()));
        self.tokens.push(normalized);
    }
}

/// Statement kinds that can stand as a control statement's body, and so
/// may or may not be wrapped in a `Block` depending on `control_braces`.
fn is_body_kind(kind: S) -> bool {
    matches!(
        kind,
        S::Block
            | S::VarDeclStmt
            | S::ExprStmt
            | S::ReturnStmt
            | S::IfStmt
            | S::WhileStmt
            | S::DoWhileStmt
            | S::ForStmt
            | S::ForeachStmt
            | S::SwitchStmt
            | S::BreakStmt
            | S::ContinueStmt
    )
}

/// Control statements whose statement-shaped children get the synthetic
/// `Block` treatment. A `for` header's initializer is a statement too;
/// wrapping it as well costs nothing, since both sides are wrapped the
/// same way.
fn has_body_slot(kind: S) -> bool {
    matches!(
        kind,
        S::IfStmt | S::WhileStmt | S::DoWhileStmt | S::ForStmt | S::ForeachStmt
    )
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

/// Index of the first entry the two sequences disagree on, or `None`
/// when they are equal. A sequence that ran out early diverges at its
/// own length.
fn first_divergence<T: PartialEq>(before: &[T], after: &[T]) -> Option<usize> {
    let index = before
        .iter()
        .zip(after)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| before.len().min(after.len()));
    (index != before.len() || index != after.len()).then_some(index)
}

fn compare_tokens(before: &[String], after: &[String]) -> Result<(), EquivalenceError> {
    match first_divergence(before, after) {
        None => Ok(()),
        Some(index) => Err(EquivalenceError::TokenMismatch {
            index,
            original: before.get(index).cloned(),
            formatted: after.get(index).cloned(),
        }),
    }
}

fn compare_shapes(before: &[ShapeItem], after: &[ShapeItem]) -> Result<(), EquivalenceError> {
    match first_divergence(before, after) {
        None => Ok(()),
        Some(index) => Err(EquivalenceError::ShapeMismatch {
            index,
            original: before.get(index).map(ShapeItem::label),
            formatted: after.get(index).map(ShapeItem::label),
        }),
    }
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
    fn pragma_comments_must_survive() {
        // The formatter keeps `// fmt: …` markers like any other
        // comment; dropping one means the next run reformats the
        // region the marker was protecting (#367).
        let err = check("// fmt: off\nvar x = 1\n", "var x = 1\n").unwrap_err();
        assert!(
            matches!(err, EquivalenceError::CommentMismatch { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn detects_a_dropped_load_bearing_paren() {
        // Same tokens either way once `(` / `)` are dropped; only the
        // `BinaryExpr` nesting tells them apart.
        let err = check("var x = (a + b) * c\n", "var x = a + b * c\n").unwrap_err();
        assert!(
            matches!(err, EquivalenceError::ShapeMismatch { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn detects_a_dropped_control_brace() {
        // `b()` escapes the `if` body — same tokens, different program.
        let err = check("if (x) { a(); b(); }\n", "if (x) a();\nb();\n").unwrap_err();
        assert!(
            matches!(err, EquivalenceError::ShapeMismatch { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn tolerates_brace_and_paren_rewrites() {
        // `control_braces`, `collapse_else_if` and
        // `remove_redundant_parens` are all shape-neutral.
        check("if (x) a();\n", "if (x) {\n    a();\n}\n").unwrap();
        check(
            "if (x) { a(); } else { if (y) { b(); } }\n",
            "if (x) {\n    a();\n} else if (y) {\n    b();\n}\n",
        )
        .unwrap();
        check("var x = ((a));\n", "var x = a;\n").unwrap();
    }

    #[test]
    fn detects_a_new_parse_error() {
        // Deleting the `)` leaves the same significant tokens (parens
        // are layout) but no longer parses.
        let err = check("var x = f(1);\n", "var x = f(1;\n").unwrap_err();
        assert!(
            matches!(err, EquivalenceError::ParseRegression { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn input_that_does_not_parse_gets_the_token_net_only() {
        // Error recovery decides the shape of broken input, so shape
        // is not compared — the token and comment layers still are.
        let broken = "function f( {\n";
        check(broken, "function f( {\n").unwrap();
        check(broken, "function f({\n").unwrap();
        let err = check(broken, "function g( {\n").unwrap_err();
        assert!(
            matches!(err, EquivalenceError::TokenMismatch { .. }),
            "{err:?}"
        );
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
