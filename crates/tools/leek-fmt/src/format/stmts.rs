//! Statement + declaration formatting.

use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind as S, SyntaxNode};

use crate::doc::{Doc, concat, group, hardline, indent, line, softline, space, text};

use super::{
    block_lead, child_nodes, comma_sep, count_newlines, fmt_node, is_trivia, lone_child_node,
    peel_context_parens, space_if, token_text, with_ctx,
};

// ---- Trivial passthroughs / utilities ----

/// Space (or nothing) between a control keyword (`if`/`while`/`for`/…)
/// and its `(`, per `space_after_control_keyword`.
fn ctrl_paren_lead() -> Doc {
    space_if(with_ctx(|cx| cx.opts.space_after_control_keyword))
}

/// The separator before a statement/declaration body. For a braced body
/// (`Block` / `ClassBody`) this honours [`block_lead`] (K&R space vs
/// Allman newline); a non-braced single-statement body just takes a
/// space.
fn body_lead(child: &SyntaxNode) -> Doc {
    if matches!(child.kind(), S::Block | S::ClassBody) {
        block_lead()
    } else {
        space()
    }
}

/// Format a control-statement body (the `then` of an `if`, a loop
/// body, …), separator included, honouring
/// [`FormatOptions::control_braces`]:
///
/// - `always`: an unbraced body gains `{ … }`.
/// - `never`: a `{ … }` around a lone *simple* statement
///   (expression / `return` / `break` / `continue` — nothing that can
///   re-bind a dangling `else`) is dropped; the statement stays on the
///   header's line, or breaks onto its own indented line if too long.
/// - `preserve`: the source's choice stands.
fn format_ctrl_body(child: &SyntaxNode) -> Doc {
    use crate::ControlBraces;
    let policy = with_ctx(|cx| cx.opts.control_braces);
    if child.kind() == S::Block {
        if policy == ControlBraces::Never
            && let Some(stmt) = block_lone_simple_stmt(child)
        {
            return group(indent(1, concat([line(), fmt_node(&stmt)])));
        }
        return concat([block_lead(), fmt_node(child)]);
    }
    if policy == ControlBraces::Always {
        return concat([
            block_lead(),
            text("{"),
            indent(1, concat([hardline(), fmt_node(child)])),
            hardline(),
            text("}"),
        ]);
    }
    concat([space(), fmt_node(child)])
}

/// The lone *simple* statement inside `block`, if that's all it
/// holds (no comments, no second statement). Simple means it can't
/// capture a dangling `else` or carry its own block structure.
fn block_lone_simple_stmt(block: &SyntaxNode) -> Option<SyntaxNode> {
    let stmt = lone_child_node(block)?;
    matches!(
        stmt.kind(),
        S::ExprStmt | S::ReturnStmt | S::BreakStmt | S::ContinueStmt
    )
    .then_some(stmt)
}

/// `;` to append when [`FormatOptions::semicolons`] is `always` and
/// the statement lacked one. Statements sitting directly in a
/// `for (…)` header are skipped — the header owns its `;` layout.
fn maybe_semicolon(node: &SyntaxNode, saw_semi: bool) -> Doc {
    use crate::Semicolons;
    if saw_semi || with_ctx(|cx| cx.opts.semicolons) != Semicolons::Always {
        return crate::doc::nil();
    }
    if node.parent().is_some_and(|p| p.kind() == S::ForStmt) {
        return crate::doc::nil();
    }
    text(";")
}

/// `break;` / `continue;` — keyword + optional semicolon.
pub(super) fn format_simple_keyword_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut saw_semi = false;
    for el in node.children_with_tokens() {
        if let Some(t) = el.as_token() {
            if is_trivia(t) {
                continue;
            }
            saw_semi |= t.kind() == S::Semicolon;
            parts.push(token_text(t));
        }
    }
    parts.push(maybe_semicolon(node, saw_semi));
    concat(parts)
}

// ---- Declarations ----

/// `function name(params) [-> type] { body }`.
pub(super) fn format_fn_decl(node: &SyntaxNode) -> Doc {
    fn_like(node, /* leading_keyword = */ Some("function"))
}

/// `class Name [extends Parent] { class_body }`.
pub(super) fn format_class_decl(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                // ClassBody owns the `{...}` wrapping; skip the
                // brace tokens that are CST siblings.
                S::LBrace | S::RBrace => {}
                S::KwClass => parts.push(text("class")),
                S::Ident => {
                    parts.push(space());
                    parts.push(token_text(&t));
                }
                S::KwExtends => {
                    parts.push(space());
                    parts.push(text("extends"));
                }
                _ => parts.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => match child.kind() {
                S::ClassBody => {
                    parts.push(body_lead(&child));
                    parts.push(fmt_node(&child));
                }
                _ => parts.push(fmt_node(&child)),
            },
        }
    }
    concat(parts)
}

