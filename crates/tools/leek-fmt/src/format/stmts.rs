//! Statement + declaration formatting.

use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind as S, SyntaxNode};

use crate::doc::{Doc, concat, group, hardline, indent, softline, space, text};

use super::{
    block_lead, child_nodes, comma_sep, count_newlines, fmt_node, format_raw, is_trivia, space_if,
    token_text, with_ctx,
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

pub(super) fn format_passthrough(node: &SyntaxNode) -> Doc {
    format_raw(node)
}

/// `break;` / `continue;` — keyword + optional semicolon.
pub(super) fn format_simple_keyword_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        if let Some(t) = el.as_token() {
            if is_trivia(t) {
                continue;
            }
            parts.push(token_text(t));
        }
    }
    concat(parts)
}

/// `@name` or `@name(args)` annotation. Preserved verbatim.
pub(super) fn format_annotation(node: &SyntaxNode) -> Doc {
    format_raw(node)
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
    let mut saw_first = false;
    let mut pending_next: Vec<(String, String)> = Vec::new();

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if t.kind() == S::LBrace || t.kind() == S::RBrace => {}
            NodeOrToken::Token(t) if t.kind() == S::Whitespace => {
                between_newlines += count_newlines(t.text());
            }
            NodeOrToken::Token(t) if is_trivia(&t) => {
                let pragma = crate::parse_fmt_pragma(t.text());
                if let crate::FmtPragma::Next(k, v) = &pragma {
                    pending_next.push((k.clone(), v.clone()));
                    continue;
                }
                if pragma != crate::FmtPragma::None {
                    super::apply_pragma_to_ctx(&pragma);
                    continue;
                }
                leading.push(token_text(&t));
            }
            NodeOrToken::Token(_) => {}
            NodeOrToken::Node(child) => {
                if saw_first {
                    members.push(if between_newlines >= 2 {
                        crate::doc::blank_line()
                    } else {
                        hardline()
                    });
                }
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

    if !saw_first {
        return text("{}");
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

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::Semicolon => parts.push(text(";")),
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

/// `(param, param, ...)` — call-style break-on-overflow group.
pub(super) fn format_param_list(node: &SyntaxNode) -> Doc {
    let params: Vec<Doc> = child_nodes(node)
        .filter(|n| n.kind() == S::Param)
        .map(|n| fmt_node(&n))
        .collect();

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
    for el in node.children_with_tokens() {
        if let Some(t) = el.as_token() {
            if is_trivia(t) {
                continue;
            }
            match t.kind() {
                S::KwInclude => parts.push(text("include")),
                _ => parts.push(token_text(t)),
            }
        }
    }
    concat(parts)
}

/// `import foo.bar;` / `import("foo.bar");`
pub(super) fn format_import_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        if let Some(t) = el.as_token() {
            if is_trivia(t) {
                continue;
            }
            match t.kind() {
                S::KwImport => parts.push(text("import")),
                _ => parts.push(token_text(t)),
            }
        }
    }
    concat(parts)
}

/// `var x = …;` / `integer x = …;` / `global …;`.
pub(super) fn format_var_decl_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_space = true;
    let mut seen_eq = false;

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
    concat(parts)
}

/// `expr;`.
pub(super) fn format_expr_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => parts.push(token_text(&t)),
            NodeOrToken::Node(n) => parts.push(group(fmt_node(&n))),
        }
    }
    concat(parts)
}

/// `return [?] [expr];`.
pub(super) fn format_return_stmt(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = vec![text("return")];
    let mut emitted_question = false;
    let mut emitted_expr = false;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwReturn => {}
                S::Question if !emitted_expr => {
                    parts.push(space());
                    parts.push(text("?"));
                    emitted_question = true;
                }
                S::Semicolon => parts.push(text(";")),
                _ => {
                    parts.push(space());
                    parts.push(token_text(&t));
                }
            },
            NodeOrToken::Node(child) => {
                parts.push(space());
                parts.push(group(fmt_node(&child)));
                emitted_expr = true;
                emitted_question = true;
            }
        }
    }
    let _ = emitted_question;
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
                    parts.push(group(fmt_node(&child)));
                    cond_seen = true;
                } else if !then_seen && seen_rparen && !seen_else {
                    parts.push(body_lead(&child));
                    parts.push(fmt_node(&child));
                    then_seen = true;
                } else if seen_else {
                    parts.push(body_lead(&child));
                    parts.push(fmt_node(&child));
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
                    parts.push(group(fmt_node(&child)));
                    cond_seen = true;
                } else {
                    parts.push(body_lead(&child));
                    parts.push(fmt_node(&child));
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
                S::Semicolon => parts.push(text(";")),
                _ => parts.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                if seen_while {
                    parts.push(group(fmt_node(&child)));
                } else {
                    if emitted_do {
                        parts.push(body_lead(&child));
                    }
                    parts.push(fmt_node(&child));
                }
            }
        }
    }
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
                    parts.push(body_lead(&child));
                    parts.push(fmt_node(&child));
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
                    parts.push(body_lead(&child));
                    parts.push(fmt_node(&child));
                    last_was_space = false;
                }
            }
        }
    }
    concat(parts)
}
