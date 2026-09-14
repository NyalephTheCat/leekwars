//! Formatter configuration — the `[format]` table.
//!
//! Lives here (not in `leek-fmt`) so that `leek-manifest` can be the
//! single owner of the manifest schema and `leek-fmt` can stay a pure
//! pretty-printer with no TOML dependency of its own.
//! Defaults match `doc/manifest.md` §3.
use crate::error::{ManifestError, ManifestErrorKind, span_of};
use crate::parse::{bool_val, string_val, table_val, wrong_type};
use toml_edit::{Item, TableLike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// Number of columns per indent level (when [`indent_style`] is
    /// [`IndentStyle::Spaces`]). Used for width measurement when
    /// [`indent_style`] is [`IndentStyle::Tabs`].
    ///
    /// [`indent_style`]: FormatOptions::indent_style
    pub indent: usize,

    /// Soft line-length budget. The printer breaks `Group`s whose
    /// flat layout would overflow this column count.
    pub max_line_length: usize,

    /// Whether each indent level is emitted as spaces or as a tab.
    pub indent_style: IndentStyle,

    /// How to handle trailing commas in multi-line collection /
    /// argument lists.
    pub trailing_comma: TrailingComma,

    /// Maximum number of consecutive blank lines to preserve between
    /// items. `0` collapses every blank-line run to nothing.
    pub max_blank_lines: usize,

    /// Insert a space between a function name and its opening `(`.
    pub space_before_call_paren: bool,

    /// Pad the inside of collection literals: `[ 1, 2 ]` / `{ 1, 2 }`
    /// instead of `[1, 2]`. Only affects the flat (single-line) layout;
    /// a broken collection always puts elements on their own lines.
    pub space_inside_brackets: bool,

    /// Pad the inside of call argument lists and parenthesised
    /// expressions: `f( a, b )` and `( a + b )` instead of `f(a, b)` /
    /// `(a + b)`. Flat layout only.
    pub space_inside_parens: bool,

    /// Where the opening brace of a block (function/class body, control
    /// statement body) goes: on the header's line (K&R) or the next
    /// line (Allman).
    pub brace_style: BraceStyle,

    /// Emit a space after each comma in element lists (arguments,
    /// arrays, sets, maps, parameters, multi-variable declarations):
    /// `[1, 2]` vs `[1,2]`.
    pub space_after_comma: bool,

    /// Emit a space between a control keyword and its `(`:
    /// `if (x)` / `while (x)` / `for (…)` vs `if(x)`.
    pub space_after_control_keyword: bool,

    /// Pad the `->` / `=>` arrows of lambdas and return types with
    /// spaces: `x -> x + 1` and `-> integer` vs `x->x + 1`.
    pub space_around_arrow: bool,

    /// Emit a space *before* the `:` in map / object entries:
    /// `[k : v]` vs `[k: v]`.
    pub space_before_colon: bool,

    /// Emit a space *after* the `:` in map / object entries:
    /// `[k: v]` vs `[k:v]`.
    pub space_after_colon: bool,

    /// Normalize line comments to have a space after `//`: `//x` becomes
    /// `// x`. Leaves doc comments (`///`, `//!`) and already-spaced
    /// comments untouched.
    pub pad_line_comments: bool,

    /// Normalize string-literal quotes: `preserve` keeps the source
    /// form, `double` rewrites `'…'` to `"…"`, `single` the reverse.
    /// Escapes are adjusted (`\'` ↔ `\"`) so the literal's value never
    /// changes.
    pub quote_style: QuoteStyle,

    /// Line terminator for formatted output. Existing `\r\n` in the
    /// source (including `// fmt: off` regions) is normalized too —
    /// line endings are a whole-file property.
    pub line_ending: LineEnding,

    /// When a binary expression breaks across lines, does the operator
    /// stay at the end of the first line (`trailing`, default) or move
    /// to the start of the continuation line (`leading`)?
    pub operator_position: OperatorPosition,

    /// Minimum number of `.member` links in a call chain before the
    /// formatter is allowed to break the chain one-call-per-line when
    /// it overflows. `0` disables chain breaking entirely.
    pub method_chain_threshold: usize,

    /// Force a blank line between a function/class declaration (or
    /// class method/constructor) and its neighbouring items. Has no
    /// effect when [`max_blank_lines`] is `0`.
    ///
    /// [`max_blank_lines`]: FormatOptions::max_blank_lines
    pub blank_line_between_functions: bool,

    /// Brace policy for single-statement control bodies
    /// (`if`/`else`/`while`/`for` …): `preserve` keeps the source,
    /// `always` adds braces, `never` drops them around a lone simple
    /// statement (expression/`return`/`break`/`continue` — never a
    /// nested control statement, so dangling-`else` can't change
    /// meaning).
    pub control_braces: ControlBraces,

    /// Remove redundant parentheses: `((x))` → `(x)`, `(f(a))` → `f(a)`,
    /// `if ((cond))` → `if (cond)`, `return (expr);` → `return expr;`.
    /// Only parens that provably can't affect parsing are removed.
    pub remove_redundant_parens: bool,

    /// Statement-terminator policy: `preserve` keeps the source,
    /// `always` appends the optional `;` to statements that lack one.
    /// (There is deliberately no `never` — removing `;` can merge
    /// adjacent statements.)
    pub semicolons: Semicolons,

    /// Collapse `else { if (…) … }` into `else if (…) …` when the block
    /// holds exactly that one `if` (and no comments). Semantics are
    /// identical either way.
    pub collapse_else_if: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    Spaces,
    Tabs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailingComma {
    Preserve,
    Always,
    Never,
}

/// Opening-brace placement for blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BraceStyle {
    /// K&R: `function f() {` — brace on the header's line (default).
    SameLine,
    /// Allman: `function f()` then `{` on its own line.
    NextLine,
}

