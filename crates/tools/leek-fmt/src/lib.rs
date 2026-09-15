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
pub use pipeline::{FormatConfig, FormatQueryResult, format_query};

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
/// Convenience wrapper around [`leek_parser::parse_with_features`] +
/// [`format`]. Diagnostics produced during parsing are discarded — the
/// formatter always succeeds (`ErrorNode`s in the CST are emitted
/// verbatim).
///
/// The experimental grammar toggles still come off the environment here:
/// this signature has no flag argument to thread, and formatting a file
/// whose syntax only parses with `LEEK_EXPERIMENTAL_*` set must keep
/// working. Threading them through `FormatOptions` is the rest of #138.
pub fn format_source(
    text: &str,
    source: SourceId,
    version: Version,
    opts: &FormatOptions,
) -> String {
    let parsed = leek_parser::parse_with_features(
        text,
        source,
        version,
        leek_parser::ParseFeatures::from_env(),
    );
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
/// Returns `Some((target_range, replacement))` — the byte range to
/// replace and the text to put there — or `None` when there is
/// nothing to do: `range` extends past EOF, or no line the selection
/// touches would change.
///
/// A selection spanning several top-level items — or the whole file
/// — has no enclosing subtree below the `SourceFile` root. That is
/// not a "no edit" case (#200): the answer is the document format,
/// handed to [`narrow_to_changed_lines`] so the edit covers only the
/// lines that actually change. Returning the document as one edit
/// instead would replace the whole buffer and throw away the
/// client's cursor and selection.
///
/// The replacement is printed at the target's *logical* indent level
/// ([`logical_indent_level`]): the subtree's doc is wrapped in that
/// many [`doc::indent`] levels, so the printer raises every
/// continuation line itself, with the configured `indent` width and
/// [`IndentStyle`]. Line one carries no indent of its own — the
/// caller splices the text in where the node's range begins, and that
/// offset already sits at the node's indent — and needs no trimming
/// to get there, because the printer emits indentation only straight
/// after a line break. Trimming the front unconditionally would in
/// fact corrupt the one case where a replacement legitimately
/// *starts* with whitespace: a node inside a `// fmt: off` region is
/// emitted from its own source text, and the parser attaches the
/// trivia in front of a node's first token inside the node.
///
/// Letting the printer do it is what keeps a tab-indented file
/// tab-indented: padding continuation lines out to the node's source
/// *byte* column (as this used to) wrote spaces around the tabs the
/// printer emits inside, and shifted every line whenever a multi-byte
/// character sat ahead of the node on its line (#200).
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
    let target = smallest_enclosing_node(&root, range.clone());
    if target.kind() == SyntaxKind::SourceFile {
        // The subtree to format *is* the document, so run the
        // document format — same walk, same trailing newline, same
        // `// fmt:` pragmas, and no replay needed because nothing
        // sits in front of the root — then narrow the result.
        return narrow_to_changed_lines(&source, &format(green, version, opts), &range);
    }
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
    let level = logical_indent_level(&target);
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
        // Outside the options wrapper: the level is scaled by
        // whatever `indent` / `indent_style` the replay landed on,
        // the same ones the body below it is printed with.
        let doc = doc::indent(isize::try_from(level).unwrap_or(0), doc);
        apply_line_ending(printer::print(&doc, version, opts), opts.line_ending)
    });

    Some((target_start..target_end, raw))
}

/// How many indent levels a whole-document format would put `node`'s
/// own lines at.
///
/// Walks `node`'s ancestors up to the `SourceFile` root and counts
/// the ones whose formatter wraps the subtree `node` sits in with
/// `indent(1, …)`:
///
/// - `Block` and `ClassBody` indent everything between their braces
///   (`format::blocks::format_block`,
///   `format::stmts::format_class_body`);
/// - `SwitchStmt` indents its arms — but not the scrutinee in its
///   header — and `SwitchCase` indents the statements after its
///   colon, not the label expression, so a statement inside an arm
///   sits two levels below its `switch` (#497).
///
/// The *conditional* indents are deliberately not counted: a broken
/// argument or parameter group indents its contents only when the
/// printer picks the broken layout, and `control_braces = always`
/// indents an unbraced control body only because it is synthesizing
/// the braces around it. Neither shape is in the source text the
/// caller splices this replacement back into, so counting them would
/// push the range format a level past where the node actually sits.
fn logical_indent_level(node: &SyntaxNode) -> usize {
    let mut level = 0usize;
    let mut child = node.clone();
    while let Some(parent) = child.parent() {
        let indents = match parent.kind() {
            SyntaxKind::Block | SyntaxKind::ClassBody => true,
            SyntaxKind::SwitchStmt => child.kind() == SyntaxKind::SwitchCase,
            SyntaxKind::SwitchCase => is_switch_case_body(&parent, &child),
            _ => false,
        };
        if indents {
            level += 1;
        }
        child = parent;
    }
    level
}

/// Does `child` land in the indented *body* of its `SwitchCase`
/// parent, rather than in the `case <expr>:` label that stays on the
/// head line?
///
/// Mirrors the placement rule in `format::stmts::format_switch_case`:
/// on a `case`, the first node before the colon is the label
/// expression and every later node is a statement; a `default` arm
/// has no label expression, so every node is a statement.
fn is_switch_case_body(case: &SyntaxNode, child: &SyntaxNode) -> bool {
    let mut is_case = false;
    let mut seen_colon = false;
    let mut label_expr_seen = false;
    for el in case.children_with_tokens() {
        match el {
            leek_syntax::language::NodeOrToken::Token(t) => match t.kind() {
                SyntaxKind::KwCase => is_case = true,
                SyntaxKind::Colon if !seen_colon => seen_colon = true,
                _ => {}
            },
            leek_syntax::language::NodeOrToken::Node(n) => {
                let in_label = is_case && !seen_colon && !label_expr_seen;
                if in_label {
                    label_expr_seen = true;
                }
                if n == *child {
                    return !in_label;
                }
            }
        }
    }
    false
}