/// `{ class_member* }` — same shape as a block but separated by
/// blank lines so members are visually grouped.
pub(super) fn format_class_body(node: &SyntaxNode) -> Doc {
    let mut members: Vec<Doc> = Vec::new();
    let mut leading: Vec<Doc> = Vec::new();
    let mut between_newlines: usize = 0;
    // Newlines between the previous member and the first leading
    // comment of the next one. The member separator is decided by that
    // gap, not by the newlines after the comments — otherwise a
    // comment on its own line would grow a blank line on every pass.
    let mut gap_before_leading: Option<usize> = None;
    let mut saw_first = false;
    let mut prev_fnlike = false;
    let mut pending_next: Vec<(String, String)> = Vec::new();

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == S::LBrace || t.kind() == S::RBrace => {}
            NodeOrToken::Token(t) if t.kind() == S::Whitespace => {
                between_newlines += count_newlines(t.text());
            }
            NodeOrToken::Token(t) if is_trivia(&t) => {
                // Like the block walker: pragmas drive the formatter's
                // state and still reach the output (#367).
                let pragma = crate::parse_fmt_pragma(t.text());
                match &pragma {
                    crate::FmtPragma::None => {}
                    crate::FmtPragma::Next(k, v) => pending_next.push((k.clone(), v.clone())),
                    p => super::apply_pragma_to_ctx(p),
                }
                gap_before_leading.get_or_insert(between_newlines);
                between_newlines = 0;
                leading.push(token_text(&t));
            }
            NodeOrToken::Token(t) => {
                // A significant token the parser could not fold into a
                // member — a modifier on a member it failed to
                // classify, e.g. `class A { static for … }`. The
                // parser bumps the modifier before deciding what the
                // member is, so it stays a direct child of ClassBody.
                // It is still the user's code: emit it like the block
                // walker does (#414) rather than dropping it.
                let gap = gap_before_leading.take().unwrap_or(between_newlines);
                if saw_first {
                    members.push(if gap >= 2 {
                        crate::doc::blank_line()
                    } else {
                        hardline()
                    });
                }
                prev_fnlike = false;
                for c in leading.drain(..) {
                    members.push(c);
                    members.push(hardline());
                }
                members.push(token_text(&t));
                saw_first = true;
                between_newlines = 0;
            }
            NodeOrToken::Node(child) => {
                let fnlike = matches!(child.kind(), S::ClassMethod | S::ClassConstructor);
                let gap = gap_before_leading.take().unwrap_or(between_newlines);
                if saw_first {
                    // `blank_line_between_functions` lifts the gap on
                    // either side of a method/constructor to a blank.
                    let force_blank = (fnlike || prev_fnlike)
                        && with_ctx(|cx| {
                            cx.opts.blank_line_between_functions && cx.opts.max_blank_lines >= 1
                        });
                    members.push(if gap >= 2 || force_blank {
                        crate::doc::blank_line()
                    } else {
                        hardline()
                    });
                }
                prev_fnlike = fnlike;
                for c in leading.drain(..) {
                    members.push(c);
                    members.push(hardline());
                }
                let member_doc = if pending_next.is_empty() {
                    fmt_node(&child)
                } else {
                    let d = super::fmt_node_with_next_overrides(&child, &pending_next);
                    pending_next.clear();
                    d
                };
                // Same idea as the block-level walker: capture the
                // currently-active opts so print-time settings
                // honored at this member's site flow into the
                // printer.
                let member_doc = super::wrap_with_active_opts(member_doc);
                members.push(member_doc);
                saw_first = true;
                between_newlines = 0;
            }
        }
    }

    if !saw_first && leading.is_empty() {
        return text("{}");
    }
    // Comments after the last member (or in a comment-only body) have
    // no member to lead; keep them in front of the closing brace
    // rather than dropping them.
    for comment in leading.drain(..) {
        if !members.is_empty() {
            members.push(hardline());
        }
        members.push(comment);
    }

    let body = concat(members);
    concat([
        text("{"),
        indent(1, concat([hardline(), body])),
        hardline(),
        text("}"),
    ])
}