/// String-literal quote normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteStyle {
    /// Keep whatever the source used (default).
    Preserve,
    /// Rewrite to `"…"`.
    Double,
    /// Rewrite to `'…'`.
    Single,
}

/// Output line terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n` (default).
    Lf,
    /// `\r\n`.
    Crlf,
}

/// Operator placement when a binary expression breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorPosition {
    /// `a +\n    b` — operator ends the first line (default).
    Trailing,
    /// `a\n    + b` — operator starts the continuation line.
    Leading,
}

/// Brace policy for single-statement control bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlBraces {
    /// Keep the source's choice (default).
    Preserve,
    /// Wrap unbraced bodies in `{ … }`.
    Always,
    /// Unwrap `{ … }` around a lone simple statement.
    Never,
}

/// Statement-terminator (`;`) policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Semicolons {
    /// Keep the source's choice (default).
    Preserve,
    /// Append the optional `;` where missing.
    Always,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent: 4,
            max_line_length: 100,
            indent_style: IndentStyle::Spaces,
            trailing_comma: TrailingComma::Preserve,
            max_blank_lines: 1,
            space_before_call_paren: false,
            space_inside_brackets: false,
            space_inside_parens: false,
            brace_style: BraceStyle::SameLine,
            space_after_comma: true,
            space_after_control_keyword: true,
            space_around_arrow: true,
            space_before_colon: false,
            space_after_colon: true,
            pad_line_comments: false,
            quote_style: QuoteStyle::Preserve,
            line_ending: LineEnding::Lf,
            operator_position: OperatorPosition::Trailing,
            method_chain_threshold: 3,
            blank_line_between_functions: false,
            control_braces: ControlBraces::Preserve,
            remove_redundant_parens: false,
            semicolons: Semicolons::Preserve,
            collapse_else_if: false,
        }
    }
}

