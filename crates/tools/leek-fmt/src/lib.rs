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

mod comments;
pub mod doc;
pub mod equivalence;
pub mod format;
pub mod pipeline;
pub mod printer;

pub use equivalence::{EquivalenceError, check_edit_equivalence, check_equivalence};
pub use leek_manifest::{
    BraceStyle, ControlBraces, FormatOptions, IndentStyle, LineEnding, OperatorPosition,
    QuoteStyle, Semicolons, TrailingComma,
};
pub use pipeline::{Fmt, FormattedArtifact};

#[cfg(feature = "salsa")]
pub use pipeline::{FormatQueryResult, format_query};

use leek_span::SourceId;
use leek_syntax::language::GreenNode;
use leek_syntax::{SyntaxKind, SyntaxNode, Version};

/// Format a parsed green tree.
///
/// `version` is the language version the tree was parsed under; the
/// printer re-lexes token boundaries with it to decide where a
/// separator is required (#412).
pub fn format(green: &GreenNode, version: Version, opts: &FormatOptions) -> String {
    let root = SyntaxNode::new_root(green.clone());
    let ctx = format::FmtCtx {
        opts: opts.clone(),
        opts_stack: Vec::new(),
        off_regions: collect_off_regions(&root),
    };
    let doc = format::with_ctx_set(ctx, || format::format_source_file(&root));
    let mut out = printer::print(&doc, version, opts);
    // Terminate the file — but never write *into* the last token. An
    // unterminated `/*` comment or string literal runs to end of file
    // and owns every byte to EOF, so a newline appended after it lands
    // inside it: the next pass lexes a longer token and appends again,
    // a line per formatting pass (#417, #419). Note this is
    // append-if-absent, never trim-then-append — trimming would delete
    // characters from inside such a token, the same bug mirrored.
    if !out.ends_with('\n') && !format::ends_in_unclosed_token(&root) {
        out.push('\n');
    }
    // Expand to CRLF *after* the terminator is in place, so the last
    // line gets the configured ending like every other one.
    apply_line_ending(out, opts.line_ending)
}

