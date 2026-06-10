//! Leekscript code formatter.
//!
//! Operates on the [`leek_syntax`] CST. The pipeline is:
//!
//! ```text
//! green tree ── walk ──▶ Doc IR ── print ──▶ String
//! ```
//!
//! The Doc IR is Wadler/Prettier-style: each node decides on
//! `group`s, `line`s, and `indent`s; the printer measures each group
//! against the configured `max_line_length` and chooses flat vs.
//! broken layout. Trivia (comments and blank lines) attached to
//! significant tokens is preserved.
//!
//! Public entry points:
//!
//! - [`format`] — format a parsed [`GreenNode`].
//! - [`format_source`] — lex+parse a string then format.

pub mod doc;
pub mod format;
pub mod pipeline;
pub mod printer;

pub use leek_manifest::{BraceStyle, FormatOptions, IndentStyle, TrailingComma};
pub use pipeline::{Fmt, FormattedArtifact};

#[cfg(feature = "salsa")]
pub use pipeline::{FormatQueryResult, format_query};

use leek_span::SourceId;
use leek_syntax::language::GreenNode;
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

/// Format a parsed green tree.
pub fn format(green: &GreenNode, opts: &FormatOptions) -> String {
    let root = SyntaxNode::new_root(green.clone());
    let ctx = format::FmtCtx {
        opts: opts.clone(),
        opts_stack: Vec::new(),
        off_regions: collect_off_regions(&root),
    };
    let doc = format::with_ctx_set(ctx, || format::format_source_file(&root));
    printer::print(&doc, opts)
}

/// Scan trivia for `// fmt: off` / `// fmt: on` markers and return
/// the disjoint byte ranges they enclose.
///
/// A `// fmt: off` with no matching `on` disables formatting to EOF.
/// Repeated `off`s (without an `on` in between) are idempotent.
fn collect_off_regions(root: &SyntaxNode) -> Vec<std::ops::Range<u32>> {
    let mut out: Vec<std::ops::Range<u32>> = Vec::new();
    let mut off_start: Option<u32> = None;
    let eof = u32::from(root.text_range().end());

    for tok in root
        .descendants_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
    {
        if !matches!(
            tok.kind(),
            SyntaxKind::LineComment | SyntaxKind::BlockComment
        ) {
            continue;
        }
        let body = parse_fmt_pragma(tok.text());
        let start = u32::from(tok.text_range().start());
        let end = u32::from(tok.text_range().end());
        match body {
            FmtPragma::Off if off_start.is_none() => {
                // Start the off region *after* the marker comment so
                // the marker itself stays present in output.
                off_start = Some(end);
            }
            FmtPragma::On => {
                if let Some(s) = off_start.take() {
                    // End the off region *before* the marker so the
                    // marker itself is preserved as-is.
                    out.push(s..start);
                }
            }
            _ => {}
        }
    }
    if let Some(s) = off_start {
        out.push(s..eof);
    }
    out
}

