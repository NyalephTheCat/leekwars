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

/// A per-construct formatter.
type NodeFormatter = fn(&SyntaxNode) -> Doc;

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
    // Kinds without a dedicated formatter pass through verbatim, which
    // guarantees idempotence for anything not modelled explicitly.
    let Some(format) = formatter_for(node.kind()) else {
        return format_raw(node);
    };
    // Safety net: per-construct formatters rebuild their output from
    // child nodes and significant tokens only, so a comment sitting
    // directly inside e.g. a call's argument list would be dropped.
    // Emit such nodes verbatim instead — never lose a comment.
    if has_unplaced_comment(node) {
        return format_verbatim(node);
    }
    format(node)
}

/// The dedicated formatter for `kind`, or `None` for kinds that are
/// emitted verbatim (`switch`, slices, annotations, error recovery and
/// anything not yet modelled).
fn formatter_for(kind: S) -> Option<NodeFormatter> {
    let format: NodeFormatter = match kind {
        S::Block => blocks::format_block,
        S::FnDecl => stmts::format_fn_decl,
        S::ClassDecl => stmts::format_class_decl,
        S::ClassBody => stmts::format_class_body,
        S::ClassField => stmts::format_class_field,
        S::ClassMethod => stmts::format_class_method,
        S::ClassConstructor => stmts::format_class_constructor,
        S::ParamList => stmts::format_param_list,
        S::Param => stmts::format_param,
        S::IncludeStmt => stmts::format_include_stmt,
        S::ImportStmt => stmts::format_import_stmt,
        S::VarDeclStmt => stmts::format_var_decl_stmt,
        S::ExprStmt => stmts::format_expr_stmt,
        S::ReturnStmt => stmts::format_return_stmt,
        S::IfStmt => stmts::format_if_stmt,
        S::WhileStmt => stmts::format_while_stmt,
        S::DoWhileStmt => stmts::format_do_while_stmt,
        S::ForStmt => stmts::format_for_stmt,
        S::ForeachStmt => stmts::format_foreach_stmt,
        S::BreakStmt | S::ContinueStmt => stmts::format_simple_keyword_stmt,
        S::TypeRef => exprs::format_type_ref,
        S::ArgList => exprs::format_arg_list,
        S::LiteralExpr | S::NameRef => exprs::format_atom,
        S::BinaryExpr => exprs::format_binary,
        S::UnaryExpr => exprs::format_unary,
        S::PostfixExpr => exprs::format_postfix,
        S::ParenExpr => exprs::format_paren,
        S::CallExpr => exprs::format_call,
        S::ArrayExpr => exprs::format_array,
        S::SetExpr => exprs::format_set,
        S::SetRangeElement => exprs::format_set_range_element,
        S::MapExpr => exprs::format_map,
        S::ObjectExpr => exprs::format_object,
        S::IndexExpr => exprs::format_index,
        S::FieldExpr => exprs::format_field,
        S::LambdaExpr => exprs::format_lambda,
        S::NewExpr => exprs::format_new,
        S::CastExpr => exprs::format_cast,
        S::TernaryExpr => exprs::format_ternary,
        S::IntervalExpr => exprs::format_interval,
        // `switch` bodies, slices (`a[i:j]`) and annotations keep the
        // user's spacing; `ErrorNode` recovery must round-trip.
        S::SwitchStmt | S::SliceExpr | S::Annotation | S::ErrorNode => return None,
        _ => return None,
    };
    Some(format)
}

/// True if `node` holds a comment token (as a direct child) that its
/// formatter would not emit.
///
/// Comment placement is only implemented by the item-sequence walkers:
/// the top level, the inside of a `{ … }` block and a class body. Every
/// other construct formatter drops direct trivia, so any direct comment
/// there counts as unplaced. Comments deeper in the tree are the
/// business of the child's own [`fmt_node`] call.
pub(crate) fn has_unplaced_comment(node: &SyntaxNode) -> bool {
    let is_comment = |t: &SyntaxToken| matches!(t.kind(), S::LineComment | S::BlockComment);
    let mut tokens = node
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token);
    match node.kind() {
        // The item walkers place every comment they see. A class body's
        // `{` is a token of the enclosing `ClassDecl`, and trivia after
        // its `}` is never attached inside it.
        S::SourceFile | S::ClassBody => false,
        // Only comments between the block's own `{` and `}` are placed;
        // the parser can attach trivia in front of the `{`.
        S::Block => {
            let mut inside = false;
            let mut closed = false;
            for t in tokens {
                match t.kind() {
                    S::LBrace if !inside && !closed => inside = true,
                    S::RBrace if inside => {
                        inside = false;
                        closed = true;
                    }
                    _ if !inside && is_comment(&t) => return true,
                    _ => {}
                }
            }
            false
        }
        _ => tokens.any(|t| is_comment(&t)),
    }
}