impl FormatOptions {
    /// Parse the `[format]` table out of a TOML source string.
    ///
    /// Unknown keys are ignored; missing keys fall back to
    /// [`Default::default`]. Returns the default options if the
    /// document has no `[format]` table at all.
    ///
    /// This is a convenience for callers that only want formatter
    /// options (e.g. `leekc --fmt-config`). For the full manifest,
    /// use [`super::load_str`] / [`super::load_from`].
    pub fn from_toml_str(s: &str) -> Result<Self, ManifestError> {
        let doc = toml_edit::ImDocument::parse(s).map_err(|e| {
            ManifestError::at(
                span_of(e.span()),
                ManifestErrorKind::Toml {
                    message: e.to_string(),
                },
            )
        })?;
        let Some(fmt) = doc.as_table().get("format") else {
            return Ok(Self::default());
        };
        Self::from_toml_table(table_val(fmt, "format")?)
    }

    /// Parse a `[format]` table given the already-extracted TOML
    /// table. Used by both [`from_toml_str`] and the full manifest
    /// loader.
    ///
    /// These are [`ManifestError`]s, not [`FormatOptionError`]s: the key came
    /// out of a `Miku.toml`, so it has a span there and belongs with the rest
    /// of the manifest's diagnostics. `FormatOptionError` is for the other
    /// caller — [`set`](Self::set), where the text is a `// fmt:` pragma in a
    /// `.leek` file and the span belongs to that comment.
    pub(crate) fn from_toml_table(tbl: &dyn TableLike) -> Result<Self, ManifestError> {
        let mut opts = Self::default();
        for (key, val) in tbl.iter() {
            match key {
                "indent" => opts.indent = expect_u(val, "indent")?,
                "max_line_length" => {
                    opts.max_line_length = expect_u(val, "max_line_length")?;
                }
                "indent_style" => {
                    opts.indent_style = one_of(
                        val,
                        "indent_style",
                        "\"spaces\" or \"tabs\"",
                        &[("spaces", IndentStyle::Spaces), ("tabs", IndentStyle::Tabs)],
                    )?;
                }
                "trailing_comma" => {
                    opts.trailing_comma = one_of(
                        val,
                        "trailing_comma",
                        "\"preserve\"/\"always\"/\"never\"",
                        &[
                            ("preserve", TrailingComma::Preserve),
                            ("always", TrailingComma::Always),
                            ("never", TrailingComma::Never),
                        ],
                    )?;
                }
                "max_blank_lines" => {
                    opts.max_blank_lines = expect_u(val, "max_blank_lines")?;
                }
                "space_before_call_paren" => {
                    opts.space_before_call_paren = expect_bool(val, "space_before_call_paren")?;
                }
                "space_inside_brackets" => {
                    opts.space_inside_brackets = expect_bool(val, "space_inside_brackets")?;
                }
                "space_inside_parens" => {
                    opts.space_inside_parens = expect_bool(val, "space_inside_parens")?;
                }
                "brace_style" => {
                    opts.brace_style = one_of(
                        val,
                        "brace_style",
                        "\"same_line\" or \"next_line\"",
                        &[
                            ("same_line", BraceStyle::SameLine),
                            ("next_line", BraceStyle::NextLine),
                        ],
                    )?;
                }
                "space_after_comma" => {
                    opts.space_after_comma = expect_bool(val, "space_after_comma")?;
                }
                "space_after_control_keyword" => {
                    opts.space_after_control_keyword =
                        expect_bool(val, "space_after_control_keyword")?;
                }
                "space_around_arrow" => {
                    opts.space_around_arrow = expect_bool(val, "space_around_arrow")?;
                }
                "space_before_colon" => {
                    opts.space_before_colon = expect_bool(val, "space_before_colon")?;
                }
                "space_after_colon" => {
                    opts.space_after_colon = expect_bool(val, "space_after_colon")?;
                }
                "pad_line_comments" => {
                    opts.pad_line_comments = expect_bool(val, "pad_line_comments")?;
                }
                "quote_style" => {
                    opts.quote_style = one_of(val, "quote_style", QUOTE_STYLE, QUOTE_STYLES)?;
                }
                "line_ending" => {
                    opts.line_ending = one_of(val, "line_ending", LINE_ENDING, LINE_ENDINGS)?;
                }
                "operator_position" => {
                    opts.operator_position = one_of(
                        val,
                        "operator_position",
                        OPERATOR_POSITION,
                        OPERATOR_POSITIONS,
                    )?;
                }
                "method_chain_threshold" => {
                    opts.method_chain_threshold = expect_u(val, "method_chain_threshold")?;
                }
                "blank_line_between_functions" => {
                    opts.blank_line_between_functions =
                        expect_bool(val, "blank_line_between_functions")?;
                }
                "control_braces" => {
                    opts.control_braces =
                        one_of(val, "control_braces", CONTROL_BRACES, CONTROL_BRACES_VALUES)?;
                }
                "remove_redundant_parens" => {
                    opts.remove_redundant_parens = expect_bool(val, "remove_redundant_parens")?;
                }
                "semicolons" => {
                    opts.semicolons = one_of(val, "semicolons", SEMICOLONS, SEMICOLONS_VALUES)?;
                }
                "collapse_else_if" => {
                    opts.collapse_else_if = expect_bool(val, "collapse_else_if")?;
                }
                _ => {}
            }
        }
        Ok(opts)
    }

