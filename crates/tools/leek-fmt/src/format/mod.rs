//! Per-construct formatting.
//!
//! Walks the CST and builds a [`Doc`] tree. Trivia (comments and
//! blank lines) is handled inline: as each node walks its
//! `children_with_tokens()`, trivia tokens between siblings drive
//! blank-line and comment placement.
//!
//! The dispatch entry point is [`format_source_file`]; everything
//! else recursively calls back through [`fmt_node`].

use std::cell::RefCell;
use std::ops::Range;

use leek_syntax::{SyntaxKind as S, SyntaxNode, SyntaxToken};

use crate::FormatOptions;
use crate::doc::{Doc, concat, hardline, text};

mod blocks;
mod exprs;
mod stmts;

// ---- Formatter context (thread-local) ----
//
// Per-node formatters need access to the active [`FormatOptions`]
// and to the precomputed `// fmt: off` / `// fmt: on` regions, but
// threading both through every `fmt_node` call would touch every
// signature. We park them in a thread-local for the duration of one
// top-level `format()` call instead.

#[derive(Debug, Clone, Default)]
pub(crate) struct FmtCtx {
    /// Currently active options. Mutated by `// fmt: <key> = <value>`
    /// pragmas; pushed onto [`opts_stack`] and replaced by `// fmt:
    /// push …`; restored by `// fmt: pop`.
    ///
    /// [`opts_stack`]: FmtCtx::opts_stack
    pub opts: FormatOptions,
    /// Saved-options stack for `push` / `pop` pragma pairs.
    pub opts_stack: Vec<FormatOptions>,
    /// Byte ranges of source that should be emitted verbatim
    /// (the `// fmt: off … // fmt: on` regions).
    pub off_regions: Vec<Range<u32>>,
}

thread_local! {
    static FMT_CTX: RefCell<FmtCtx> = RefCell::new(FmtCtx::default());
}

/// Read the active [`FmtCtx`]. Cheap; no allocations.
pub(crate) fn with_ctx<R>(f: impl FnOnce(&FmtCtx) -> R) -> R {
    FMT_CTX.with(|c| f(&c.borrow()))
}

/// Run `body` with `ctx` installed as the active [`FmtCtx`]. The previous
/// context is restored on exit — even if `body` panics — so a panic mid-format
/// can't leave stale context (e.g. leaked `// fmt:` overrides) poisoning a later
/// format on the same thread. The restore runs from a drop guard during unwind;
/// the LSP `catch_unwind`s formatter calls, so this keeps the next keystroke's
/// format clean.
pub(crate) fn with_ctx_set<R>(ctx: FmtCtx, body: impl FnOnce() -> R) -> R {
    /// Restores the saved context on drop (normal return *or* panic unwind).
    struct Restore(Option<FmtCtx>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(prev) = self.0.take() {
                FMT_CTX.with(|c| *c.borrow_mut() = prev);
            }
        }
    }
    let prev = FMT_CTX.with(|c| std::mem::replace(&mut *c.borrow_mut(), ctx));
    let _restore = Restore(Some(prev));
    body()
}

/// Wrap a `fmt_node(node)` call with a stack of `// fmt: next`
/// overrides. Each `(key, value)` is `push`ed before formatting and
/// `pop`ped after, so the override applies to exactly one item.
///
/// Returns the formatted [`Doc`] for `node`. Caller-side use:
///
/// ```ignore
/// let doc = fmt_node_with_next_overrides(&child, &pending_next);
/// pending_next.clear();
/// ```
pub(crate) fn fmt_node_with_next_overrides(
    node: &SyntaxNode,
    overrides: &[(String, String)],
) -> Doc {
    use crate::FmtPragma;
    for (k, v) in overrides {
        apply_pragma_to_ctx(&FmtPragma::Push(k.clone(), v.clone()));
    }
    let out = fmt_node(node);
    for _ in 0..overrides.len() {
        apply_pragma_to_ctx(&FmtPragma::Pop);
    }
    out
}

/// Snapshot the currently-active formatter options and wrap `inner`
/// in a [`Doc::WithOptions`] so the printer applies them when this
/// region renders. Used by sibling walkers (block bodies, source-
/// file children) to capture per-region print-time options
/// (`indent`, `indent_style`, `max_line_length`) that
/// [`apply_pragma_to_ctx`] may have mutated mid-walk.
///
/// No-op when `inner` is `Doc::Nil`.
pub(crate) fn wrap_with_active_opts(inner: crate::doc::Doc) -> crate::doc::Doc {
    use crate::doc::Doc;
    if matches!(inner, Doc::Nil) {
        return inner;
    }
    let opts = with_ctx(|cx| cx.opts.clone());
    crate::doc::with_options(opts, inner)
}