/// Recognized formatter pragmas.
///
/// Pragma syntax (in a `// …` or `/* … */` comment):
/// - `// fmt: off` / `// fmt: on` — region-based formatting toggle.
/// - `// fmt: skip` / `// fmt-skip` — skip the next sibling.
/// - `// fmt: <key> = <value>` — set the option from this point on.
/// - `// fmt: push <key> = <value>` — push a scoped override.
/// - `// fmt: pop` — restore the previous scope's options.
/// - `// fmt: next <key> = <value>` — apply an override to the next
///   sibling only, then restore. Multiple `next` pragmas stack and
///   all apply to the same following item.
///
/// All pragma comments are suppressed from formatter output —
/// users don't want their `// fmt: …` markers reformatted *and*
/// preserved as content.
///
/// **Which keys take effect in pragmas:** all
/// [`FormatOptions`] fields work per-region. Build-time options
/// (`trailing_comma`, `max_blank_lines`, `space_before_call_paren`)
/// are consulted as the formatter constructs the Doc IR;
/// print-time options (`indent`, `indent_style`,
/// `max_line_length`) ride into the printer via
/// [`Doc::WithOptions`](crate::doc::Doc::WithOptions) wrappers
/// inserted by the sibling walkers. Push/Pop/Set/Next all switch
/// the active options for the next item; `pop` restores the
/// previously-pushed snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FmtPragma {
    Off,
    On,
    Skip,
    /// `// fmt: <key> = <value>` — mutate one option, persisting
    /// until the next change.
    Set(String, String),
    /// `// fmt: push <key> = <value>` — save current options, then
    /// mutate one option for the next scope.
    Push(String, String),
    /// `// fmt: pop` — restore previously pushed options.
    Pop,
    /// `// fmt: next <key> = <value>` — apply the override to the
    /// next sibling item only. The sibling walker queues these and
    /// pushes/pops them around the next item it emits.
    Next(String, String),
    None,
}

/// True iff `raw` is a `// fmt-skip` (or `// fmt: skip`) comment.
/// Exposed for `format::is_fmt_skipped`.
pub(crate) fn is_fmt_skip_marker(raw: &str) -> bool {
    matches!(parse_fmt_pragma(raw), FmtPragma::Skip)
}

/// Parse a comment's text into a [`FmtPragma`]. Non-pragma comments
/// (or unrecognized `// fmt: …` syntax) return [`FmtPragma::None`].
pub(crate) fn parse_fmt_pragma(raw: &str) -> FmtPragma {
    let inner = if let Some(s) = raw.strip_prefix("///") {
        // Doc comments are never pragmas.
        let _ = s;
        return FmtPragma::None;
    } else if let Some(s) = raw.strip_prefix("//") {
        s
    } else if let Some(s) = raw.strip_prefix("/*").and_then(|s| s.strip_suffix("*/")) {
        s
    } else {
        return FmtPragma::None;
    };
    let body = inner.trim();

    // The `fmt-skip` alias is the one place we accept the kebab
    // form without a colon.
    if body == "fmt-skip" {
        return FmtPragma::Skip;
    }

    let rest = match body
        .strip_prefix("fmt:")
        .or_else(|| body.strip_prefix("fmt :"))
    {
        Some(s) => s.trim(),
        None => return FmtPragma::None,
    };

    match rest {
        "off" => FmtPragma::Off,
        "on" => FmtPragma::On,
        "skip" => FmtPragma::Skip,
        "pop" => FmtPragma::Pop,
        _ => {
            // Verb-prefixed forms first; bare "key = value" last.
            // The verb must be followed by whitespace so a key whose
            // name happens to start with "next"/"push"/"set" (e.g. a
            // future `nextline_threshold`) doesn't get misparsed.
            if let Some(after) = strip_verb(rest, "next")
                && let Some((k, v)) = parse_key_eq_value(after)
            {
                return FmtPragma::Next(k.to_string(), v.to_string());
            }
            if let Some(after) = strip_verb(rest, "push")
                && let Some((k, v)) = parse_key_eq_value(after)
            {
                return FmtPragma::Push(k.to_string(), v.to_string());
            }
            if let Some(after) = strip_verb(rest, "set")
                && let Some((k, v)) = parse_key_eq_value(after)
            {
                return FmtPragma::Set(k.to_string(), v.to_string());
            }
            if let Some((k, v)) = parse_key_eq_value(rest) {
                return FmtPragma::Set(k.to_string(), v.to_string());
            }
            FmtPragma::None
        }
    }
}

/// Strip `verb` followed by at least one whitespace char from the
/// front of `s`, returning the trimmed remainder. The whitespace
/// requirement keeps verbs from accidentally swallowing key names
/// that happen to start with the same letters.
fn strip_verb<'a>(s: &'a str, verb: &str) -> Option<&'a str> {
    let after = s.strip_prefix(verb)?;
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    Some(after.trim_start())
}