/// `[modifiers] [type] name [= expr] [;]`.
pub(super) fn format_class_field(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut emitted_anything = false;
    let mut seen_eq = false;
    let mut saw_semi = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::Semicolon => {
                    parts.push(text(";"));
                    saw_semi = true;
                }
                S::Eq => {
                    parts.push(space());
                    parts.push(text("="));
                    parts.push(space());
                    seen_eq = true;
                }
                S::Comma => {
                    parts.push(text(","));
                    parts.push(space());
                }
                _ => {
                    if emitted_anything && !matches!(parts.last(), Some(Doc::Text(s)) if s == " ") {
                        parts.push(space());
                    }
                    parts.push(token_text(&t));
                    emitted_anything = true;
                }
            },
            NodeOrToken::Node(child) => {
                if !seen_eq
                    && !matches!(parts.last(), Some(Doc::Text(s)) if s == " ")
                    && emitted_anything
                {
                    parts.push(space());
                }
                parts.push(fmt_node(&child));
                emitted_anything = true;
                seen_eq = false;
            }
        }
    }
    parts.push(maybe_semicolon(node, saw_semi));
    concat(parts)
}

/// `[modifiers] [type] name(params) [-> type] { body }`.
pub(super) fn format_class_method(node: &SyntaxNode) -> Doc {
    fn_like(node, None)
}

/// `constructor(params) { body }`.
pub(super) fn format_class_constructor(node: &SyntaxNode) -> Doc {
    fn_like(node, None)
}

/// Shared implementation for fn-decl, class-method, class-constructor.
///
/// `leading_keyword`:
/// - `Some(kw)` for `FnDecl` (keyword is already a token in the
///   node; we accept and emit it).
/// - `None` for methods and constructors (the keyword may be
///   `constructor`, or absent for typed-method declarations).
fn fn_like(node: &SyntaxNode, _leading_keyword: Option<&'static str>) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_space = true;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                // ParamList owns the `(...)` wrapping; skip these
                // CST siblings of ParamList so we don't double-paren.
                S::LParen | S::RParen => {}
                S::Arrow | S::FatArrow => {
                    let on = with_ctx(|cx| cx.opts.space_around_arrow);
                    if on && !last_was_space {
                        parts.push(space());
                    }
                    parts.push(token_text(&t));
                    if on {
                        parts.push(space());
                    }
                    last_was_space = on;
                }
                _ => {
                    if !last_was_space && !parts.is_empty() {
                        parts.push(space());
                    }
                    parts.push(token_text(&t));
                    last_was_space = false;
                }
            },
            NodeOrToken::Node(child) => {
                if child.kind() == S::Block {
                    // The function/method/constructor body — honour the
                    // brace style (space or next-line).
                    parts.push(block_lead());
                    parts.push(fmt_node(&child));
                    last_was_space = true;
                    continue;
                }
                let needs_space_before = match child.kind() {
                    S::ParamList => false, // attaches right after the name
                    S::TypeRef => false,   // already preceded by `->` + space
                    _ => !last_was_space,
                };
                if needs_space_before && !last_was_space {
                    parts.push(space());
                }
                parts.push(fmt_node(&child));
                last_was_space = false;
            }
        }
    }
    concat(parts)
}

/// The `Param` children of a parameter list, each already formatted.
fn param_docs(node: &SyntaxNode) -> Vec<Doc> {
    child_nodes(node)
        .filter(|n| n.kind() == S::Param)
        .map(|n| fmt_node(&n))
        .collect()
}

/// `(param, param, ...)` — call-style break-on-overflow group.
pub(super) fn format_param_list(node: &SyntaxNode) -> Doc {
    let params = param_docs(node);

    if params.is_empty() {
        return text("()");
    }

    let inner = crate::doc::join(&comma_sep(), params);
    group(concat([
        text("("),
        indent(1, concat([softline(), inner])),
        softline(),
        text(")"),
    ]))
}

/// `param, param, ...` — the parameters alone, with no parens of their
/// own.
///
/// The usual [`format_param_list`] synthesizes the `(`…`)` pair because
/// the enclosing function/lambda formatter drops the literal delimiter
/// tokens. That is wrong for the legacy `(params -> body)` lambda, whose
/// parens wrap the *whole* lambda rather than the parameters: there the
/// enclosing formatter keeps its own tokens and needs the bare form, or
/// the output grows a second, misplaced pair.
pub(super) fn param_list_inner(node: &SyntaxNode) -> Doc {
    // Reaching this formatter directly bypasses `fmt_node`'s comment
    // safety net, so apply it here: a comment sitting in the list would
    // otherwise be dropped. The raw text is already paren-free.
    if super::has_unplaced_comment(node) {
        return super::format_raw(node);
    }
    let params = param_docs(node);
    if params.is_empty() {
        return text("");
    }
    group(crate::doc::join(&comma_sep(), params))
}