/// Apply a local-override pragma (Set / Push / Pop) to the active
/// [`FmtCtx`]. Off/On/Skip pragmas are no-ops here — those are
/// pre-scanned during [`crate::format`].
///
/// Errors from bad `key = value` payloads are silently ignored —
/// pragmas are user-typed comments, and the alternative of
/// emitting a diagnostic mid-format would couple the formatter to
/// the diagnostic pipeline. A future slice can surface them.
pub(crate) fn apply_pragma_to_ctx(p: &crate::FmtPragma) {
    use crate::FmtPragma::{Next, None, Off, On, Pop, Push, Set, Skip};
    FMT_CTX.with(|c| {
        let mut cx = c.borrow_mut();
        match p {
            Push(key, val) => {
                let prev = cx.opts.clone();
                cx.opts_stack.push(prev);
                let _ = cx.opts.set(key, val);
            }
            Pop => {
                if let Some(prev) = cx.opts_stack.pop() {
                    cx.opts = prev;
                }
            }
            Set(key, val) => {
                let _ = cx.opts.set(key, val);
            }
            // `Next` is handled by the sibling walker (not pushed
            // here) so it can scope the override to exactly one
            // following item.
            Next(_, _) => {}
            Off | On | Skip | None => {}
        }
    });
}

/// True iff the entire `node`'s text range lies inside an active
/// `// fmt: off` region.
pub(crate) fn in_off_region(node: &SyntaxNode) -> bool {
    let range = node.text_range();
    let start = u32::from(range.start());
    let end = u32::from(range.end());
    with_ctx(|cx| {
        cx.off_regions
            .iter()
            .any(|r| r.start <= start && end <= r.end)
    })
}

/// True iff `node`'s immediately preceding sibling trivia is a
/// `// fmt-skip` (or `// fmt: skip`) comment, optionally separated
/// by whitespace only.
///
/// Walks `prev_sibling_or_token()`, which only sees siblings at the
/// same CST level — so a skip comment applies to the next sibling
/// node in its parent (the standard "skip this declaration" use
/// case).
pub(crate) fn is_fmt_skipped(node: &SyntaxNode) -> bool {
    let mut prev = node.prev_sibling_or_token();
    while let Some(el) = prev {
        if let Some(t) = el.as_token() {
            match t.kind() {
                S::Whitespace => {
                    prev = t.prev_sibling_or_token();
                    continue;
                }
                S::LineComment | S::BlockComment => {
                    return crate::is_fmt_skip_marker(t.text());
                }
                _ => return false,
            }
        }
        return false;
    }
    false
}

/// Top-level entry: format the `SourceFile` root.
pub fn format_source_file(root: &SyntaxNode) -> Doc {
    debug_assert_eq!(root.kind(), S::SourceFile);
    let body = blocks::format_top_level(root);
    // Ensure exactly one trailing newline.
    concat([body, hardline()])
}

