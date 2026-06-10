//! Editor-pushed configuration (`workspace/didChangeConfiguration` and
//! the initial `workspace/configuration` pull).
//!
//! The client owns these settings (VS Code's `settings.json` under the
//! `leek` section); the server mirrors them and the relevant handlers
//! consult the mirror. Today that's the formatter's [`FormatOptions`]
//! and an inlay-hint on/off toggle — the two things a user most often
//! wants to tune from the editor rather than a `Miku.toml`.
//!
//! Parsing is deliberately lenient: unknown keys are ignored and any
//! missing key keeps its default, so a partial or differently-shaped
//! settings blob never breaks the server.

use leek_fmt::{BraceStyle, FormatOptions, IndentStyle, TrailingComma};
use serde_json::Value;

/// The server's view of client configuration.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Options handed to the formatter for `textDocument/formatting`,
    /// `rangeFormatting`, and `onTypeFormatting`.
    pub format: FormatOptions,
    /// Whether `textDocument/inlayHint` produces hints. When `false`
    /// the handler returns an empty set so the editor clears them.
    pub inlay_hints: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: FormatOptions::default(),
            inlay_hints: true,
        }
    }
}

impl Settings {
    /// Parse settings from a client-supplied JSON value. Accepts either
    /// the full settings tree (we read its `leek` section) or the
    /// already-scoped `leek` object (as returned by a
    /// `workspace/configuration` pull for section `"leek"`).
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        // Prefer a nested `leek` section; otherwise treat `value` itself
        // as that section.
        let root = value.get("leek").unwrap_or(value);
        let mut s = Settings::default();

        if let Some(fmt) = root.get("format").and_then(Value::as_object) {
            apply_format(&mut s.format, fmt);
        }
        // `inlayHints` accepts either a bare bool or `{ "enabled": bool }`
        // (both spellings appear in the wild).
        match root.get("inlayHints") {
            Some(Value::Bool(b)) => s.inlay_hints = *b,
            Some(Value::Object(o)) => {
                if let Some(b) = o.get("enabled").and_then(Value::as_bool) {
                    s.inlay_hints = b;
                }
            }
            _ => {}
        }
        s
    }
}

/// Overlay a `format` settings object onto `opts`, leaving any field the
/// client didn't specify at its current value.
fn apply_format(opts: &mut FormatOptions, fmt: &serde_json::Map<String, Value>) {
    if let Some(n) = read_usize(fmt, "indent") {
        opts.indent = n;
    }
    if let Some(n) = read_usize(fmt, "maxLineLength") {
        opts.max_line_length = n;
    }
    if let Some(n) = read_usize(fmt, "maxBlankLines") {
        opts.max_blank_lines = n;
    }
    if let Some(b) = fmt.get("spaceBeforeCallParen").and_then(Value::as_bool) {
        opts.space_before_call_paren = b;
    }
    if let Some(b) = fmt.get("spaceInsideBrackets").and_then(Value::as_bool) {
        opts.space_inside_brackets = b;
    }
    if let Some(b) = fmt.get("spaceInsideParens").and_then(Value::as_bool) {
        opts.space_inside_parens = b;
    }
    if let Some(style) = fmt.get("braceStyle").and_then(Value::as_str) {
        match style.to_ascii_lowercase().replace('-', "_").as_str() {
            "next_line" | "nextline" | "allman" => opts.brace_style = BraceStyle::NextLine,
            "same_line" | "sameline" | "kr" => opts.brace_style = BraceStyle::SameLine,
            _ => {}
        }
    }
    if let Some(b) = fmt.get("spaceAfterComma").and_then(Value::as_bool) {
        opts.space_after_comma = b;
    }
    if let Some(b) = fmt.get("spaceAfterControlKeyword").and_then(Value::as_bool) {
        opts.space_after_control_keyword = b;
    }
    if let Some(b) = fmt.get("spaceAroundArrow").and_then(Value::as_bool) {
        opts.space_around_arrow = b;
    }
    if let Some(b) = fmt.get("spaceBeforeColon").and_then(Value::as_bool) {
        opts.space_before_colon = b;
    }
    if let Some(b) = fmt.get("spaceAfterColon").and_then(Value::as_bool) {
        opts.space_after_colon = b;
    }
    if let Some(b) = fmt.get("padLineComments").and_then(Value::as_bool) {
        opts.pad_line_comments = b;
    }
    if let Some(style) = fmt.get("indentStyle").and_then(Value::as_str) {
        match style.to_ascii_lowercase().as_str() {
            "tab" | "tabs" => opts.indent_style = IndentStyle::Tabs,
            "space" | "spaces" => opts.indent_style = IndentStyle::Spaces,
            _ => {}
        }
    }
    if let Some(tc) = fmt.get("trailingComma").and_then(Value::as_str) {
        match tc.to_ascii_lowercase().as_str() {
            "always" => opts.trailing_comma = TrailingComma::Always,
            "never" => opts.trailing_comma = TrailingComma::Never,
            "preserve" => opts.trailing_comma = TrailingComma::Preserve,
            _ => {}
        }
    }
}

