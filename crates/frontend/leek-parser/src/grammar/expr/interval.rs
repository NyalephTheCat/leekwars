//! Bracket-delimited interval literals: `[a..b]`, `]a..b[`,
//! `[a..]`, `[..b]`, `[a..b:step]`, etc.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

use super::expr;

/// True if the `[` at the cursor opens an interval literal — i.e. a
/// `..` appears at depth 1 before the matching close bracket.
pub(super) fn looks_like_interval_open(p: &Parser) -> bool {
    assert!(p.at(S::LBracket));
    let mut i = 1;
    let mut depth: i32 = 1;
    let cap = 256;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBrace => depth += 1,
            S::LBracket => depth += 1,
            S::RParen | S::RBrace => depth -= 1,
            S::RBracket if depth == 1 => return false,
            S::RBracket => depth -= 1,
            S::DotDot if depth == 1 => return true,
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// True if the `]` at the cursor opens an interval literal.
/// `]` can't open any other expression, so we accept liberally —
/// require a `..` at the same balance level somewhere ahead.
pub(super) fn looks_like_interval_open_rbracket(p: &Parser) -> bool {
    assert!(p.at(S::RBracket));
    let mut i = 1;
    let mut depth: i32 = 0;
    let cap = 256;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBrace => depth += 1,
            // At depth 0, a `[` or `]` confirms the interval close
            // bracket. At higher depths it's just a nested open.
            S::LBracket if depth == 0 => return true,
            S::LBracket => depth += 1,
            S::RBracket if depth == 0 => return true,
            S::RBracket => depth -= 1,
            S::RParen | S::RBrace => depth -= 1,
            S::DotDot if depth == 0 => return true,
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// True if the `[` at the cursor has a matching `]` at the same depth
/// somewhere ahead. Used to keep postfix subscript parsing from
/// accidentally swallowing an interval-closing `[`.
pub(super) fn has_matching_rbracket(p: &Parser) -> bool {
    assert!(p.at(S::LBracket));
    let mut i = 1;
    let mut depth: i32 = 1;
    let cap = 256;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBrace => depth += 1,
            S::LBracket => depth += 1,
            S::RParen | S::RBrace => depth -= 1,
            S::RBracket if depth == 1 => return true,
            S::RBracket => depth -= 1,
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// Bracketed interval literal. Open bracket is `[` (inclusive start)
/// or `]` (exclusive start); close is `]` (inclusive end) or `[`
/// (exclusive end). Start and end expressions are optional. Optional
/// `:step` follows the end.
pub(super) fn interval_expr(p: &mut Parser) {
    assert!(p.at(S::LBracket) || p.at(S::RBracket));
    p.start_node(S::IntervalExpr);
    p.bump(); // open bracket
    // Optional start expression.
    if !p.at(S::DotDot) {
        expr(p);
    }
    if !p.eat(S::DotDot) {
        p.error("expected `..` inside interval literal");
    }
    // Optional end expression — stop tokens are `:`, `]`, `[`, EOF.
    if !matches!(p.current(), S::Colon | S::RBracket | S::LBracket | S::Eof) {
        expr(p);
    }
    // Optional step.
    if p.eat(S::Colon) && !matches!(p.current(), S::RBracket | S::LBracket | S::Eof) {
        expr(p);
    }
    // Close bracket.
    if !p.eat(S::RBracket) && !p.eat(S::LBracket) {
        p.error("expected `]` or `[` to close interval");
    }
    p.finish_node();
}