/// `[@] [type] IDENT [= expr]`.
pub(super) fn format_param(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_space = true;
    let mut seen_eq = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::At => {
                    parts.push(text("@"));
                    last_was_space = false;
                }
                S::Eq => {
                    parts.push(space());
                    parts.push(text("="));
                    parts.push(space());
                    last_was_space = true;
                    seen_eq = true;
                }
                _ => {
                    if !last_was_space && !parts.is_empty() {
                        parts.push(space());
                    }
                    parts.push(token_text(&t));
                    last_was_space = false;
                }
            },
            NodeOrToken::Node(child) => {
                if !seen_eq && !last_was_space {
                    parts.push(space());
                }
                parts.push(fmt_node(&child));
                last_was_space = false;
                seen_eq = false;
            }
        }
    }
    concat(parts)
}

/// `include("…");`
pub(super) fn format_include_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut saw_semi = false;
    for el in node.children_with_tokens() {
        if let Some(t) = el.as_token() {
            if is_trivia(t) {
                continue;
            }
            saw_semi |= t.kind() == S::Semicolon;
            match t.kind() {
                S::KwInclude => parts.push(text("include")),
                _ => parts.push(token_text(t)),
            }
        }
    }
    parts.push(maybe_semicolon(node, saw_semi));
    concat(parts)
}

/// `import foo.bar;` / `import("foo.bar");`
pub(super) fn format_import_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut saw_semi = false;
    for el in node.children_with_tokens() {
        if let Some(t) = el.as_token() {
            if is_trivia(t) {
                continue;
            }
            saw_semi |= t.kind() == S::Semicolon;
            match t.kind() {
                S::KwImport => parts.push(text("import")),
                _ => parts.push(token_text(t)),
            }
        }
    }
    parts.push(maybe_semicolon(node, saw_semi));
    concat(parts)
}

/// `var x = …;` / `integer x = …;` / `global …;`.
pub(super) fn format_var_decl_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_space = true;
    let mut seen_eq = false;
    let mut saw_semi = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwVar | S::KwGlobal | S::Ident => {
                    if !last_was_space && !parts.is_empty() {
                        parts.push(space());
                    }
                    parts.push(token_text(&t));
                    last_was_space = false;
                    seen_eq = false;
                }
                S::Eq => {
                    parts.push(space());
                    parts.push(text("="));
                    parts.push(space());
                    last_was_space = true;
                    seen_eq = true;
                }
                S::Comma => {
                    parts.push(text(","));
                    if with_ctx(|cx| cx.opts.space_after_comma) {
                        parts.push(space());
                        last_was_space = true;
                    } else {
                        last_was_space = false;
                    }
                }
                S::Semicolon => {
                    parts.push(text(";"));
                    last_was_space = false;
                    saw_semi = true;
                }
                _ => {
                    if !last_was_space && !parts.is_empty() {
                        parts.push(space());
                    }
                    parts.push(token_text(&t));
                    last_was_space = false;
                }
            },
            NodeOrToken::Node(child) => {
                if !seen_eq && !last_was_space {
                    parts.push(space());
                }
                if seen_eq {
                    // The RHS expression is a candidate for breaking.
                    parts.push(group(fmt_node(&child)));
                } else {
                    parts.push(fmt_node(&child));
                }
                last_was_space = false;
                seen_eq = false;
            }
        }
    }
    parts.push(maybe_semicolon(node, saw_semi));
    concat(parts)
}

/// `expr;`.
pub(super) fn format_expr_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut saw_semi = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => {
                saw_semi |= t.kind() == S::Semicolon;
                parts.push(token_text(&t));
            }
            NodeOrToken::Node(n) => parts.push(group(fmt_node(&n))),
        }
    }
    parts.push(maybe_semicolon(node, saw_semi));
    concat(parts)
}

/// `return [?] [expr];`.
pub(super) fn format_return_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = vec![text("return")];
    let mut emitted_expr = false;
    let mut saw_semi = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwReturn => {}
                S::Question if !emitted_expr => {
                    parts.push(space());
                    parts.push(text("?"));
                }
                S::Semicolon => {
                    parts.push(text(";"));
                    saw_semi = true;
                }
                _ => {
                    parts.push(space());
                    parts.push(token_text(&t));
                }
            },
            NodeOrToken::Node(child) => {
                parts.push(space());
                // `return (expr);` — the parens add nothing; the `;`
                // (or EOL) already delimits the operand.
                parts.push(group(fmt_node(&peel_context_parens(&child))));
                emitted_expr = true;
            }
        }
    }
    parts.push(maybe_semicolon(node, saw_semi));
    concat(parts)
}

