//! Expression formatting.

use leek_syntax::language::NodeOrToken;
use leek_syntax::{SyntaxKind as S, SyntaxNode};

use crate::doc::{Doc, concat, group, indent, line, softline, space, text};

use super::{
    child_nodes, comma_sep, fmt_node, is_trivia, token_text, trailing_comma_doc, with_ctx,
};

/// `LiteralExpr` / `NameRef` — leaf expressions, emit their tokens
/// verbatim.
pub(super) fn format_atom(node: &SyntaxNode) -> Doc {
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

/// `lhs OP rhs` — binary expression. Adds spaces around the operator
/// and groups so long expressions can break before the operator.
pub(super) fn format_binary(node: &SyntaxNode) -> Doc {
    let mut lhs: Option<Doc> = None;
    let mut op: Option<Doc> = None;
    let mut rhs: Option<Doc> = None;

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => {
                // The op is the only non-trivia direct token child.
                if op.is_none() {
                    op = Some(token_text(&t));
                }
            }
            NodeOrToken::Node(child) => {
                if lhs.is_none() {
                    lhs = Some(fmt_node(&child));
                } else {
                    rhs = Some(fmt_node(&child));
                }
            }
        }
    }

    let lhs = lhs.unwrap_or_else(|| text(""));
    let op = op.unwrap_or_else(|| text("?"));
    let rhs = rhs.unwrap_or_else(|| text(""));

    group(concat([lhs, space(), op, line(), rhs]))
}

/// `OP operand` — prefix unary.
pub(super) fn format_unary(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => parts.push(token_text(&t)),
            NodeOrToken::Node(child) => parts.push(fmt_node(&child)),
        }
    }
    concat(parts)
}

/// `operand OP` — postfix unary (`++`, `--`, `!`).
pub(super) fn format_postfix(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => parts.push(token_text(&t)),
            NodeOrToken::Node(child) => parts.push(fmt_node(&child)),
        }
    }
    concat(parts)
}

/// Edge separator for the "space inside" padding options. A `line`
/// (space when flat, newline when broken) when padding is on, else a
/// `softline` (nothing when flat).
fn pad_edge(on: bool) -> Doc {
    if on { line() } else { softline() }
}

/// The `:` separator in map / object entries, padded per
/// `space_before_colon` / `space_after_colon`.
fn colon_doc() -> Doc {
    let (before, after) = with_ctx(|cx| (cx.opts.space_before_colon, cx.opts.space_after_colon));
    let mut s = String::new();
    if before {
        s.push(' ');
    }
    s.push(':');
    if after {
        s.push(' ');
    }
    text(s)
}

/// `( expr )`.
pub(super) fn format_paren(node: &SyntaxNode) -> Doc {
    let inner: Vec<Doc> = child_nodes(node).map(|n| fmt_node(&n)).collect();
    let inner_doc = concat(inner);
    let on = with_ctx(|cx| cx.opts.space_inside_parens);
    group(concat([
        text("("),
        indent(1, concat([pad_edge(on), inner_doc])),
        pad_edge(on),
        text(")"),
    ]))
}

/// `callee(args)`.
pub(super) fn format_call(node: &SyntaxNode) -> Doc {
    let mut callee: Option<Doc> = None;
    let mut arg_list: Option<Doc> = None;
    for child in child_nodes(node) {
        match child.kind() {
            S::ArgList => arg_list = Some(fmt_node(&child)),
            _ => {
                if callee.is_none() {
                    callee = Some(fmt_node(&child));
                }
            }
        }
    }
    let sep = if with_ctx(|cx| cx.opts.space_before_call_paren) {
        crate::doc::space()
    } else {
        crate::doc::nil()
    };
    concat([
        callee.unwrap_or_else(|| text("")),
        sep,
        arg_list.unwrap_or_else(|| text("()")),
    ])
}

/// `(arg, arg, ...)`. Trailing-comma behavior follows
/// [`FormatOptions::trailing_comma`].
pub(super) fn format_arg_list(node: &SyntaxNode) -> Doc {
    let args: Vec<Doc> = child_nodes(node).map(|n| fmt_node(&n)).collect();
    if args.is_empty() {
        return text("()");
    }

    let inner = crate::doc::join(&comma_sep(), args);
    let trailing = trailing_comma_doc(has_trailing_comma(node));
    let on = with_ctx(|cx| cx.opts.space_inside_parens);
    group(concat([
        text("("),
        indent(1, concat([pad_edge(on), inner, trailing])),
        pad_edge(on),
        text(")"),
    ]))
}

/// `[a, b, c]` — array literal.
pub(super) fn format_array(node: &SyntaxNode) -> Doc {
    bracketed_list(node, "[", "]")
}

/// `{a, b, c}` or `<a, b, c>` — set literal.
pub(super) fn format_set(node: &SyntaxNode) -> Doc {
    // Source may use either `<...>` or `{...}`; preserve the user's
    // choice by inspecting the first/last significant tokens.
    let (open, close) = pick_brackets(node, ('<', '>'), ('{', '}'));
    bracketed_list_with(node, open, close)
}