/// Emit `node`'s source text verbatim, minus leading and trailing
/// whitespace (the parser can attach surrounding trivia inside a node,
/// and the enclosing formatter supplies its own spacing). The fallback
/// for nodes with [unplaced comments](has_unplaced_comment).
pub(crate) fn format_verbatim(node: &SyntaxNode) -> Doc {
    let body = text(node.text().to_string().trim().to_string());
    // A parameter list's parens are usually tokens of the enclosing
    // function or lambda, whose formatter skips them and relies on
    // `format_param_list` to emit them — so the fallback must too.
    let owns_parens = node
        .descendants_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| !is_trivia(t))
        .is_some_and(|t| t.kind() == S::LParen);
    if node.kind() == S::ParamList && !owns_parens {
        return concat([text("("), body, text(")")]);
    }
    body
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

/// The single child node of `node`, or `None` if it has zero or
/// several. Comment tokens inside also yield `None` — removing the
/// node's delimiters would orphan them.
pub(crate) fn lone_child_node(node: &SyntaxNode) -> Option<SyntaxNode> {
    let has_comments = node
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .any(|t| matches!(t.kind(), S::LineComment | S::BlockComment));
    if has_comments {
        return None;
    }
    let mut nodes = node.children();
    let first = nodes.next()?;
    if nodes.next().is_some() {
        return None;
    }
    Some(first)
}

/// True if parentheses around `inner` can never affect parsing —
/// `inner` is a primary expression (atoms, calls, indexing, field
/// access, collection literals, or another paren). Operators
/// (binary/unary/ternary/cast/lambda/`new`) are excluded: deciding
/// those needs full precedence/associativity context.
pub(crate) fn parens_redundant_around(inner: &SyntaxNode) -> bool {
    matches!(
        inner.kind(),
        S::LiteralExpr
            | S::NameRef
            | S::ParenExpr
            | S::CallExpr
            | S::IndexExpr
            | S::FieldExpr
            | S::ArrayExpr
            | S::MapExpr
            | S::SetExpr
            | S::ObjectExpr
    )
}

/// With `remove_redundant_parens` on, peel `ParenExpr` layers off a
/// node that is *already* delimited by its context (an `if`/`while`
/// condition, a `return` operand): `((x && y))` → `x && y`. Returns
/// the innermost non-paren expression, or `node` itself when the
/// option is off / nothing to peel.
pub(crate) fn peel_context_parens(node: &SyntaxNode) -> SyntaxNode {
    if !with_ctx(|cx| cx.opts.remove_redundant_parens) {
        return node.clone();
    }
    let mut current = node.clone();
    while current.kind() == S::ParenExpr {
        match lone_child_node(&current) {
            Some(inner) => current = inner,
            None => break,
        }
    }
    current
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
/// `&&`) in one place. Today it canonicalizes string-literal quotes
/// per [`FormatOptions::quote_style`].
pub(crate) fn token_text(t: &SyntaxToken) -> Doc {
    if t.kind() == S::StringLiteral {
        let style = with_ctx(|cx| cx.opts.quote_style);
        return text(normalize_quotes(t.text(), style));
    }
    text(t.text().to_string())
}

/// Rewrite a string literal's outer quotes to match `style`,
/// adjusting escapes so the literal's runtime value is unchanged:
/// the old quote char no longer needs its backslash, the new quote
/// char gains one. All other escape sequences pass through verbatim.
///
/// Also the canonical form [`crate::check_equivalence`] compares string
/// literals in, so quote-style rewrites don't count as a change.
pub(crate) fn normalize_quotes(raw: &str, style: crate::QuoteStyle) -> String {
    use crate::QuoteStyle;
    let target = match style {
        QuoteStyle::Preserve => return raw.to_string(),
        QuoteStyle::Double => '"',
        QuoteStyle::Single => '\'',
    };
    let mut chars = raw.chars();
    let Some(open) = chars.next() else {
        return raw.to_string();
    };
    // Not a quoted literal we understand, or already in the target
    // style — emit as-is.
    if (open != '"' && open != '\'') || open == target {
        return raw.to_string();
    }
    let inner: Vec<char> = chars.collect();
    let Some((&close, body)) = inner.split_last() else {
        return raw.to_string();
    };
    if close != open {
        return raw.to_string();
    }

    let mut out = String::with_capacity(raw.len() + 2);
    out.push(target);
    let mut iter = body.iter().copied();
    while let Some(c) = iter.next() {
        if c == '\\' {
            match iter.next() {
                // `\'` → `'` (or `\"` → `"`): the old quote no longer
                // needs escaping inside the new quotes.
                Some(esc) if esc == open => out.push(esc),
                Some(esc) => {
                    out.push('\\');
                    out.push(esc);
                }
                None => out.push('\\'),
            }
        } else if c == target {
            out.push('\\');
            out.push(target);
        } else {
            out.push(c);
        }
    }
    out.push(target);
    out
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