/// Generic node dispatch — pick the right formatter for a node by
/// its [`SyntaxKind`].
pub(crate) fn fmt_node(node: &SyntaxNode) -> Doc {
    // `// fmt: off` regions and `// fmt-skip`-marked nodes are
    // emitted verbatim. Nothing inside an off region gets
    // reformatted; nothing whose immediately-preceding sibling
    // trivia is `// fmt-skip` does either.
    if in_off_region(node) || is_fmt_skipped(node) {
        return format_raw(node);
    }
    match node.kind() {
        S::Block => blocks::format_block(node),
        S::FnDecl => stmts::format_fn_decl(node),
        S::ClassDecl => stmts::format_class_decl(node),
        S::ClassBody => stmts::format_class_body(node),
        S::ClassField => stmts::format_class_field(node),
        S::ClassMethod => stmts::format_class_method(node),
        S::ClassConstructor => stmts::format_class_constructor(node),
        S::ParamList => stmts::format_param_list(node),
        S::Param => stmts::format_param(node),
        S::IncludeStmt => stmts::format_include_stmt(node),
        S::ImportStmt => stmts::format_import_stmt(node),
        S::VarDeclStmt => stmts::format_var_decl_stmt(node),
        S::ExprStmt => stmts::format_expr_stmt(node),
        S::ReturnStmt => stmts::format_return_stmt(node),
        S::IfStmt => stmts::format_if_stmt(node),
        S::WhileStmt => stmts::format_while_stmt(node),
        S::DoWhileStmt => stmts::format_do_while_stmt(node),
        S::ForStmt => stmts::format_for_stmt(node),
        S::ForeachStmt => stmts::format_foreach_stmt(node),
        S::SwitchStmt => stmts::format_passthrough(node),
        S::BreakStmt | S::ContinueStmt => stmts::format_simple_keyword_stmt(node),
        S::TypeRef => exprs::format_type_ref(node),
        S::Annotation => stmts::format_annotation(node),
        S::ArgList => exprs::format_arg_list(node),
        S::LiteralExpr | S::NameRef => exprs::format_atom(node),
        S::BinaryExpr => exprs::format_binary(node),
        S::UnaryExpr => exprs::format_unary(node),
        S::PostfixExpr => exprs::format_postfix(node),
        S::ParenExpr => exprs::format_paren(node),
        S::CallExpr => exprs::format_call(node),
        S::ArrayExpr => exprs::format_array(node),
        S::SetExpr => exprs::format_set(node),
        S::MapExpr => exprs::format_map(node),
        S::ObjectExpr => exprs::format_object(node),
        S::IndexExpr => exprs::format_index(node),
        S::SliceExpr => exprs::format_slice(node),
        S::FieldExpr => exprs::format_field(node),
        S::LambdaExpr => exprs::format_lambda(node),
        S::NewExpr => exprs::format_new(node),
        S::CastExpr => exprs::format_cast(node),
        S::TernaryExpr => exprs::format_ternary(node),
        S::IntervalExpr => exprs::format_interval(node),
        S::ErrorNode => format_raw(node),
        // Fallback: pass through the node's raw text. Guarantees
        // idempotence for any construct the formatter doesn't yet
        // model explicitly.
        _ => format_raw(node),
    }
}

/// Emit the node's source text verbatim. The catch-all for
/// unhandled constructs and for `ErrorNode` recovery; combined with
/// the fact that any unmodified subtree round-trips, this gives
/// idempotence on broken input.
pub(crate) fn format_raw(node: &SyntaxNode) -> Doc {
    text(node.text().to_string())
}

// ---- Token / element helpers shared by sub-modules ----

/// The separator before a block's opening brace (and before an `else`):
/// a space under [`BraceStyle::SameLine`] (K&R — brace on the header's
/// line) or a hardline under [`BraceStyle::NextLine`] (Allman — brace on
/// its own line). The hardline re-indents to the header's level, so the
/// brace lands directly under it.
///
/// [`BraceStyle::SameLine`]: crate::BraceStyle::SameLine
/// [`BraceStyle::NextLine`]: crate::BraceStyle::NextLine
pub(crate) fn block_lead() -> Doc {
    use crate::BraceStyle;
    match with_ctx(|cx| cx.opts.brace_style) {
        BraceStyle::SameLine => crate::doc::space(),
        BraceStyle::NextLine => hardline(),
    }
}

/// The separator emitted after a comma in element lists. A `line`
/// (space when flat, newline when broken) when `space_after_comma` is
/// on, else a `softline` (nothing when flat).
pub(crate) fn comma_sep() -> Doc {
    let trail = if with_ctx(|cx| cx.opts.space_after_comma) {
        crate::doc::line()
    } else {
        crate::doc::softline()
    };
    concat([text(","), trail])
}

/// A single space when `on`, else nothing — a tiny helper for the
/// optional-spacing toggles (control keyword `(`, arrows).
pub(crate) fn space_if(on: bool) -> Doc {
    if on {
        crate::doc::space()
    } else {
        crate::doc::nil()
    }
}

/// True if `t` is a trivia token (whitespace or comment).
pub(crate) fn is_trivia(t: &SyntaxToken) -> bool {
    t.kind().is_trivia()
}

/// Number of `\n` characters in `s`.
pub(crate) fn count_newlines(s: &str) -> usize {
    s.bytes().filter(|b| *b == b'\n').count()
}

/// Direct child nodes of `node`.
pub(crate) fn child_nodes(node: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> + '_ {
    node.children()
}

/// Render the `text` of a significant token. Kept as a function so
/// a future slice can do per-keyword canonicalization (e.g. `and` →
/// `&&`) in one place.
pub(crate) fn token_text(t: &SyntaxToken) -> Doc {
    text(t.text().to_string())
}