/// Split `key = value`. Trims whitespace and strips matched outer
/// quotes from `value` so `indent_style = "tabs"` and
/// `indent_style = tabs` both parse the same way.
fn parse_key_eq_value(s: &str) -> Option<(&str, &str)> {
    let (k, v) = s.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    let v = v.trim();
    let v = strip_quotes(v);
    if v.is_empty() {
        return None;
    }
    Some((k, v))
}

fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[0] == bytes[bytes.len() - 1]
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Lex + parse `text` and format the result.
///
/// Convenience wrapper around [`leek_parser::parse`] + [`format`].
/// Diagnostics produced during parsing are discarded — the formatter
/// always succeeds (`ErrorNode`s in the CST are emitted verbatim).
pub fn format_source(
    text: &str,
    source: SourceId,
    version: Version,
    opts: &FormatOptions,
) -> String {
    let parsed = leek_parser::parse(text, source, version);
    format(&parsed.green, opts)
}

/// Format the smallest CST subtree that fully contains `range`.
///
/// Returns `Some((target_range, replacement))` if a suitable subtree
/// exists, or `None` if `range` doesn't match any node (e.g. it
/// extends past EOF).
///
/// The replacement is re-indented so its first line starts at column
/// 0 (callers append it where the node's range begins), and every
/// continuation line is prefixed with the source-detected leading
/// indent of the node's start position. This way, an LSP client
/// applying the returned text-edit gets correctly-indented output.
///
/// The returned `(start, end)` is the byte range of the chosen
/// subtree — the same range the caller should replace with
/// `replacement`.
pub fn format_range(
    green: &GreenNode,
    opts: &FormatOptions,
    range: std::ops::Range<u32>,
) -> Option<(std::ops::Range<u32>, String)> {
    let root = SyntaxNode::new_root(green.clone());
    let source = root.text().to_string();
    if range.end > leek_span::offset(source.len()) {
        return None;
    }
    let target = smallest_enclosing_node(&root, range)?;
    let target_start = u32::from(target.text_range().start());
    let target_end = u32::from(target.text_range().end());

    // Reuse the global ctx-install path so off-regions and pragmas
    // still apply. Format ONLY this subtree.
    let ctx = format::FmtCtx {
        opts: opts.clone(),
        opts_stack: Vec::new(),
        off_regions: collect_off_regions(&root),
    };
    let raw = format::with_ctx_set(ctx, || {
        let doc = format::fmt_node(&target);
        printer::print(&doc, opts)
    });

    let base_col = leading_column(&source, target_start as usize);
    let result = if base_col > 0 {
        let pad = " ".repeat(base_col);
        raw.replace('\n', &format!("\n{pad}"))
    } else {
        raw
    };

    Some((target_start..target_end, result))
}

/// Find the smallest [`SyntaxNode`] under `root` whose text range
/// fully contains `range`. Skips the trivia-only edges of nodes —
/// if the range falls inside a token's whitespace, we still return
/// the enclosing significant node.
fn smallest_enclosing_node(root: &SyntaxNode, range: std::ops::Range<u32>) -> Option<SyntaxNode> {
    let mut current = root.clone();
    'outer: loop {
        for child in current.children() {
            let r = child.text_range();
            let cs = u32::from(r.start());
            let ce = u32::from(r.end());
            if cs <= range.start && range.end <= ce {
                current = child;
                continue 'outer;
            }
        }
        break;
    }
    // Whole-file range or a range spanning multiple top-level
    // items lands on `SourceFile`. Either way, callers should fall
    // back to `format()` for whole-document formatting.
    if current.kind() == leek_syntax::SyntaxKind::SourceFile {
        return None;
    }
    Some(current)
}

/// Column (0-based, byte-counted) of `offset` within its line.
fn leading_column(source: &str, offset: usize) -> usize {
    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    offset - line_start
}
