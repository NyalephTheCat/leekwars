//! Formatter configuration — the `[format]` table.
//!
//! Lives here (not in `leek-fmt`) so that `leek-manifest` can be the
//! single owner of the manifest schema and `leek-fmt` can stay a pure
//! pretty-printer with no TOML dependency of its own.
//! Defaults match `doc/manifest.md` §3.

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
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        let doc: toml::Value = toml::from_str(s).map_err(|e| format!("Miku.toml: {e}"))?;
        let Some(fmt) = doc.get("format") else {
            return Ok(Self::default());
        };
        let tbl = fmt.as_table().ok_or("Miku.toml: `format` is not a table")?;
        Self::from_toml_table(tbl)
    }

    /// Parse a `[format]` table given the already-extracted TOML
    /// table. Used by both [`from_toml_str`] and the full manifest
    /// loader.
    pub(crate) fn from_toml_table(tbl: &toml::value::Table) -> Result<Self, String> {
        let mut opts = Self::default();
        for (key, val) in tbl {
            match key.as_str() {
                "indent" => opts.indent = expect_u(val, "indent")?,
                "max_line_length" => {
                    opts.max_line_length = expect_u(val, "max_line_length")?;
                }
                "indent_style" => match expect_str(val, "indent_style")? {
                    "spaces" => opts.indent_style = IndentStyle::Spaces,
                    "tabs" => opts.indent_style = IndentStyle::Tabs,
                    other => {
                        return Err(format!(
                            "Miku.toml: indent_style must be \"spaces\" or \"tabs\", got {other:?}"
                        ));
                    }
                },
                "trailing_comma" => match expect_str(val, "trailing_comma")? {
                    "preserve" => opts.trailing_comma = TrailingComma::Preserve,
                    "always" => opts.trailing_comma = TrailingComma::Always,
                    "never" => opts.trailing_comma = TrailingComma::Never,
                    other => {
                        return Err(format!(
                            "Miku.toml: trailing_comma must be \"preserve\"/\"always\"/\"never\", got {other:?}"
                        ));
                    }
                },
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
                "brace_style" => match expect_str(val, "brace_style")? {
                    "same_line" => opts.brace_style = BraceStyle::SameLine,
                    "next_line" => opts.brace_style = BraceStyle::NextLine,
                    other => {
                        return Err(format!(
                            "Miku.toml: brace_style must be \"same_line\" or \"next_line\", got {other:?}"
                        ));
                    }
                },
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
                _ => {}
            }
        }
        Ok(opts)
    }

    /// Mutate one option in place, parsing `value` as the right type
    /// for `key`. Returns `Err` for unknown keys or unparseable values.
    /// Used by `leek-fmt`'s `// fmt: <key> = <value>` pragma.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "indent" => self.indent = parse_uint(key, value)?,
            "max_line_length" => self.max_line_length = parse_uint(key, value)?,
            "max_blank_lines" => self.max_blank_lines = parse_uint(key, value)?,
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
                self.brace_style = match value {
                    "same_line" => BraceStyle::SameLine,
                    "next_line" => BraceStyle::NextLine,
                    other => {
                        return Err(format!(
                            "brace_style: expected \"same_line\" or \"next_line\", got {other:?}"
                        ));
                    }
                };
            }
            "indent_style" => {
                self.indent_style = match value {
                    "spaces" => IndentStyle::Spaces,
                    "tabs" => IndentStyle::Tabs,
                    other => {
                        return Err(format!(
                            "indent_style: expected \"spaces\" or \"tabs\", got {other:?}"
                        ));
                    }
                };
            }
            "trailing_comma" => {
                self.trailing_comma = match value {
                    "preserve" => TrailingComma::Preserve,
                    "always" => TrailingComma::Always,
                    "never" => TrailingComma::Never,
                    other => {
                        return Err(format!(
                            "trailing_comma: expected \"preserve\"/\"always\"/\"never\", got {other:?}"
                        ));
                    }
                };
            }
            other => return Err(format!("unknown option {other:?}")),
        }
        Ok(())
    }
}

fn parse_uint(key: &str, raw: &str) -> Result<usize, String> {
    raw.parse::<usize>()
        .map_err(|_| format!("{key}: expected non-negative integer, got {raw:?}"))
}

fn parse_bool(key: &str, raw: &str) -> Result<bool, String> {
    match raw {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(format!("{key}: expected boolean, got {other:?}")),
    }
}

fn expect_u(v: &toml::Value, key: &str) -> Result<usize, String> {
    v.as_integer()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| format!("Miku.toml: {key} must be a non-negative integer"))
}

fn expect_str<'a>(v: &'a toml::Value, key: &str) -> Result<&'a str, String> {
    v.as_str()
        .ok_or_else(|| format!("Miku.toml: {key} must be a string"))
}

fn expect_bool(v: &toml::Value, key: &str) -> Result<bool, String> {
    v.as_bool()
        .ok_or_else(|| format!("Miku.toml: {key} must be a boolean"))
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
    fn ignores_unknown_keys() {
        let src = r#"[format]
some_future_knob = "v9"
"#;
        FormatOptions::from_toml_str(src).unwrap();
    }
}
