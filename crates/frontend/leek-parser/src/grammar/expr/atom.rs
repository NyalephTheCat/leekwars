//! Primary atom dispatch for the expression parser — literals,
//! names, parenthesised groups, collection literals, intervals,
//! prefix operators, `new C(...)`, and the four lambda shapes.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

use super::collection::{angle_set_or_map, brace_collection, bracket_collection};
use super::interval::{interval_expr, looks_like_interval_open, looks_like_interval_open_rbracket};
use super::lambda::{
    anonymous_fn_expr, anonymous_fn_expr_named, has_inner_arrow_lambda, is_lambda_head,
    lambda_paren, lambda_paren_inner_arrow, lambda_single_param, lambda_zero_param,
};
use super::{PREFIX_BP, arg_list, expr_bp};

/// Parse a single atom or unary-prefixed expression. Returns `false`
/// if no expression could be started (parser is left where it was,
/// minus an emitted diagnostic).
pub(super) fn atom_or_prefix(p: &mut Parser) -> bool {
    match p.current() {
        S::IntLiteral
        | S::RealLiteral
        | S::StringLiteral
        | S::KwTrue
        | S::KwFalse
        | S::KwNull
        | S::Lemniscate
        | S::Pi => {
            p.start_node(S::LiteralExpr);
            p.bump();
            p.finish_node();
            true
        }
        // `this`, `super`, `class` are name-like expression atoms.
        // (`class` inside a method body is an expression yielding the
        // current class — see `TestClass.testClass_name`.)
        S::Ident | S::KwThis | S::KwSuper | S::KwClass => {
            // Lambda with a single bare-parameter (`x -> x + 1` or
            // `x => x + 1`) lives under the same opener as an
            // identifier expression; we disambiguate via the next
            // token.
            if matches!(p.nth(1), S::Arrow | S::FatArrow) {
                lambda_single_param(p);
                return true;
            }
            // v1-v2 alias: `Function(params) { body }` /
            // `FUNCTION(...)` parse as an anonymous function
            // expression, same shape as the lowercase
            // `function` keyword. Detected at the atom level so
            // postfix parsing doesn't grab the trailing block as a
            // separate statement.
            if p.version() <= leek_syntax::Version::V2
                && p.current() == S::Ident
                && matches!(p.current_text(), "Function" | "FUNCTION")
                && p.nth(1) == S::LParen
            {
                anonymous_fn_expr_named(p);
                return true;
            }
            p.start_node(S::NameRef);
            p.bump();
            p.finish_node();
            true
        }
        // Anonymous function as an expression:
        //   function([params]) [-> type] { body }
        // Distinct from `(params) -> body` lambdas — both are
        // accepted.
        S::KwFunction => {
            anonymous_fn_expr(p);
            true
        }
        // Zero-param lambda: `-> expr` or `=> expr`.
        S::Arrow | S::FatArrow => {
            lambda_zero_param(p);
            true
        }
        S::LParen => {
            // Lambdas vs paren-expressions are disambiguated by
            // scanning ahead for a matching `)` immediately followed
            // by `->` or `=>`.
            if is_lambda_head(p) {
                lambda_paren(p);
            } else if has_inner_arrow_lambda(p) {
                lambda_paren_inner_arrow(p);
            } else {
                p.start_node(S::ParenExpr);
                p.bump();
                // Inside parens, `>` is a normal binary operator
                // again — the outer set's `gt_is_binary=false` only
                // applies at the top level of the set element.
                let saved = p.gt_is_binary;
                p.gt_is_binary = true;
                expr_bp(p, 0);
                p.gt_is_binary = saved;
                p.expect(S::RParen);
                p.finish_node();
            }
            true
        }
        S::LBracket => {
            // Could be one of:
            //   `[]`         — empty array
            //   `[:]`        — empty map literal
            //   `[a, b, c]`  — array literal
            //   `[k: v, …]`  — map literal
            //   `[a..b]`     — closed interval (v≥4)
            //   `[a..b[`     — half-open interval
            //   `[..b]`      — open-start interval
            //   `[a..]`      — open-end interval
            if looks_like_interval_open(p) {
                interval_expr(p);
            } else {
                bracket_collection(p);
            }
            true
        }
        S::RBracket => {
            // `]` at expression start can only be an open-start
            // interval: `]a..b]`, `]a..b[`, `]..[`, etc.
            if looks_like_interval_open_rbracket(p) {
                interval_expr(p);
                return true;
            }
            p.error(format!("expected expression, found {:?}", p.current()));
            false
        }
        S::LBrace => {
            brace_collection(p);
            true
        }
        // Legacy `<a, b, c>` and `<>` set literals (also `<:>` empty
        // map). Only matched at expression start since `<` is
        // otherwise less-than.
        S::Lt => {
            angle_set_or_map(p);
            true
        }
        S::KwNew => {
            new_expr(p);
            true
        }
        S::Minus
        | S::Plus
        | S::Bang
        | S::Tilde
        | S::KwNot
        | S::PlusPlus
        | S::MinusMinus
        | S::At => {
            p.start_node(S::UnaryExpr);
            p.bump();
            expr_bp(p, PREFIX_BP);
            p.finish_node();
            true
        }
        _ => {
            p.error(format!("expected expression, found {:?}", p.current()));
            false
        }
    }
}

/// `new Name(args)`.
fn new_expr(p: &mut Parser) {
    assert!(p.at(S::KwNew));
    p.start_node(S::NewExpr);
    p.bump(); // 'new'
    let _ = p.expect(S::Ident);
    if p.at(S::LParen) {
        arg_list(p);
    }
    p.finish_node();
}