/// `if (cond) then [else other]`. Handles `else if` chains by
/// recursing into the else branch.
pub(super) fn format_if_stmt(node: &SyntaxNode) -> Doc {
    // Walk in source order so we keep the original cond / then /
    // else relationship without relying on AST accessors that may
    // not exist yet for all forms.
    let mut parts: Vec<Doc> = Vec::new();
    let mut seen_kw_if = false;
    let mut seen_lparen = false;
    let mut seen_rparen = false;
    let mut seen_else = false;
    let mut cond_seen = false;
    let mut then_seen = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwIf => {
                    if seen_else {
                        // `else if` continuation
                        parts.push(space());
                    }
                    parts.push(text("if"));
                    seen_kw_if = true;
                    seen_else = false;
                }
                S::LParen if seen_kw_if && !seen_rparen => {
                    parts.push(ctrl_paren_lead());
                    parts.push(text("("));
                    seen_lparen = true;
                }
                S::RParen if seen_lparen && !seen_rparen => {
                    parts.push(text(")"));
                    seen_rparen = true;
                }
                S::KwElse => {
                    // Allman puts `else` on its own line after the `}`.
                    parts.push(block_lead());
                    parts.push(text("else"));
                    seen_else = true;
                }
                _ => parts.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                if !cond_seen && seen_lparen && !seen_rparen {
                    parts.push(group(fmt_node(&peel_context_parens(&child))));
                    cond_seen = true;
                } else if !then_seen && seen_rparen && !seen_else {
                    parts.push(format_ctrl_body(&child));
                    then_seen = true;
                } else if seen_else {
                    parts.push(format_else_branch(&child));
                    seen_else = false;
                    // Reset for potential subsequent `else if` continuation.
                    seen_kw_if = false;
                    seen_lparen = false;
                    seen_rparen = false;
                    cond_seen = false;
                    then_seen = false;
                } else {
                    parts.push(fmt_node(&child));
                }
            }
        }
    }
    concat(parts)
}

/// The body of an `else`. An `else if` continuation stays on the
/// `else`'s line; with `collapse_else_if`, an `else { if … }` block
/// holding exactly that `if` (and no comments) is flattened to the
/// same shape. Everything else goes through [`format_ctrl_body`].
fn format_else_branch(child: &SyntaxNode) -> Doc {
    if child.kind() == S::IfStmt {
        return concat([space(), fmt_node(child)]);
    }
    if with_ctx(|cx| cx.opts.collapse_else_if)
        && child.kind() == S::Block
        && let Some(inner) = lone_child_node(child)
        && inner.kind() == S::IfStmt
    {
        return concat([space(), fmt_node(&inner)]);
    }
    format_ctrl_body(child)
}

/// `while (cond) body`.
pub(super) fn format_while_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut seen_lparen = false;
    let mut seen_rparen = false;
    let mut cond_seen = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwWhile => parts.push(text("while")),
                S::LParen if !seen_lparen => {
                    parts.push(ctrl_paren_lead());
                    parts.push(text("("));
                    seen_lparen = true;
                }
                S::RParen if !seen_rparen => {
                    parts.push(text(")"));
                    seen_rparen = true;
                }
                _ => parts.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                if !cond_seen && seen_lparen && !seen_rparen {
                    parts.push(group(fmt_node(&peel_context_parens(&child))));
                    cond_seen = true;
                } else {
                    parts.push(format_ctrl_body(&child));
                }
            }
        }
    }
    concat(parts)
}

/// `do body while (cond) [;]`.
pub(super) fn format_do_while_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut emitted_do = false;
    let mut seen_while = false;
    let mut seen_lparen = false;
    let mut seen_rparen = false;
    let mut saw_semi = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwDo => {
                    parts.push(text("do"));
                    emitted_do = true;
                }
                S::KwWhile => {
                    parts.push(space());
                    parts.push(text("while"));
                    seen_while = true;
                }
                S::LParen if seen_while && !seen_lparen => {
                    parts.push(ctrl_paren_lead());
                    parts.push(text("("));
                    seen_lparen = true;
                }
                S::RParen if !seen_rparen => {
                    parts.push(text(")"));
                    seen_rparen = true;
                }
                S::Semicolon => {
                    parts.push(text(";"));
                    saw_semi = true;
                }
                _ => parts.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                if seen_while {
                    parts.push(group(fmt_node(&peel_context_parens(&child))));
                } else if emitted_do {
                    parts.push(format_ctrl_body(&child));
                } else {
                    parts.push(fmt_node(&child));
                }
            }
        }
    }
    parts.push(maybe_semicolon(node, saw_semi));
    concat(parts)
}

