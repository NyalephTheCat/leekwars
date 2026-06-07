//! Pure AST helpers and small utilities used across the resolver
//! modules. Nothing in here touches `Resolver` state — these are
//! free functions over `SyntaxNode` / AST types.

use leek_parser::ast::{AstNode, BinaryExpr, ClassField, ClassMethod, Expr, ForeachStmt};
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// Two arity ranges `[a..b]` and `[c..d]` overlap if `a <= d` and
/// `c <= b`. Inclusive endpoints.
pub(crate) fn ranges_overlap(a: u8, b: u8, c: u8, d: u8) -> bool {
    a <= d && c <= b
}

/// Strip surrounding `'…'` / `"…"` quotes from a string literal.
pub(crate) fn strip_string_quotes(s: &str) -> String {
    if s.len() >= 2
        && let Some(first) = s.chars().next()
        && let Some(last) = s.chars().next_back()
        && first == last
        && (first == '\'' || first == '"')
    {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

/// Collect loop-variable names declared via `var` in a foreach
/// `for (var x in ...)` / `for (var x : var y in ...)` form. Only
/// the `var`-prefixed bindings count as new declarations; bare
/// `for (x in arr)` reuses an outer `x`.
pub(crate) fn collect_foreach_var_names(fe: &ForeachStmt) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending_var = false;
    let mut seen_in = false;
    for el in fe.syntax().children_with_tokens() {
        let Some(t) = el.into_token() else { continue };
        if t.kind() == SyntaxKind::KwIn {
            seen_in = true;
            continue;
        }
        if seen_in {
            continue;
        }
        match t.kind() {
            SyntaxKind::KwVar => pending_var = true,
            SyntaxKind::Colon => pending_var = false,
            SyntaxKind::Ident if pending_var => {
                out.push(t.text().to_string());
                pending_var = false;
            }
            _ => {}
        }
    }
    out
}

/// Walk a subtree looking for a NameRef whose Ident text matches
/// `name`. Returns the first matching Ident token.
pub(crate) fn find_name_ref_in(expr: &Expr, name: &str) -> Option<SyntaxToken> {
    for desc in expr.syntax().descendants() {
        if desc.kind() != SyntaxKind::NameRef {
            continue;
        }
        if let Some(tok) = desc
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .find(|t| t.kind() == SyntaxKind::Ident)
            && tok.text() == name
        {
            return Some(tok);
        }
    }
    None
}

pub(crate) fn first_ident(node: &SyntaxNode) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .filter_map(rowan::NodeOrToken::into_token)
        .find(|t| t.kind() == SyntaxKind::Ident)
}

pub(crate) fn first_ident_after(node: &SyntaxNode, after: SyntaxKind) -> Option<SyntaxToken> {
    let mut seen = false;
    for el in node.children_with_tokens() {
        let Some(t) = el.into_token() else { continue };
        if !seen {
            if t.kind() == after {
                seen = true;
            }
            continue;
        }
        if t.kind() == SyntaxKind::Ident {
            return Some(t);
        }
    }
    None
}

/// All `Ident` tokens that appear inside a var/global declaration.
/// These are the declarator names.
pub(crate) fn idents_after_keyword(node: &SyntaxNode) -> Vec<SyntaxToken> {
    let mut out = Vec::new();
    for el in node.children_with_tokens() {
        if let Some(t) = el.into_token()
            && t.kind() == SyntaxKind::Ident
        {
            out.push(t);
        }
    }
    out
}

pub(crate) fn field_name(f: &ClassField) -> Option<SyntaxToken> {
    // The field name is the first Ident token not inside a TypeRef
    // child. For simplicity grab the last top-level Ident token
    // before `=` or `;`.
    let mut last_ident: Option<SyntaxToken> = None;
    for el in f.syntax().children_with_tokens() {
        if let rowan::NodeOrToken::Token(t) = el {
            if t.kind() == SyntaxKind::Ident {
                last_ident = Some(t);
            } else if matches!(t.kind(), SyntaxKind::Eq | SyntaxKind::Semicolon) {
                break;
            }
        }
    }
    last_ident
}