/// Find the smallest [`SyntaxNode`] under `root` whose text range
/// fully contains `range`. Skips the trivia-only edges of nodes —
/// if the range falls inside a token's whitespace, we still return
/// the enclosing significant node.
///
/// A range no single item contains — several top-level items, or the
/// whole file — stops the descent at the `SourceFile` root, and the
/// root is what comes back. It used to come back as `None`, which
/// both LSP handlers turn into an empty edit list: range-formatting a
/// two-statement selection silently did nothing (#200). The one range
/// this tree genuinely cannot answer for is a range past EOF, and
/// [`format_range`] rejects that one before it gets here.
fn smallest_enclosing_node(root: &SyntaxNode, range: std::ops::Range<u32>) -> SyntaxNode {
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
    current
}

/// The smallest edit that turns `original` into `formatted`, or
/// `None` when no line the selection touches changes.
///
/// `original` and `formatted` are compared line by line; the common
/// prefix and the common suffix of whole lines drop out, and what is
/// left — the first differing line through the last — is the edit.
/// Identical lines hold identical bytes, so both texts' prefix lines
/// end at the same offset and both texts' suffix lines start the same
/// number of bytes from the end: that arithmetic is the whole
/// alignment, and it makes the replacement precisely the formatted
/// counterpart of the original lines it replaces.
///
/// `range` then decides whether the edit is wanted at all. One that
/// touches none of the lines `range` covers is dropped, so formatting
/// a tidy selection inside an untidy file leaves the file alone.
/// Beyond that yes-or-no the edit is *not* clipped back to the
/// selection: dropping a differing line off the front would mean
/// splicing from somewhere in the middle of the formatted text, and
/// nothing here can say which formatted line a changed original line
/// became — matching lines is exactly what failed on it. So the
/// guarantee is the weaker, honest one: every line in the edit is a
/// line the document format rewrites (never the whole buffer for a
/// one-line fix), and the selection decides whether to send it.
fn narrow_to_changed_lines(
    original: &str,
    formatted: &str,
    range: &std::ops::Range<u32>,
) -> Option<(std::ops::Range<u32>, String)> {
    // Nothing differs: `None`, the "no edits" both LSP handlers
    // already understand — each turns it into an empty edit list, and
    // `rangeFormatting` would have dropped an empty edit on its own
    // anyway by comparing the replacement against the original slice.
    if original == formatted {
        return None;
    }
    let orig: Vec<&str> = original.split_inclusive('\n').collect();
    let new: Vec<&str> = formatted.split_inclusive('\n').collect();
    let common = orig.len().min(new.len());

    let mut prefix = 0;
    while prefix < common && orig[prefix] == new[prefix] {
        prefix += 1;
    }
    // Bounded by the lines the prefix has not already claimed, so the
    // two runs never overlap in either text.
    let mut suffix = 0;
    while suffix < common - prefix && orig[orig.len() - 1 - suffix] == new[new.len() - 1 - suffix] {
        suffix += 1;
    }

    let start: usize = orig[..prefix].iter().map(|l| l.len()).sum();
    let tail: usize = orig[orig.len() - suffix..].iter().map(|l| l.len()).sum();
    let end = original.len() - tail;
    let replacement = &formatted[start..formatted.len() - tail];

    let window = whole_lines(original, range);
    // Half-open ranges touch only where they overlap, but an empty
    // one — an insertion, or a caret selection — has to count as
    // touching the position it sits at, or typing at a line boundary
    // would never format.
    let touches = if start == end || window.start == window.end {
        start <= window.end && window.start <= end
    } else {
        start < window.end && window.start < end
    };
    if !touches {
        return None;
    }
    Some((
        leek_span::offset(start)..leek_span::offset(end),
        replacement.to_owned(),
    ))
}

/// `range` grown to whole lines of `text`: back to the start of the
/// line it starts on, forward past the end of the line it ends on.
///
/// An end already sitting at a line start is left where it is — an
/// editor's full-line selection ends at column 0 of the line *after*
/// the last highlighted one, and pulling that line in would let the
/// selection claim a line the user never highlighted.
///
/// Scans bytes rather than slicing: a `\n` cannot occur inside a
/// multi-byte UTF-8 sequence, so every offset this walks to is a char
/// boundary even when the one it was handed is not. An out-of-bounds
/// offset clamps instead of panicking — this runs inside a language
/// server, where the tree and the buffer can disagree mid-edit.
fn whole_lines(text: &str, range: &std::ops::Range<u32>) -> std::ops::Range<usize> {
    let bytes = text.as_bytes();
    let mut start = (range.start as usize).min(bytes.len());
    let mut end = (range.end as usize).min(bytes.len());
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }
    if end > 0 && bytes[end - 1] != b'\n' {
        while end < bytes.len() && bytes[end] != b'\n' {
            end += 1;
        }
        // Past the terminator, so a line and its line break move together.
        if end < bytes.len() {
            end += 1;
        }
    }
    start..end.max(start)
}