/// `for (init; cond; step) body` — C-style.
///
/// The init may be a `VarDeclStmt`/`ExprStmt` (which carry their
/// own trailing `;` internally) or absent (a bare `;` direct
/// child). Between the cond and step there is always a direct
/// `Semicolon` child of `ForStmt`. We walk children in order and
/// add a space after each `;` (whether internal or external).
pub(super) fn format_for_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut paren_depth = 0i32;
    let mut in_header = false;
    let mut just_emitted_semi_in_header = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwFor => parts.push(text("for")),
                S::LParen if paren_depth == 0 => {
                    parts.push(ctrl_paren_lead());
                    parts.push(text("("));
                    in_header = true;
                    paren_depth += 1;
                    just_emitted_semi_in_header = false;
                }
                S::RParen if paren_depth == 1 => {
                    parts.push(text(")"));
                    paren_depth -= 1;
                    in_header = false;
                }
                S::LParen => {
                    parts.push(text("("));
                    paren_depth += 1;
                }
                S::RParen => {
                    parts.push(text(")"));
                    paren_depth -= 1;
                }
                S::Semicolon if in_header && paren_depth == 1 => {
                    parts.push(text(";"));
                    just_emitted_semi_in_header = true;
                }
                _ => parts.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                if in_header {
                    if just_emitted_semi_in_header {
                        parts.push(space());
                        just_emitted_semi_in_header = false;
                    }
                    let child_doc = fmt_node(&child);
                    // A child node (VarDeclStmt / ExprStmt) ending
                    // with its own `;` should be followed by a space
                    // before the next clause's first token.
                    let ends_with_semi =
                        child.last_token().is_some_and(|t| t.kind() == S::Semicolon);
                    parts.push(child_doc);
                    if ends_with_semi {
                        parts.push(space());
                    }
                } else {
                    parts.push(format_ctrl_body(&child));
                }
            }
        }
    }
    concat(parts)
}

/// `for (binding [: binding] in iter) body`.
pub(super) fn format_foreach_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut paren_depth = 0i32;
    let mut in_header = false;
    let mut last_was_space = true;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwFor => {
                    parts.push(text("for"));
                    last_was_space = false;
                }
                S::LParen if paren_depth == 0 => {
                    parts.push(ctrl_paren_lead());
                    parts.push(text("("));
                    paren_depth += 1;
                    in_header = true;
                    // After `(`, no extra space before the next token.
                    last_was_space = true;
                }
                S::RParen if paren_depth == 1 => {
                    parts.push(text(")"));
                    paren_depth -= 1;
                    in_header = false;
                    last_was_space = false;
                }
                S::LParen => {
                    paren_depth += 1;
                    parts.push(text("("));
                    last_was_space = true;
                }
                S::RParen => {
                    paren_depth -= 1;
                    parts.push(text(")"));
                    last_was_space = false;
                }
                S::Colon => {
                    parts.push(space());
                    parts.push(text(":"));
                    parts.push(space());
                    last_was_space = true;
                }
                S::At => {
                    parts.push(text("@"));
                    last_was_space = false;
                }
                S::KwIn => {
                    parts.push(space());
                    parts.push(text("in"));
                    parts.push(space());
                    last_was_space = true;
                }
                S::KwVar | S::Ident => {
                    if !last_was_space {
                        parts.push(space());
                    }
                    parts.push(token_text(&t));
                    last_was_space = false;
                }
                _ => parts.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                if in_header {
                    if !last_was_space {
                        parts.push(space());
                    }
                    parts.push(fmt_node(&child));
                    last_was_space = false;
                } else {
                    parts.push(format_ctrl_body(&child));
                    last_was_space = false;
                }
            }
        }
    }
    concat(parts)
}

// ---- Switch ----