pub(crate) fn method_name(m: &ClassMethod) -> Option<SyntaxToken> {
    // Same idea — last Ident token before `(`.
    let mut last_ident: Option<SyntaxToken> = None;
    for el in m.syntax().children_with_tokens() {
        if let rowan::NodeOrToken::Token(t) = el {
            if t.kind() == SyntaxKind::Ident {
                last_ident = Some(t);
            } else if t.kind() == SyntaxKind::LParen {
                break;
            }
        }
    }
    last_ident
}

/// Compute `(min_args, max_args)` for a `FnDecl` from its parameter
/// list. Parameters with default values count toward `max` but not
/// `min`. Returns `(0, 0)` if there's no param list.
pub(crate) fn fn_arity(node: &SyntaxNode) -> (u8, u8) {
    let Some(params) = node.children().find(|n| n.kind() == SyntaxKind::ParamList) else {
        return (0, 0);
    };
    let mut min = 0u8;
    let mut max = 0u8;
    let mut seen_default = false;
    for p in params.children() {
        if p.kind() != SyntaxKind::Param {
            continue;
        }
        let has_default = p
            .children_with_tokens()
            .filter_map(rowan::NodeOrToken::into_token)
            .any(|t| t.kind() == SyntaxKind::Eq);
        max = max.saturating_add(1);
        if has_default {
            seen_default = true;
        } else if !seen_default {
            min = min.saturating_add(1);
        }
    }
    (min, max)
}

/// Compute `(min_args, max_args)` for a lambda by walking its
/// `ParamList`. Lambdas don't support default args today, so min == max.
pub(crate) fn lambda_arity(node: &SyntaxNode) -> (u8, u8) {
    fn_arity(node)
}

/// True if `e` could be a valid l-value target. Conservatively
/// includes anything that looks index- / field- / name-shaped; the
/// runtime / type-checker decides definitively. Used to flag
/// obvious non-l-values like literals and arithmetic results.
pub(crate) fn is_potential_lvalue(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Name(_) | Expr::Field(_) | Expr::Index(_) | Expr::Slice(_) | Expr::Paren(_)
    )
}

/// True if the operator of a [`BinaryExpr`] is an assignment of any
/// kind (`=`, `+=`, `-=`, `*=`, `??=`, …).
pub(crate) fn is_assignment_binary(b: &BinaryExpr) -> bool {
    let Some(op) = b.op() else { return false };
    matches!(
        op.kind(),
        SyntaxKind::Eq
            | SyntaxKind::PlusEq
            | SyntaxKind::MinusEq
            | SyntaxKind::StarEq
            | SyntaxKind::SlashEq
            | SyntaxKind::BackslashEq
            | SyntaxKind::PercentEq
            | SyntaxKind::StarStarEq
            | SyntaxKind::AmpEq
            | SyntaxKind::PipeEq
            | SyntaxKind::CaretEq
            | SyntaxKind::ShiftLeftEq
            | SyntaxKind::ShiftRightEq
            | SyntaxKind::UShiftRightEq
            | SyntaxKind::QuestionQuestionEq
    )
}

/// Built-in class metadata fields that are always final on every
/// user-declared class. Assigning to them via `ClassName.foo = …`
/// errors regardless of whether `foo` was declared in the body.
pub(crate) const INTRINSIC_FINAL_CLASS_FIELDS: &[&str] = &[
    "name",
    "fields",
    "static_fields",
    "staticFields",
    "methods",
    "static_methods",
    "staticMethods",
    "parent",
    "superclass",
    "constructors",
    // Reflection: `A.class` yields the `Class` for `A`, `x.class`
    // yields the class of the value. Treated as an intrinsic so
    // `A.class` doesn't trip the "no static member" check.
    "class",
    // `A.class.super` walks the inheritance chain to a parent
    // `Class` value (or null).
    "super",
];

/// Builtins that exist at one or more versions but were removed in a
/// later version. Calling them at the removal-version-or-later
/// emits `REMOVED_FUNCTION`. Format: `(name, removed_at_version)`.
pub(crate) const REMOVED_BUILTINS: &[(&str, u8)] = &[
    ("assocSort", 4),
    ("keySort", 4),
    ("assocReverse", 4),
    ("color", 4),
    ("removeKey", 4),
];