/// Normalize the output's line terminators per [`LineEnding`].
///
/// The printer always emits `\n`; raw-passthrough regions (`// fmt:
/// off`, error nodes) may carry `\r\n` from the source. Normalizing
/// to `\n` first and then (for [`LineEnding::Crlf`]) expanding makes
/// both paths land on the configured terminator — and keeps the
/// transform idempotent.
fn apply_line_ending(s: String, le: LineEnding) -> String {
    let normalized = if s.contains('\r') {
        s.replace("\r\n", "\n")
    } else {
        s
    };
    match le {
        LineEnding::Lf => normalized,
        LineEnding::Crlf => normalized.replace('\n', "\r\n"),
    }
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

/// Collect the *option* pragmas (`set` / `push` / `pop` / `next`)
/// that precede `offset`, in document order, each paired with the
/// byte range of the comment that carried it.
///
/// Same walk shape as [`collect_off_regions`]: every comment token
/// under `root`, in document order, run through [`parse_fmt_pragma`].
/// `off` / `on` / `skip` and plain comments are dropped — off-regions
/// are already turned into verbatim ranges by [`collect_off_regions`],
/// and `skip` is decided per-node by [`format::is_fmt_skipped`].
///
/// Used by [`format_range`] to replay the settings a whole-document
/// format would have accumulated on its way to the target (#203).
fn pragmas_before(root: &SyntaxNode, offset: u32) -> Vec<(std::ops::Range<u32>, FmtPragma)> {
    root.descendants_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .filter(|tok| {
            matches!(
                tok.kind(),
                SyntaxKind::LineComment | SyntaxKind::BlockComment
            ) && u32::from(tok.text_range().end()) <= offset
        })
        .map(|tok| {
            let range = u32::from(tok.text_range().start())..u32::from(tok.text_range().end());
            (range, parse_fmt_pragma(tok.text()))
        })
        .filter(|(_, p)| {
            matches!(
                p,
                FmtPragma::Set(..) | FmtPragma::Push(..) | FmtPragma::Pop | FmtPragma::Next(..)
            )
        })
        .collect()
}

/// End offset of the last significant (non-trivia) token that ends
/// at or before `offset` — the point past which only whitespace and
/// comments separate a pragma from the target.
///
/// `0` when there is no such token (the target is the first thing in
/// the file), which lets every preceding pragma count as adjacent.
fn last_significant_end_before(root: &SyntaxNode, offset: u32) -> u32 {
    root.descendants_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .filter(|tok| !tok.kind().is_trivia() && u32::from(tok.text_range().end()) <= offset)
        .map(|tok| u32::from(tok.text_range().end()))
        .last()
        .unwrap_or(0)
}

/// Split `pragmas` into the ones to replay as persistent state and
/// the `// fmt: next` overrides that actually reach the target.
///
/// `next` is scoped to exactly one following item, so only the run of
/// `next` pragmas that no code separates from the target applies —
/// `barrier` is [`last_significant_end_before`] the target, and the
/// sibling walkers likewise hold a `next` across trivia and hand it
/// to the first real item they reach. Every earlier `next` was
/// consumed by whatever item followed it; replaying those would
/// silently re-target this range, so they stay in the prefix, where
/// [`format::apply_pragma_to_ctx`] discards them.
///
/// Returns `(prefix, overrides)` with `overrides` in document order,
/// matching the order the sibling walkers push them in — several
/// `next` pragmas stack onto the same following item.
fn split_trailing_next(
    pragmas: &[(std::ops::Range<u32>, FmtPragma)],
    barrier: u32,
) -> (&[(std::ops::Range<u32>, FmtPragma)], Vec<(String, String)>) {
    let mut cut = pragmas.len();
    while cut > 0 {
        let (range, FmtPragma::Next(..)) = &pragmas[cut - 1] else {
            break;
        };
        if range.start < barrier {
            break;
        }
        cut -= 1;
    }
    let overrides = pragmas[cut..]
        .iter()
        .filter_map(|(_, p)| match p {
            FmtPragma::Next(k, v) => Some((k.clone(), v.clone())),
            _ => None,
        })
        .collect();
    (&pragmas[..cut], overrides)
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
/// Pragma comments survive into the output like any other comment.
/// Dropping them would make formatting non-idempotent — the second
/// run would no longer see the `// fmt: off` that protected a region
/// on the first — and [`check_equivalence`] treats a lost pragma as
/// the lost comment it is.
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
    format(&parsed.green, version, opts)
}

/// [`format_source`] followed by the [`check_equivalence`] safety net.
///
/// Returns the formatted text only when it provably keeps every
/// comment and significant token of `text`. Callers that write the
/// result back to disk or into an editor buffer should use this rather
/// than [`format_source`], and leave the source untouched on error.
pub fn format_source_checked(
    text: &str,
    source: SourceId,
    version: Version,
    opts: &FormatOptions,
) -> Result<String, EquivalenceError> {
    let formatted = format_source(text, source, version, opts);
    check_equivalence(text, &formatted, version)?;
    Ok(formatted)
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
    version: Version,
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
    //
    // A whole-document format reaches the target having already
    // walked every `// fmt:` comment in front of it, so its options
    // are whatever the user's pragmas made them. A range format
    // starts at the target, so it has to replay those pragmas first
    // or on-type and range formatting disagree with the document
    // format about the user's own settings (#203).
    let pragmas = pragmas_before(&root, target_start);
    let barrier = last_significant_end_before(&root, target_start);
    let (replay, next_overrides) = split_trailing_next(&pragmas, barrier);
    let ctx = format::FmtCtx {
        opts: opts.clone(),
        opts_stack: Vec::new(),
        off_regions: collect_off_regions(&root),
    };
    let raw = format::with_ctx_set(ctx, || {
        // Document order: a later `set` beats an earlier one, and
        // `push` / `pop` pairs nest the way the walkers nest them.
        // A `Next` left in the prefix is one an earlier item already
        // consumed; `apply_pragma_to_ctx` drops it, which is exactly
        // the "discard, don't apply" this needs.
        for (_, p) in replay {
            format::apply_pragma_to_ctx(p);
        }
        let doc = format::fmt_node_with_next_overrides(&target, &next_overrides);
        // Same shape as the sibling walkers: snapshot the options the
        // replay landed on so print-time settings (`indent`,
        // `indent_style`, `max_line_length`) reach the printer, which
        // is otherwise handed the unmodified `opts`.
        let doc = format::wrap_with_active_opts(doc);
        apply_line_ending(printer::print(&doc, version, opts), opts.line_ending)
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