/// `switch (expr) { case … }`.
///
/// Shaped exactly like [`format_block`]: the `{` follows
/// [`block_lead`] (a space under K&R, a newline under Allman), the
/// arms sit one indent level in, and the `}` comes back out to the
/// `switch`'s own column — so a switch written at some other
/// indentation is re-indented to where it now sits, like every other
/// braced construct.
///
/// A comment sitting directly in the body never reaches here:
/// [`fmt_node`] hands such a node to `format_verbatim` first, which is
/// what keeps a `// fallthrough` between two arms from being dropped.
///
/// [`format_block`]: super::blocks::format_block
/// [`fmt_node`]: super::fmt_node
pub(super) fn format_switch_stmt(node: &SyntaxNode) -> Doc {
    debug_assert_eq!(node.kind(), S::SwitchStmt);
    let mut header: Vec<Doc> = Vec::new();
    let mut body: Vec<Doc> = Vec::new();
    let mut seen_lparen = false;
    let mut seen_rparen = false;
    let mut scrutinee_seen = false;
    let mut in_body = false;
    let mut newlines: usize = 0;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == S::Whitespace => {
                newlines += count_newlines(t.text());
            }
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwSwitch => header.push(text("switch")),
                S::LParen if !seen_lparen => {
                    header.push(ctrl_paren_lead());
                    header.push(text("("));
                    seen_lparen = true;
                }
                S::RParen if seen_lparen && !seen_rparen => {
                    header.push(text(")"));
                    seen_rparen = true;
                }
                S::LBrace if !in_body => in_body = true,
                // Everything after the body's `}` belongs to no arm;
                // this formatter re-emits both braces itself.
                S::RBrace if in_body => break,
                _ if in_body => push_item(&mut body, &mut newlines, token_text(&t)),
                _ => header.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                if in_body {
                    // `SwitchCase` arms, plus any statement the parser
                    // found in the body before the first `case`.
                    newlines += leading_newlines(&child);
                    push_item(&mut body, &mut newlines, fmt_node(&child));
                } else if seen_lparen && !seen_rparen && !scrutinee_seen {
                    header.push(group(fmt_node(&peel_context_parens(&child))));
                    scrutinee_seen = true;
                } else {
                    header.push(fmt_node(&child));
                }
            }
        }
    }

    let head = concat([concat(header), block_lead()]);
    if body.is_empty() {
        return concat([head, text("{}")]);
    }
    concat([
        head,
        text("{"),
        indent(1, concat([hardline(), concat(body)])),
        hardline(),
        text("}"),
    ])
}

/// One arm of a switch: `case <expr>:` (or `default:`) on its own
/// line, then the arm's statements one indent level in.
///
/// An arm owns every statement up to the next `case` / `default` / `}`,
/// so the indent is this formatter's to place; the arm itself is placed
/// by [`format_switch_stmt`], which keeps the blank lines the source
/// had between arms.
pub(super) fn format_switch_case(node: &SyntaxNode) -> Doc {
    debug_assert_eq!(node.kind(), S::SwitchCase);
    let mut label: Vec<Doc> = Vec::new();
    let mut body: Vec<Doc> = Vec::new();
    let mut is_case = false;
    let mut label_expr_seen = false;
    let mut seen_colon = false;
    let mut newlines: usize = 0;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == S::Whitespace => {
                newlines += count_newlines(t.text());
            }
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwCase => {
                    label.push(text("case"));
                    is_case = true;
                }
                S::KwDefault => label.push(text("default")),
                S::Colon if !seen_colon => {
                    label.push(text(":"));
                    seen_colon = true;
                }
                _ if seen_colon => push_item(&mut body, &mut newlines, token_text(&t)),
                _ => {
                    label.push(space());
                    label.push(token_text(&t));
                }
            },
            NodeOrToken::Node(child) => {
                if is_case && !seen_colon && !label_expr_seen {
                    label.push(space());
                    label.push(group(fmt_node(&child)));
                    label_expr_seen = true;
                } else {
                    newlines += leading_newlines(&child);
                    push_item(&mut body, &mut newlines, fmt_node(&child));
                }
            }
        }
    }

    let head = concat(label);
    if body.is_empty() {
        // `case 3:` with nothing of its own — the next arm follows on
        // the next line.
        return head;
    }
    concat([head, indent(1, concat([hardline(), concat(body)]))])
}

/// Append one item to a switch body (an arm) or an arm body (a
/// statement), separated from the previous one by what `newlines` —
/// the newline count of the whitespace between them — calls for.
/// `newlines` is consumed: the count restarts for the next gap.
fn push_item(items: &mut Vec<Doc>, newlines: &mut usize, item: Doc) {
    if !items.is_empty() {
        items.push(item_separator(*newlines));
    }
    *newlines = 0;
    items.push(item);
}

/// Newlines in the whitespace the parser attached *inside* `node`, in
/// front of its first token.
///
/// A switch arm's leading trivia lands inside the arm: `switch_case`
/// opens the `SwitchCase` node before consuming the `case` keyword
/// whose bump flushes the pending trivia. So the blank line a user left
/// between two arms is only visible from in there, and a walker that
/// counted the whitespace between siblings alone would swallow it.
fn leading_newlines(node: &SyntaxNode) -> usize {
    node.children_with_tokens()
        .map_while(|el| el.into_token().filter(|t| t.kind() == S::Whitespace))
        .map(|t| count_newlines(t.text()))
        .sum()
}