    /// Mutate one option in place, parsing `value` as the right type
    /// for `key`. Returns `Err` for unknown keys or unparseable values.
    /// Used by `leek-fmt`'s `// fmt: <key> = <value>` pragma.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), FormatOptionError> {
        match key {
            "indent" => self.indent = parse_uint(key, value)?,
            "max_line_length" => self.max_line_length = parse_uint(key, value)?,
            "max_blank_lines" => self.max_blank_lines = parse_uint(key, value)?,
            "method_chain_threshold" => self.method_chain_threshold = parse_uint(key, value)?,
            "blank_line_between_functions" => {
                self.blank_line_between_functions = parse_bool(key, value)?;
            }
            "remove_redundant_parens" => self.remove_redundant_parens = parse_bool(key, value)?,
            "collapse_else_if" => self.collapse_else_if = parse_bool(key, value)?,
            "quote_style" => self.quote_style = pick(key, value, QUOTE_STYLE, QUOTE_STYLES)?,
            "line_ending" => self.line_ending = pick(key, value, LINE_ENDING, LINE_ENDINGS)?,
            "operator_position" => {
                self.operator_position = pick(key, value, OPERATOR_POSITION, OPERATOR_POSITIONS)?;
            }
            "control_braces" => {
                self.control_braces = pick(key, value, CONTROL_BRACES, CONTROL_BRACES_VALUES)?;
            }
            "semicolons" => self.semicolons = pick(key, value, SEMICOLONS, SEMICOLONS_VALUES)?,
            "space_before_call_paren" => {
                self.space_before_call_paren = parse_bool(key, value)?;
            }
            "space_inside_brackets" => self.space_inside_brackets = parse_bool(key, value)?,
            "space_inside_parens" => self.space_inside_parens = parse_bool(key, value)?,
            "space_after_comma" => self.space_after_comma = parse_bool(key, value)?,
            "space_after_control_keyword" => {
                self.space_after_control_keyword = parse_bool(key, value)?;
            }
            "space_around_arrow" => self.space_around_arrow = parse_bool(key, value)?,
            "space_before_colon" => self.space_before_colon = parse_bool(key, value)?,
            "space_after_colon" => self.space_after_colon = parse_bool(key, value)?,
            "pad_line_comments" => self.pad_line_comments = parse_bool(key, value)?,
            "brace_style" => {
                self.brace_style = pick(
                    key,
                    value,
                    "\"same_line\" or \"next_line\"",
                    &[
                        ("same_line", BraceStyle::SameLine),
                        ("next_line", BraceStyle::NextLine),
                    ],
                )?;
            }
            "indent_style" => {
                self.indent_style = pick(
                    key,
                    value,
                    "\"spaces\" or \"tabs\"",
                    &[("spaces", IndentStyle::Spaces), ("tabs", IndentStyle::Tabs)],
                )?;
            }
            "trailing_comma" => {
                self.trailing_comma = pick(
                    key,
                    value,
                    "\"preserve\"/\"always\"/\"never\"",
                    &[
                        ("preserve", TrailingComma::Preserve),
                        ("always", TrailingComma::Always),
                        ("never", TrailingComma::Never),
                    ],
                )?;
            }
            other => {
                return Err(FormatOptionError::UnknownOption {
                    key: other.to_string(),
                });
            }
        }
        Ok(())
    }
}