/// Build the trailing-comma [`Doc`] for a multi-element list,
/// honoring [`FormatOptions::trailing_comma`].
///
/// `source_had_comma` is `true` iff the source had a comma between
/// the last element and the closing bracket. Caller is responsible
/// for figuring that out.
///
/// Returned doc is inserted *between* the last element and the
/// closing softline. It uses [`Doc::IfBreak`] so flat-mode output
/// never includes the trailing comma even when policy is `Always`.
pub(crate) fn trailing_comma_doc(source_had_comma: bool) -> Doc {
    use crate::TrailingComma;
    use crate::doc::{ifbreak, nil};
    let want = with_ctx(|cx| match cx.opts.trailing_comma {
        TrailingComma::Always => true,
        TrailingComma::Never => false,
        TrailingComma::Preserve => source_had_comma,
    });
    if want {
        ifbreak(text(""), text(","))
    } else {
        nil()
    }
}

/// Normalize a single-line comment to have a space after `//`
/// (`//x` → `// x`). Leaves doc/inner comments (`///`, `//!`),
/// already-spaced comments, and non-`//` comments (`/* … */`) untouched.
fn pad_line_comment(raw: &str) -> String {
    let Some(rest) = raw.strip_prefix("//") else {
        return raw.to_string();
    };
    // `///` (doc) and `//!` (inner doc) keep their conventional form.
    if matches!(rest.chars().next(), Some('/' | '!')) {
        return raw.to_string();
    }
    if rest.is_empty() || rest.starts_with([' ', '\t']) {
        return raw.to_string();
    }
    format!("// {rest}")
}

/// True if the comment text looks like a documentation comment.
/// Doxygen/Javadoc-style `/** … */` block comments and rustdoc-style
/// `///` line comments both qualify.
pub(crate) fn is_doc_comment(comment_text: &str) -> bool {
    comment_text.starts_with("/**") || comment_text.starts_with("///")
}

/// Render a comment token as a [`Doc`].
///
/// Single-line comments (line comments and single-line block
/// comments) become a single [`Doc::Text`]. Multi-line block
/// comments are split on `\n` and joined with [`Doc::HardLine`] so
/// the printer re-indents continuation lines.
///
/// For doxygen-style multi-line block comments (every continuation
/// line's first non-whitespace character is `*`), continuation lines
/// are re-emitted with a single leading space so the `*` aligns
/// under the `*` of the opening `/**`. This matches rustfmt /
/// clang-format / Prettier.
pub(crate) fn comment_doc(t: &SyntaxToken) -> Doc {
    let raw = t.text();
    if !raw.contains('\n') {
        if with_ctx(|cx| cx.opts.pad_line_comments) {
            return text(pad_line_comment(raw));
        }
        return text(raw.to_string());
    }
    let lines: Vec<&str> = raw.split('\n').collect();
    let is_starred = lines[1..].iter().all(|l| {
        let trimmed = l.trim_start();
        trimmed.is_empty() || trimmed.starts_with('*')
    });

    let mut parts: Vec<Doc> = Vec::with_capacity(lines.len() * 2);
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            parts.push(text(line.to_string()));
            continue;
        }
        parts.push(hardline());
        if is_starred {
            let stripped = line.trim_start();
            if stripped.is_empty() {
                // Blank inner line: emit a bare `*` so the doc block
                // stays visually solid.
                parts.push(text(" *"));
            } else {
                parts.push(text(format!(" {stripped}")));
            }
        } else {
            // Preserve the original line — but trim only trailing
            // whitespace so the relative indent inside the comment is
            // kept.
            parts.push(text(line.trim_end().to_string()));
        }
    }
    concat(parts)
}

#[cfg(test)]
mod ctx_panic_tests {
    use super::*;

    #[test]
    fn ctx_is_restored_even_when_body_panics() {
        // Baseline: a fresh thread starts with default (empty) context.
        assert!(with_ctx(|c| c.off_regions.is_empty()));

        // Install a distinguishable context, then panic inside the body. The
        // drop guard must restore the previous (default) context during unwind.
        let custom = FmtCtx {
            off_regions: std::iter::once(0..1).collect(),
            ..FmtCtx::default()
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_ctx_set(custom, || {
                // Sanity: the custom context is active here.
                assert!(with_ctx(|c| !c.off_regions.is_empty()));
                panic!("boom");
            })
        }));
        assert!(r.is_err(), "body should have panicked");

        // The poisoned context must NOT have leaked past the panic.
        assert!(
            with_ctx(|c| c.off_regions.is_empty()),
            "FmtCtx leaked across a panic — later formats would see stale context",
        );
    }
}