/// `[k: v, …]` or `[:]` — map literal.
pub(super) fn format_map(node: &SyntaxNode) -> Doc {
    format_kv_brackets(node, "[", "]", /* empty_is_colon = */ true)
}

/// `{f: v, …}` — object literal.
pub(super) fn format_object(node: &SyntaxNode) -> Doc {
    format_kv_brackets(node, "{", "}", /* empty_is_colon = */ false)
}

/// `base[index]`.
pub(super) fn format_index(node: &SyntaxNode) -> Doc {
    let mut base: Option<Doc> = None;
    let mut index: Option<Doc> = None;
    for child in child_nodes(node) {
        if base.is_none() {
            base = Some(fmt_node(&child));
        } else {
            index = Some(fmt_node(&child));
        }
    }
    concat([
        base.unwrap_or_else(|| text("")),
        text("["),
        index.unwrap_or_else(|| text("")),
        text("]"),
    ])
}

/// `base[i:j]` / `base[i:j:k]`.
pub(super) fn format_slice(node: &SyntaxNode) -> Doc {
    // Pass through — preserves user spacing between slice parts;
    // these are uncommon enough that a dedicated layout isn't worth
    // the complexity in v0.1.
    super::format_raw(node)
}

/// `base.field`.
pub(super) fn format_field(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => parts.push(token_text(&t)),
            NodeOrToken::Node(child) => parts.push(fmt_node(&child)),
        }
    }
    concat(parts)
}

/// `(params) => body` / `param -> body`.
pub(super) fn format_lambda(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_space = true;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
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
                // The parameter group's own parens are emitted by the
                // `ParamList` child below; the literal `(`/`)` delimiter tokens
                // that also hang directly off the `LambdaExpr` must be dropped,
                // else they double-wrap (`(a, b)` → `((a, b))`) — which is both
                // wrong and non-idempotent (the re-parse mangles it).
                S::LParen | S::RParen => {}
                _ => {
                    parts.push(token_text(&t));
                    last_was_space = false;
                }
            },
            NodeOrToken::Node(child) => {
                parts.push(fmt_node(&child));
                last_was_space = false;
            }
        }
    }
    concat(parts)
}

/// `new Type(args)`.
pub(super) fn format_new(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut last_was_space = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => {
                if t.kind() == S::KwNew {
                    parts.push(text("new"));
                    parts.push(space());
                    last_was_space = true;
                } else {
                    parts.push(token_text(&t));
                    last_was_space = false;
                }
            }
            NodeOrToken::Node(child) => {
                if !last_was_space && !parts.is_empty() {
                    // No space — `Type(args)` runs together.
                }
                parts.push(fmt_node(&child));
                last_was_space = false;
            }
        }
    }
    concat(parts)
}

/// `expr as Type`.
pub(super) fn format_cast(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    let mut emitted_first = false;
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::KwAs => {
                    parts.push(space());
                    parts.push(text("as"));
                    parts.push(space());
                }
                _ => parts.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                if emitted_first {
                    // The TypeRef following `as` is preceded by `as `
                    // already; nothing to add.
                }
                parts.push(fmt_node(&child));
                emitted_first = true;
            }
        }
    }
    concat(parts)
}

/// `cond ? then : else`.
pub(super) fn format_ternary(node: &SyntaxNode) -> Doc {
    let nodes: Vec<Doc> = child_nodes(node).map(|n| fmt_node(&n)).collect();
    let mut iter = nodes.into_iter();
    let cond = iter.next().unwrap_or_else(|| text(""));
    let then = iter.next().unwrap_or_else(|| text(""));
    let other = iter.next().unwrap_or_else(|| text(""));
    group(concat([
        cond,
        indent(
            1,
            concat([line(), text("? "), then, line(), text(": "), other]),
        ),
    ]))
}

/// `start..end[:step]`.
pub(super) fn format_interval(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => parts.push(token_text(&t)),
            NodeOrToken::Node(child) => parts.push(fmt_node(&child)),
        }
    }
    concat(parts)
}

/// `TypeRef` — a type expression (e.g. `Array<integer>`, `integer?`).
///
/// The parser sometimes attaches leading whitespace *inside* the
/// `TypeRef` node (because `start_node` doesn't flush trivia). We
/// emit only the significant tokens, joined without spaces — type
/// expressions in Leekscript don't take internal whitespace.
pub(super) fn format_type_ref(node: &SyntaxNode) -> Doc {
    let mut parts: Vec<Doc> = Vec::new();
    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => parts.push(token_text(&t)),
            NodeOrToken::Node(child) => parts.push(fmt_node(&child)),
        }
    }
    concat(parts)
}

// ---- Helpers ----

/// Shared brackets-with-element-list formatter for arrays, sets,
/// maps, and objects.
fn bracketed_list(node: &SyntaxNode, open: &'static str, close: &'static str) -> Doc {
    bracketed_list_with(node, open, close)
}