/// A `// fmt: <key> = <value>` pragma this formatter can't act on.
///
/// Deliberately carries no [`Span`](leek_span::Span): the text it failed on
/// lives in a `.leek` comment, and only `leek-fmt` — which owns that comment's
/// range — can say where. The error names the key and the accepted values so
/// the caller can build the diagnostic with its own span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatOptionError {
    /// No formatter option by that name.
    UnknownOption { key: String },
    /// The option exists, but the text isn't a value it accepts.
    BadValue {
        key: String,
        /// The accepted values, spelled as the message wants them.
        expected: &'static str,
        got: String,
    },
}

impl std::fmt::Display for FormatOptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatOptionError::UnknownOption { key } => write!(f, "unknown option {key:?}"),
            FormatOptionError::BadValue { key, expected, got } => {
                write!(f, "{key}: expected {expected}, got {got:?}")
            }
        }
    }
}

impl std::error::Error for FormatOptionError {}

// The accepted spellings of each enum-valued option, shared by the TOML
// walker and the pragma setter so the two can never drift.
const QUOTE_STYLE: &str = "\"preserve\"/\"double\"/\"single\"";
const QUOTE_STYLES: &[(&str, QuoteStyle)] = &[
    ("preserve", QuoteStyle::Preserve),
    ("double", QuoteStyle::Double),
    ("single", QuoteStyle::Single),
];

const LINE_ENDING: &str = "\"lf\" or \"crlf\"";
const LINE_ENDINGS: &[(&str, LineEnding)] = &[("lf", LineEnding::Lf), ("crlf", LineEnding::Crlf)];

const OPERATOR_POSITION: &str = "\"trailing\" or \"leading\"";
const OPERATOR_POSITIONS: &[(&str, OperatorPosition)] = &[
    ("trailing", OperatorPosition::Trailing),
    ("leading", OperatorPosition::Leading),
];

const CONTROL_BRACES: &str = "\"preserve\"/\"always\"/\"never\"";
const CONTROL_BRACES_VALUES: &[(&str, ControlBraces)] = &[
    ("preserve", ControlBraces::Preserve),
    ("always", ControlBraces::Always),
    ("never", ControlBraces::Never),
];

const SEMICOLONS: &str = "\"preserve\" or \"always\"";
const SEMICOLONS_VALUES: &[(&str, Semicolons)] = &[
    ("preserve", Semicolons::Preserve),
    ("always", Semicolons::Always),
];

/// Look `raw` up in an option's accepted spellings — the pragma path.
fn pick<T: Copy>(
    key: &str,
    raw: &str,
    expected: &'static str,
    accepted: &[(&str, T)],
) -> Result<T, FormatOptionError> {
    accepted
        .iter()
        .find(|(name, _)| *name == raw)
        .map(|(_, v)| *v)
        .ok_or_else(|| FormatOptionError::BadValue {
            key: key.to_string(),
            expected,
            got: raw.to_string(),
        })
}

/// Look a TOML string value up in an option's accepted spellings — the
/// manifest path, which has a span to point at.
fn one_of<T: Copy>(
    item: &Item,
    key: &str,
    expected: &str,
    accepted: &[(&str, T)],
) -> Result<T, ManifestError> {
    let raw = string_val(item, key)?;
    accepted
        .iter()
        .find(|(name, _)| *name == raw)
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            ManifestError::at(
                span_of(item.span()),
                ManifestErrorKind::BadValue {
                    key: key.to_string(),
                    expected: expected.to_string(),
                    got: Some(format!("{raw:?}")),
                },
            )
        })
}