/// Blank line or hardline between two items, from the number of
/// newlines the source had between them.
///
/// The rule the block walker's item sequence applies between
/// statements, [`FormatOptions::max_blank_lines`] included: with no
/// blank lines allowed, even a `\n\n+` run comes back as a single
/// hardline.
///
/// [`FormatOptions::max_blank_lines`]: crate::FormatOptions::max_blank_lines
fn item_separator(newlines: usize) -> Doc {
    if newlines >= 2 && with_ctx(|cx| cx.opts.max_blank_lines) >= 1 {
        crate::doc::blank_line()
    } else {
        hardline()
    }
}

#[cfg(test)]
mod switch_tests {
    use leek_span::SourceId;
    use leek_syntax::Version;

    use crate::{FormatOptions, format_source_checked};

    /// Format with the equivalence net in circuit, so a lost comment
    /// fails the test here rather than being asserted around.
    fn fmt(src: &str) -> String {
        format_source_checked(
            src,
            SourceId::new(1).expect("1 is a valid source id"),
            Version::V4,
            &FormatOptions::default(),
        )
        .expect("formatting a switch must keep every comment and token")
    }

    /// The common shape this slice must not break.
    ///
    /// The parser attaches an arm's leading trivia *inside* the arm
    /// that follows it, so a `// fallthrough` note lands as a direct
    /// child of the next `SwitchCase` — where `format_switch_case`,
    /// which rebuilds its output from the label and the statements,
    /// would drop it. It never gets the chance: [`fmt_node`]'s
    /// unplaced-comment guard runs before dispatch and hands that arm
    /// to `format_verbatim`. Coming out verbatim is a fine outcome;
    /// coming out without the comment would be the worst one.
    ///
    /// [`fmt_node`]: super::super::fmt_node
    #[test]
    fn a_fallthrough_comment_between_two_arms_survives() {
        let src = "switch (x) {\ncase 1:\na();\n// fallthrough\ncase 2:\nb();\nbreak;\n}\n";
        let out = fmt(src);
        assert!(out.contains("// fallthrough"), "{out}");
        assert!(out.contains("case 2"), "{out}");
        assert!(out.contains("b();"), "{out}");
    }

    /// The same guard one level up: a comment after the last arm's
    /// statements is flushed into the `SwitchStmt` itself (the `}`'s
    /// bump is what emits it), so the whole switch goes out verbatim
    /// rather than the comment going nowhere.
    #[test]
    fn a_comment_before_the_closing_brace_survives() {
        let src = "switch (x) {\ncase 1:\na();\n// done\n}\n";
        let out = fmt(src);
        assert!(out.contains("// done"), "{out}");
        assert!(out.contains("a();"), "{out}");
    }

    /// A comment in the header — between the scrutinee and the `{` —
    /// is a direct child of the `SwitchStmt` too.
    #[test]
    fn a_comment_in_the_header_survives() {
        let src = "switch (x) /* why */ {\ncase 1:\na();\n}\n";
        let out = fmt(src);
        assert!(out.contains("/* why */"), "{out}");
    }

    /// Falling back to verbatim must not oscillate: the second pass
    /// over the first pass's output is a no-op, comments and all.
    #[test]
    fn the_verbatim_fallback_is_idempotent() {
        for src in [
            "function f(x) {\nswitch (x) {\ncase 1:\na();\n// fallthrough\ncase 2:\nb();\n}\n}\n",
            "function f(x) {\nswitch (x) {\ncase 1:\na();\n// done\n}\n}\n",
        ] {
            let once = fmt(src);
            let twice = fmt(&once);
            assert_eq!(once, twice, "not idempotent for {src:?}");
        }
    }

    /// With no comment in the way the real formatter runs: the switch
    /// is re-indented to the block it now sits in, arms one level in
    /// and statements one level inside those, whatever column the
    /// source had them at.
    #[test]
    fn a_switch_is_reindented_to_the_block_it_sits_in() {
        let src = "function f(x) {\n      switch (x) {\n  case 1:\n        a();\nbreak;\n}\n}\n";
        assert_eq!(
            fmt(src),
            "function f(x) {\n    switch (x) {\n        case 1:\n            a();\n            \
             break;\n    }\n}\n",
        );
    }

    /// A switch whose body the parser never saw closed keeps the
    /// user's text: rebuilding the braces from constants would
    /// manufacture a `}` nobody wrote (#415, #417).
    #[test]
    fn an_unclosed_switch_body_round_trips() {
        let src = "switch (x) {\ncase 1:\na();\n";
        let out = fmt(src);
        assert!(!out.contains('}'), "a `}}` nobody wrote: {out}");
        assert!(out.contains("case 1:"), "{out}");
    }
}
