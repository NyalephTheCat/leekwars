//! Lambda parsing — the five shapes Leekscript accepts:
//! - Single-param: `x -> body`
//! - Zero-param: `-> body` or `=> body`
//! - Parenthesised params: `(a, b) -> body`
//! - Parenthesised inner-arrow: `(a, b -> body)`
//! - `function`-keyword anonymous form: `function (a, b) { … }`
//!
//! Plus the look-aheads that disambiguate `(` between a paren-
//! expression, lambda head, or inner-arrow lambda.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

use super::expr;

/// Scan past a balanced `(...)` and check what's next. If `->` or
/// `=>`, the parens are a lambda head; otherwise a grouped
/// expression. Spec: `LexicalParserTokenStream.isArrowFunctionAhead`.
pub(super) fn is_lambda_head(p: &Parser) -> bool {
    assert!(p.at(S::LParen));
    let mut depth = 1i32;
    let mut i = 1usize;
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen => depth += 1,
            S::RParen => {
                depth -= 1;
                if depth == 0 {
                    return matches!(p.nth(i + 1), S::Arrow | S::FatArrow);
                }
                // depth > 0: keep scanning (nested parens).
            }
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// `(x, y -> body)` form: parens enclose both the params *and* the
/// arrow + body. Detect by scanning at depth 1 for an `->`/`=>`
/// before the matching `)`. Nested parens are skipped.
pub(super) fn has_inner_arrow_lambda(p: &Parser) -> bool {
    assert!(p.at(S::LParen));
    let mut depth = 1i32;
    let mut i = 1usize;
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen => depth += 1,
            S::RParen => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            S::Arrow | S::FatArrow if depth == 1 => return true,
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// Heuristic: scan forward from the start of a `as`-cast's type and
/// decide if a `?` we'd encounter is more likely a ternary than a
/// nullable suffix. Returns true if a `:` is reachable at depth 0
/// before a statement-ending token — that means we're in a ternary.
pub(super) fn looks_like_ternary_after_type(p: &Parser) -> bool {
    let mut i = 0usize;
    let mut depth_paren = 0i32;
    let mut depth_bracket = 0i32;
    let mut depth_brace = 0i32;
    let mut depth_angle = 0i32;
    let mut saw_question = false;
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        let k = p.nth(i);
        match k {
            S::LParen => depth_paren += 1,
            S::RParen => {
                if depth_paren == 0 {
                    return false;
                }
                depth_paren -= 1;
            }
            S::LBracket => depth_bracket += 1,
            S::RBracket => {
                if depth_bracket == 0 {
                    return false;
                }
                depth_bracket -= 1;
            }
            S::LBrace => depth_brace += 1,
            S::RBrace => {
                if depth_brace == 0 {
                    return false;
                }
                depth_brace -= 1;
            }
            S::Lt => depth_angle += 1,
            S::Gt if depth_angle > 0 => depth_angle -= 1,
            S::ShiftRight if depth_angle >= 2 => depth_angle -= 2,
            S::UShiftRight if depth_angle >= 3 => depth_angle -= 3,
            S::Question
                if depth_paren == 0
                    && depth_bracket == 0
                    && depth_brace == 0
                    && depth_angle == 0 =>
            {
                saw_question = true;
            }
            S::Colon
                if saw_question
                    && depth_paren == 0
                    && depth_bracket == 0
                    && depth_brace == 0
                    && depth_angle == 0 =>
            {
                return true;
            }
            S::Semicolon | S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// Scan a type starting at lookahead offset `i` (handling generics,
/// `[]`, `?`, and `|` unions), returning the offset just past it — or
/// `None` if `i` isn't a type. Pure look-ahead (no tokens consumed).
fn scan_type_ahead(p: &Parser, mut i: usize) -> Option<usize> {
    let is_base = matches!(
        p.nth(i),
        S::Ident | S::KwBoolean | S::KwVoid | S::KwInt | S::KwFloat
    );
    if !is_base {
        return None;
    }
    i += 1;
    // Generic arguments: balanced `<...>` (with `>>`/`>>>` shift tokens).
    if p.nth(i) == S::Lt {
        let mut depth = 1i32;
        i += 1;
        let mut steps = 0;
        while depth > 0 && steps < 256 {
            match p.nth(i) {
                S::Lt => depth += 1,
                S::Gt => depth -= 1,
                S::ShiftRight => depth -= 2,
                S::UShiftRight => depth -= 3,
                S::Eof => return None,
                _ => {}
            }
            i += 1;
            steps += 1;
        }
    }
    // `[]` array and `?` nullable suffixes.
    loop {
        if p.nth(i) == S::LBracket && p.nth(i + 1) == S::RBracket {
            i += 2;
        } else if p.nth(i) == S::Question {
            i += 1;
        } else {
            break;
        }
    }
    // Union: `Type | Type`.
    if p.nth(i) == S::Pipe {
        return scan_type_ahead(p, i + 1);
    }
    Some(i)
}

/// True if the tokens after a lambda arrow form an explicit return type
/// *followed by a body* — `x => integer x + 1`, `() => real { … }`. The
/// body-start token (an ident/literal/`{`/unary prefix) distinguishes a
/// return type from a body that merely begins with a type-ish name
/// (`x => y + 1`, `x => Array(…)`). Mirrors the official arrow-function
/// grammar (`WordCompiler`: arrow → optional `eatType` → body).
fn lambda_has_return_type(p: &Parser) -> bool {
    let Some(end) = scan_type_ahead(p, 0) else {
        return false;
    };
    if end == 0 {
        return false;
    }
    // Only UNAMBIGUOUS body starts confirm a return type. Operator tokens
    // (`-`, `!`, `~`, `++`, `--`, `@`) are excluded: after a bare ident they
    // are postfix/binary continuations of that ident as an expression
    // (`f++`, `x - 1`), not a body following a return type.
    matches!(
        p.nth(end),
        S::Ident
            | S::KwThis
            | S::KwSuper
            | S::KwNew
            | S::KwFunction
            | S::IntLiteral
            | S::RealLiteral
            | S::StringLiteral
            | S::KwTrue
            | S::KwFalse
            | S::KwNull
            | S::LBrace
    )
}

/// Parse a lambda's body after the arrow: an optional return type, then a
/// block (`{ … }`) or a single expression.
fn arrow_lambda_body(p: &mut Parser) {
    if lambda_has_return_type(p) {
        crate::grammar::types::ty(p);
    }
    if p.at(S::LBrace) {
        crate::grammar::stmt::block(p);
    } else {
        expr(p);
    }
}

/// `(params) -> body | expr` (also accepts `=>`).
pub(super) fn lambda_paren(p: &mut Parser) {
    p.start_node(S::LambdaExpr);
    crate::grammar::decls::param_list(p);
    if !p.eat(S::Arrow) && !p.eat(S::FatArrow) {
        p.error("expected `->` or `=>` after lambda parameters");
    }
    arrow_lambda_body(p);
    p.finish_node();
}

/// `(params -> body)` form: the closing `)` comes after the body,
/// so the params and body are both inside one set of parens.
/// `( x, y -> x + y )` is equivalent to `(x, y) -> x + y`.
pub(super) fn lambda_paren_inner_arrow(p: &mut Parser) {
    p.start_node(S::LambdaExpr);
    if !p.expect(S::LParen) {
        p.finish_node();
        return;
    }
    p.start_node(S::ParamList);
    // Parse comma-separated params until `->`/`=>`. Mirrors
    // [`decls::param_list`] but stops at the arrow instead of `)`.
    if !matches!(p.current(), S::Arrow | S::FatArrow) {
        crate::grammar::decls::inner_param(p);
        while p.eat(S::Comma) {
            crate::grammar::decls::inner_param(p);
        }
    }
    p.finish_node(); // ParamList
    if !p.eat(S::Arrow) && !p.eat(S::FatArrow) {
        p.error("expected `->` or `=>` in parenthesised lambda");
    }
    arrow_lambda_body(p);
    let _ = p.expect(S::RParen);
    p.finish_node();
}

/// `-> body | expr` or `=> body | expr` — zero-param lambda.
pub(super) fn lambda_zero_param(p: &mut Parser) {
    assert!(matches!(p.current(), S::Arrow | S::FatArrow));
    p.start_node(S::LambdaExpr);
    p.start_node(S::ParamList);
    p.finish_node(); // empty ParamList
    p.bump(); // `->` or `=>`
    arrow_lambda_body(p);
    p.finish_node();
}

/// Detect `T x -> body` or `T x => body` (single typed bare
/// parameter lambda). The type may carry the modifiers `eatType`
/// recognizes — generics (`Array<integer> x =>`), nullable
/// (`integer? x =>`), and unions (`integer | string x =>`) —
/// mirroring the upstream lambda fast-path (6d55b79).
pub(super) fn looks_like_typed_bare_lambda(p: &Parser) -> bool {
    // First token must be a type-ish ident (or a type keyword)
    // and not followed by an arrow itself (`x -> body` is the
    // already-handled single-bare-param form).
    let first = p.current();
    let type_ish = matches!(first, S::Ident | S::KwBoolean | S::KwVoid | S::KwNull);
    if !type_ish {
        return false;
    }
    // Bare single-token type: `integer x =>`.
    if p.nth(1) == S::Ident && matches!(p.nth(2), S::Arrow | S::FatArrow) {
        return true;
    }
    // Modified type: scan the full type grammar, then require the
    // param name + arrow. (The arrow requirement keeps ternaries like
    // `integer ? a : b` and bitwise-ors like `a | b + 1` out.)
    let Some(end) = scan_type_ahead(p, 0) else {
        return false;
    };
    end > 1 && p.nth(end) == S::Ident && matches!(p.nth(end + 1), S::Arrow | S::FatArrow)
}

/// `T x -> body` — single typed bare-param lambda. The full type
/// (with generic/nullable/union modifiers) is consumed as a
/// `TypeRef` child of the `Param` so HIR can pick it up.
pub(super) fn lambda_typed_bare(p: &mut Parser) {
    p.start_node(S::LambdaExpr);
    p.start_node(S::ParamList);
    p.start_node(S::Param);
    crate::grammar::types::ty(p); // full type (TypeRef node)
    p.bump(); // ident (param name)
    p.finish_node(); // Param
    p.finish_node(); // ParamList
    if !p.eat(S::Arrow) && !p.eat(S::FatArrow) {
        p.error("expected `->` or `=>` after typed lambda parameter");
    }
    arrow_lambda_body(p);
    p.finish_node();
}

/// Look ahead from a leading `Ident` to detect a multi-param
/// bare lambda — `x, y -> body` or `a, b, c => body`. Returns the
/// number of param identifiers (≥2) on success.
pub(super) fn multi_bare_lambda_param_count(p: &Parser) -> Option<usize> {
    if !matches!(p.current(), S::Ident | S::KwThis | S::KwSuper | S::KwClass) {
        return None;
    }
    if p.nth(1) != S::Comma {
        return None;
    }
    let mut i = 0usize;
    let mut count = 0usize;
    let cap = 32;
    loop {
        if !matches!(p.nth(i), S::Ident | S::KwThis | S::KwSuper | S::KwClass) {
            return None;
        }
        count += 1;
        let next = p.nth(i + 1);
        match next {
            S::Arrow | S::FatArrow if count >= 2 => return Some(count),
            S::Comma => {
                i += 2;
                if i / 2 > cap {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

/// `x, y -> body` — multi-param bare lambda. Each identifier is a
/// fresh parameter; the body is either a block or a single
/// expression.
pub(super) fn lambda_multi_bare(p: &mut Parser, count: usize) {
    p.start_node(S::LambdaExpr);
    p.start_node(S::ParamList);
    for i in 0..count {
        p.start_node(S::Param);
        p.bump(); // ident
        p.finish_node(); // Param
        if i + 1 < count {
            let _ = p.eat(S::Comma);
        }
    }
    p.finish_node(); // ParamList
    if !p.eat(S::Arrow) && !p.eat(S::FatArrow) {
        p.error("expected `->` or `=>` after lambda parameters");
    }
    arrow_lambda_body(p);
    p.finish_node();
}

/// `ident -> body | expr` — single-param lambda. Both `->` and `=>`
/// arrows are accepted.
pub(super) fn lambda_single_param(p: &mut Parser) {
    p.start_node(S::LambdaExpr);
    p.start_node(S::ParamList);
    p.start_node(S::Param);
    p.bump(); // the bare identifier
    p.finish_node(); // Param
    p.finish_node(); // ParamList
    if !p.eat(S::Arrow) && !p.eat(S::FatArrow) {
        p.error("expected `->` or `=>` after lambda parameter");
    }
    arrow_lambda_body(p);
    p.finish_node();
}

/// v1-v2 alias: `Function(params) { body }` /
/// `FUNCTION(params) { body }` — the upstream parser also accepts
/// the leading identifier as a synonym for the `function` keyword
/// in expression position. We consume the `Function`/`FUNCTION`
/// Ident as if it were `KwFunction` and reuse the rest of the
/// anonymous-function shape.
pub(super) fn anonymous_fn_expr_named(p: &mut Parser) {
    p.start_node(S::LambdaExpr);
    p.bump(); // `Function` / `FUNCTION` ident
    crate::grammar::decls::param_list(p);
    if p.eat(S::Arrow) || p.eat(S::FatArrow) {
        crate::grammar::types::ty(p);
    }
    if p.at(S::LBrace) {
        crate::grammar::stmt::block(p);
    } else {
        p.error("expected `{` to open function body");
    }
    p.finish_node();
}

/// Anonymous function expression:
/// `function [name](params) [-> type] { body }`.
/// Modeled as a [`LambdaExpr`](S::LambdaExpr) node so callers
/// treating it as an expression (e.g. argument-list walks)
/// recognize it. The shape inside the node is the same as a
/// `FnDecl`, just expression-typed.
pub(super) fn anonymous_fn_expr(p: &mut Parser) {
    assert!(p.at(S::KwFunction));
    p.start_node(S::LambdaExpr);
    p.bump(); // 'function'
    // Optional name in expression position (legacy v1/v2 syntax).
    let _ = p.eat(S::Ident);
    crate::grammar::decls::param_list(p);
    if p.eat(S::Arrow) || p.eat(S::FatArrow) {
        crate::grammar::types::ty(p);
    }
    if p.at(S::LBrace) {
        crate::grammar::stmt::block(p);
    } else {
        p.error("expected `{` to open function body");
    }
    p.finish_node();
}