fn parse_uint(key: &str, raw: &str) -> Result<usize, FormatOptionError> {
    raw.parse::<usize>()
        .map_err(|_| FormatOptionError::BadValue {
            key: key.to_string(),
            expected: "non-negative integer",
            got: raw.to_string(),
        })
}

fn parse_bool(key: &str, raw: &str) -> Result<bool, FormatOptionError> {
    match raw {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(FormatOptionError::BadValue {
            key: key.to_string(),
            expected: "boolean",
            got: other.to_string(),
        }),
    }
}

fn expect_u(item: &Item, key: &str) -> Result<usize, ManifestError> {
    item.as_integer()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| wrong_type(item, key, "a non-negative integer"))
}

fn expect_bool(item: &Item, key: &str) -> Result<bool, ManifestError> {
    bool_val(item, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_manifest() {
        let o = FormatOptions::default();
        assert_eq!(o.indent, 4);
        assert_eq!(o.max_line_length, 100);
        assert_eq!(o.indent_style, IndentStyle::Spaces);
        assert_eq!(o.trailing_comma, TrailingComma::Preserve);
        assert_eq!(o.max_blank_lines, 1);
        assert!(!o.space_before_call_paren);
    }

    #[test]
    fn empty_toml_yields_defaults() {
        let o = FormatOptions::from_toml_str("").unwrap();
        assert_eq!(o, FormatOptions::default());
    }

    #[test]
    fn parses_known_keys() {
        let src = r#"
            [format]
            indent = 2
            max_line_length = 80
            indent_style = "tabs"
            trailing_comma = "always"
            max_blank_lines = 2
            space_before_call_paren = true
        "#;
        let o = FormatOptions::from_toml_str(src).unwrap();
        assert_eq!(o.indent, 2);
        assert_eq!(o.max_line_length, 80);
        assert_eq!(o.indent_style, IndentStyle::Tabs);
        assert_eq!(o.trailing_comma, TrailingComma::Always);
        assert_eq!(o.max_blank_lines, 2);
        assert!(o.space_before_call_paren);
    }

    #[test]
    fn rejects_unknown_enum_value() {
        let src = r#"[format]
indent_style = "kebabs"
"#;
        assert!(FormatOptions::from_toml_str(src).is_err());
    }

    #[test]
    fn parses_brace_style() {
        let src = "[format]\nbrace_style = \"next_line\"\n";
        let o = FormatOptions::from_toml_str(src).unwrap();
        assert_eq!(o.brace_style, BraceStyle::NextLine);
        // Default stays SameLine when unset.
        assert_eq!(FormatOptions::default().brace_style, BraceStyle::SameLine);
    }

    #[test]
    fn rejects_bad_brace_style() {
        let src = "[format]\nbrace_style = \"same-line\"\n";
        assert!(FormatOptions::from_toml_str(src).is_err());
    }

    #[test]
    fn parses_spacing_options() {
        let src = "[format]\nspace_inside_brackets = true\nspace_inside_parens = true\n";
        let o = FormatOptions::from_toml_str(src).unwrap();
        assert!(o.space_inside_brackets);
        assert!(o.space_inside_parens);
        // Defaults are off.
        let d = FormatOptions::default();
        assert!(!d.space_inside_brackets);
        assert!(!d.space_inside_parens);
    }

    #[test]
    fn set_pragma_handles_new_options() {
        let mut o = FormatOptions::default();
        o.set("brace_style", "next_line").unwrap();
        assert_eq!(o.brace_style, BraceStyle::NextLine);
        o.set("space_inside_brackets", "true").unwrap();
        assert!(o.space_inside_brackets);
        assert!(o.set("brace_style", "bogus").is_err());
    }

    #[test]
    fn parses_spacing_and_comment_options() {
        let src = "[format]\n\
            space_after_comma = false\n\
            space_after_control_keyword = false\n\
            space_around_arrow = false\n\
            space_before_colon = true\n\
            space_after_colon = false\n\
            pad_line_comments = true\n";
        let o = FormatOptions::from_toml_str(src).unwrap();
        assert!(!o.space_after_comma);
        assert!(!o.space_after_control_keyword);
        assert!(!o.space_around_arrow);
        assert!(o.space_before_colon);
        assert!(!o.space_after_colon);
        assert!(o.pad_line_comments);
    }

    #[test]
    fn new_spacing_defaults() {
        let d = FormatOptions::default();
        assert!(d.space_after_comma);
        assert!(d.space_after_control_keyword);
        assert!(d.space_around_arrow);
        assert!(!d.space_before_colon);
        assert!(d.space_after_colon);
        assert!(!d.pad_line_comments);
    }

    #[test]
    fn set_pragma_handles_spacing_options() {
        let mut o = FormatOptions::default();
        o.set("space_after_comma", "false").unwrap();
        o.set("pad_line_comments", "true").unwrap();
        assert!(!o.space_after_comma);
        assert!(o.pad_line_comments);
    }

    #[test]
    fn parses_layout_and_rewrite_options() {
        let src = "[format]\n\
            quote_style = \"double\"\n\
            line_ending = \"crlf\"\n\
            operator_position = \"leading\"\n\
            method_chain_threshold = 5\n\
            blank_line_between_functions = true\n\
            control_braces = \"always\"\n\
            remove_redundant_parens = true\n\
            semicolons = \"always\"\n\
            collapse_else_if = true\n";
        let o = FormatOptions::from_toml_str(src).unwrap();
        assert_eq!(o.quote_style, QuoteStyle::Double);
        assert_eq!(o.line_ending, LineEnding::Crlf);
        assert_eq!(o.operator_position, OperatorPosition::Leading);
        assert_eq!(o.method_chain_threshold, 5);
        assert!(o.blank_line_between_functions);
        assert_eq!(o.control_braces, ControlBraces::Always);
        assert!(o.remove_redundant_parens);
        assert_eq!(o.semicolons, Semicolons::Always);
        assert!(o.collapse_else_if);
    }

    #[test]
    fn layout_and_rewrite_defaults() {
        let d = FormatOptions::default();
        assert_eq!(d.quote_style, QuoteStyle::Preserve);
        assert_eq!(d.line_ending, LineEnding::Lf);
        assert_eq!(d.operator_position, OperatorPosition::Trailing);
        assert_eq!(d.method_chain_threshold, 3);
        assert!(!d.blank_line_between_functions);
        assert_eq!(d.control_braces, ControlBraces::Preserve);
        assert!(!d.remove_redundant_parens);
        assert_eq!(d.semicolons, Semicolons::Preserve);
        assert!(!d.collapse_else_if);
    }

    #[test]
    fn set_pragma_handles_layout_and_rewrite_options() {
        let mut o = FormatOptions::default();
        o.set("quote_style", "single").unwrap();
        o.set("operator_position", "leading").unwrap();
        o.set("control_braces", "never").unwrap();
        o.set("semicolons", "always").unwrap();
        o.set("method_chain_threshold", "0").unwrap();
        assert_eq!(o.quote_style, QuoteStyle::Single);
        assert_eq!(o.operator_position, OperatorPosition::Leading);
        assert_eq!(o.control_braces, ControlBraces::Never);
        assert_eq!(o.semicolons, Semicolons::Always);
        assert_eq!(o.method_chain_threshold, 0);
        assert!(o.set("quote_style", "fancy").is_err());
        assert!(o.set("semicolons", "never").is_err());
    }

    #[test]
    fn rejects_bad_layout_enum_values() {
        for src in [
            "[format]\nquote_style = \"smart\"\n",
            "[format]\nline_ending = \"cr\"\n",
            "[format]\noperator_position = \"middle\"\n",
            "[format]\ncontrol_braces = \"sometimes\"\n",
            "[format]\nsemicolons = \"never\"\n",
        ] {
            assert!(FormatOptions::from_toml_str(src).is_err(), "{src}");
        }
    }

    #[test]
    fn ignores_unknown_keys() {
        let src = r#"[format]
some_future_knob = "v9"
"#;
        FormatOptions::from_toml_str(src).unwrap();
    }
}