/// Read a non-negative integer setting as `usize` (ignoring values that
/// don't fit, rather than truncating).
fn read_usize(fmt: &serde_json::Map<String, Value>, key: &str) -> Option<usize> {
    fmt.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_when_empty() {
        let s = Settings::from_value(&json!({}));
        assert_eq!(s.format, FormatOptions::default());
        assert!(s.inlay_hints);
    }

    #[test]
    fn reads_nested_leek_section() {
        let s = Settings::from_value(&json!({
            "leek": {
                "format": { "indent": 2, "indentStyle": "tabs", "maxLineLength": 80 },
                "inlayHints": false
            }
        }));
        assert_eq!(s.format.indent, 2);
        assert_eq!(s.format.indent_style, IndentStyle::Tabs);
        assert_eq!(s.format.max_line_length, 80);
        assert!(!s.inlay_hints);
    }

    #[test]
    fn reads_already_scoped_section() {
        // A `workspace/configuration` pull for "leek" returns the inner
        // object directly, with no `leek` wrapper.
        let s = Settings::from_value(&json!({
            "format": { "trailingComma": "always", "spaceBeforeCallParen": true }
        }));
        assert_eq!(s.format.trailing_comma, TrailingComma::Always);
        assert!(s.format.space_before_call_paren);
    }

    #[test]
    fn inlay_hints_object_form() {
        let s = Settings::from_value(&json!({ "inlayHints": { "enabled": false } }));
        assert!(!s.inlay_hints);
    }

    #[test]
    fn partial_format_keeps_other_defaults() {
        let s = Settings::from_value(&json!({ "format": { "indent": 8 } }));
        let d = FormatOptions::default();
        assert_eq!(s.format.indent, 8);
        // Everything else stays at the default.
        assert_eq!(s.format.max_line_length, d.max_line_length);
        assert_eq!(s.format.indent_style, d.indent_style);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let s =
            Settings::from_value(&json!({ "leek": { "bogus": 1, "format": { "nope": true } } }));
        assert_eq!(s.format, FormatOptions::default());
    }

    #[test]
    fn reads_new_format_options() {
        let s = Settings::from_value(&json!({
            "format": {
                "spaceInsideBrackets": true,
                "spaceInsideParens": true,
                "braceStyle": "next_line"
            }
        }));
        assert!(s.format.space_inside_brackets);
        assert!(s.format.space_inside_parens);
        assert_eq!(s.format.brace_style, BraceStyle::NextLine);
    }

    #[test]
    fn reads_spacing_and_comment_options() {
        let s = Settings::from_value(&json!({
            "format": {
                "spaceAfterComma": false,
                "spaceAfterControlKeyword": false,
                "spaceAroundArrow": false,
                "spaceBeforeColon": true,
                "spaceAfterColon": false,
                "padLineComments": true
            }
        }));
        assert!(!s.format.space_after_comma);
        assert!(!s.format.space_after_control_keyword);
        assert!(!s.format.space_around_arrow);
        assert!(s.format.space_before_colon);
        assert!(!s.format.space_after_colon);
        assert!(s.format.pad_line_comments);
    }
}