fn bracketed_list_with(node: &SyntaxNode, open: &'static str, close: &'static str) -> Doc {
    let elements: Vec<Doc> = child_nodes(node).map(|n| fmt_node(&n)).collect();
    if elements.is_empty() {
        // `[]` / `{}` — but `[:]` for empty map. Detect by looking
        // for a `:` token in the children.
        let has_colon = node
            .children_with_tokens()
            .filter_map(leek_syntax::language::NodeOrToken::into_token)
            .any(|t| t.kind() == S::Colon);
        if has_colon && open == "[" {
            return text("[:]");
        }
        return text(format!("{open}{close}"));
    }
    let trailing = trailing_comma_doc(has_trailing_comma(node));
    let inner = crate::doc::join(&comma_sep(), elements);
    let on = with_ctx(|cx| cx.opts.space_inside_brackets);
    group(concat([
        text(open),
        indent(1, concat([pad_edge(on), inner, trailing])),
        pad_edge(on),
        text(close),
    ]))
}

/// Format a key-value bracketed list like `[k: v, k: v]` or
/// `{f: v, f: v}`. The colons between key and value are bare
/// tokens in the CST, not separator nodes — we walk the token
/// stream to keep them.
fn format_kv_brackets(
    node: &SyntaxNode,
    open: &'static str,
    close: &'static str,
    empty_is_colon: bool,
) -> Doc {
    // Collect entries by walking children_with_tokens and grouping
    // (expr, ":", expr) triples. Commas act as entry separators.
    let mut entries: Vec<Doc> = Vec::new();
    let mut current: Vec<Doc> = Vec::new();
    let mut saw_colon_in_current = false;
    let mut saw_any_colon = false;

    let flush = |current: &mut Vec<Doc>, entries: &mut Vec<Doc>| {
        if !current.is_empty() {
            entries.push(concat(std::mem::take(current)));
        }
    };

    for el in node.children_with_tokens() {
        match el {
            NodeOrToken::Token(t) if is_trivia(&t) => {}
            NodeOrToken::Token(t) => match t.kind() {
                S::LBracket | S::RBracket | S::LBrace | S::RBrace => {}
                S::Colon => {
                    current.push(colon_doc());
                    saw_colon_in_current = true;
                    saw_any_colon = true;
                }
                S::Comma => {
                    flush(&mut current, &mut entries);
                    saw_colon_in_current = false;
                }
                _ => current.push(token_text(&t)),
            },
            NodeOrToken::Node(child) => {
                let _ = saw_colon_in_current;
                current.push(fmt_node(&child));
            }
        }
    }
    flush(&mut current, &mut entries);

    if entries.is_empty() {
        if empty_is_colon && saw_any_colon {
            return text("[:]");
        }
        return text(format!("{open}{close}"));
    }

    let inner = crate::doc::join(&comma_sep(), entries);
    let trailing = trailing_comma_doc(has_trailing_comma(node));
    let on = with_ctx(|cx| cx.opts.space_inside_brackets);
    group(concat([
        text(open),
        indent(1, concat([pad_edge(on), inner, trailing])),
        pad_edge(on),
        text(close),
    ]))
}

/// Decide between two bracket pairs by inspecting the actual
/// opening token in `node`.
fn pick_brackets(
    node: &SyntaxNode,
    primary: (char, char),
    fallback: (char, char),
) -> (&'static str, &'static str) {
    let primary_open = match primary.0 {
        '<' => S::Lt,
        '{' => S::LBrace,
        '[' => S::LBracket,
        '(' => S::LParen,
        _ => return ("[", "]"),
    };
    let first = node
        .children_with_tokens()
        .filter_map(leek_syntax::language::NodeOrToken::into_token)
        .find(|t| !is_trivia(t));
    let kind = first.map(|t| t.kind());
    if kind == Some(primary_open) {
        match primary.0 {
            '<' => ("<", ">"),
            '{' => ("{", "}"),
            '[' => ("[", "]"),
            '(' => ("(", ")"),
            _ => ("[", "]"),
        }
    } else {
        match fallback.0 {
            '<' => ("<", ">"),
            '{' => ("{", "}"),
            '[' => ("[", "]"),
            '(' => ("(", ")"),
            _ => ("[", "]"),
        }
    }
}

/// True if the source had a trailing comma before the closing
/// bracket of `node`.
fn has_trailing_comma(node: &SyntaxNode) -> bool {
    let mut last_sig_before_close: Option<S> = None;
    for el in node.children_with_tokens() {
        if let Some(t) = el.as_token() {
            if is_trivia(t) {
                continue;
            }
            // Stop tracking once we hit the closing bracket.
            if matches!(t.kind(), S::RBracket | S::RBrace | S::Gt | S::RParen) {
                break;
            }
            last_sig_before_close = Some(t.kind());
        } else {
            last_sig_before_close = None;
        }
    }
    last_sig_before_close == Some(S::Comma)
}
