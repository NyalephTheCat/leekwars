//! Array, map, set, and object literals — `[...]`, `{...}`,
//! `<...>` forms. Each shape is disambiguated from sibling shapes
//! by scanning ahead for the first top-level separator.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

use super::{can_start_expr, expr};

/// Either an array literal `[a, b, ...]` or a map literal
/// `[k: v, k: v, ...]`. Decided by looking at the first element's
/// follower: `:` → map, anything else → array.
pub(super) fn bracket_collection(p: &mut Parser) {
    assert!(p.at(S::LBracket));
    // `[:]` → empty map literal.
    if p.nth(1) == S::Colon && p.nth(2) == S::RBracket {
        p.start_node(S::MapExpr);
        p.bump(); // '['
        p.bump(); // ':'
        p.bump(); // ']'
        p.finish_node();
        return;
    }
    // Empty `[]` → empty array.
    if p.nth(1) == S::RBracket {
        p.start_node(S::ArrayExpr);
        p.bump(); // '['
        p.bump(); // ']'
        p.finish_node();
        return;
    }
    let is_map = bracket_first_sep_is_colon(p);
    if is_map {
        bracket_map_literal(p);
    } else {
        array_literal(p);
    }
}

/// True if the first comma/closing-bracket follower of `[`'s first
/// element is a `:`. Tracks paren/bracket/brace depth.
fn bracket_first_sep_is_colon(p: &Parser) -> bool {
    let mut i = 1usize;
    let mut depth = 0i32;
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBracket | S::LBrace => depth += 1,
            S::RParen | S::RBracket | S::RBrace if depth == 0 => return false,
            S::RParen | S::RBracket | S::RBrace => depth -= 1,
            S::Comma if depth == 0 => return false,
            S::Colon if depth == 0 => return true,
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// `[k: v, k: v, …]` map literal — the canonical map syntax.
fn bracket_map_literal(p: &mut Parser) {
    assert!(p.at(S::LBracket));
    p.start_node(S::MapExpr);
    p.bump(); // '['
    map_entries_until(p, S::RBracket);
    p.expect(S::RBracket);
    p.finish_node();
}

/// `{ … }` brace collection: object `{f: v, …}`, set `{a, b, c}`, or
/// empty `{}`. The first top-level separator decides: `:` → object,
/// `,` or close → set, neither → empty object by convention.
///
/// Maps use the `[k: v]` syntax, NOT braces — `{k: v}` is an object
/// (identifier-keyed record).
pub(super) fn brace_collection(p: &mut Parser) {
    assert!(p.at(S::LBrace));
    if p.nth(1) == S::RBrace {
        // Empty `{}` — convention: empty object.
        p.start_node(S::ObjectExpr);
        p.bump();
        p.bump();
        p.finish_node();
        return;
    }
    if brace_first_sep_is_colon(p) {
        object_literal(p);
    } else {
        set_literal(p);
    }
}

fn object_literal(p: &mut Parser) {
    p.start_node(S::ObjectExpr);
    p.bump(); // '{'
    // Object fields: `name: expr [, name: expr]*`. We accept any
    // expression as the key here and let the type-checker reject
    // non-identifier shapes later. `,` between fields is optional —
    // `{a: 1 b: 2}` parses the same as `{a: 1, b: 2}` (matches
    // upstream object-literal tolerance).
    while !p.at(S::RBrace) && !p.at_eof() {
        let before = p.position();
        expr(p);
        p.expect(S::Colon);
        expr(p);
        if p.at(S::Comma) {
            p.bump();
        } else if !p.at(S::RBrace) && can_start_expr(p.current()) {
            // No comma but the next field can start — keep going.
        } else {
            break;
        }
        if p.position() == before {
            p.err_and_bump("stuck in object literal");
            break;
        }
    }
    p.expect(S::RBrace);
    p.finish_node();
}

fn set_literal(p: &mut Parser) {
    // Set literal: `{a, b, c}` (alternate to `<a, b, c>`).
    p.start_node(S::SetExpr);
    p.bump(); // '{'
    if !p.at(S::RBrace) {
        set_element(p);
        while p.eat(S::Comma) {
            if p.at(S::RBrace) {
                break;
            }
            set_element(p);
        }
    }
    p.expect(S::RBrace);
    p.finish_node();
}

/// One set-literal element: a plain expression, or an inclusive
/// integer range `a..b` (`<1..3>` → `<1, 2, 3>`, descending allowed —
/// upstream #2335). The range form wraps both bounds in a
/// [`S::SetRangeElement`] node.
fn set_element(p: &mut Parser) {
    let cp = p.checkpoint();
    expr(p);
    if p.at(S::DotDot) {
        p.start_node_at(cp, S::SetRangeElement);
        p.bump(); // '..'
        expr(p);
        p.finish_node();
    }
}

fn brace_first_sep_is_colon(p: &Parser) -> bool {
    let mut i = 1usize;
    let mut depth = 0i32;
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBracket | S::LBrace => depth += 1,
            S::RParen | S::RBracket => depth -= 1,
            S::RBrace if depth == 0 => return false,
            S::RBrace => depth -= 1,
            S::Comma if depth == 0 => return false,
            S::Colon if depth == 0 => return true,
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// Parse `key: value, key: value, ...` until `closer` (not consumed).
fn map_entries_until(p: &mut Parser, closer: S) {
    while !p.at(closer) && !p.at_eof() {
        let before = p.position();
        expr(p); // key
        p.expect(S::Colon);
        expr(p); // value
        if !p.eat(S::Comma) {
            break;
        }
        if p.position() == before {
            p.err_and_bump("stuck in map literal");
            break;
        }
    }
}

/// Array literal: `[]` or `[e1, e2, …]`. Trailing comma allowed.
fn array_literal(p: &mut Parser) {
    assert!(p.at(S::LBracket));
    p.start_node(S::ArrayExpr);
    p.bump(); // '['
    while !p.at(S::RBracket) && !p.at_eof() {
        let before = p.position();
        expr(p);
        if p.at(S::Comma) {
            p.bump();
        } else if can_start_expr(p.current()) {
            // v1 quirk: `[1 2 3]` — comma-free array literal.
            // Just continue with the next element.
        } else if !p.at(S::RBracket) {
            // No comma, can't continue. Recovery.
            if p.position() == before {
                p.err_and_bump(format!(
                    "unexpected token in array literal: {:?}",
                    p.current()
                ));
            }
            break;
        }
    }
    p.expect(S::RBracket);
    p.finish_node();
}

/// Legacy angle-bracket set/map literal: `<a, b, c>`, `<a: b, c: d>`,
/// `<>`, or `<:>`. Disambiguates between map and set on the first
/// separator like the brace form.
pub(super) fn angle_set_or_map(p: &mut Parser) {
    assert!(p.at(S::Lt));
    // Empty `<>` → empty set.
    if p.nth(1) == S::Gt {
        p.start_node(S::SetExpr);
        p.bump(); // '<'
        p.bump(); // '>'
        p.finish_node();
        return;
    }
    let is_map = angle_first_sep_is_colon(p);
    let saved_gt = p.gt_is_binary;
    p.gt_is_binary = false;
    if is_map {
        p.start_node(S::MapExpr);
        p.bump(); // '<'
        map_entries_until_gt(p);
        let _ = p.eat(S::Gt);
        p.finish_node();
    } else {
        p.start_node(S::SetExpr);
        p.bump(); // '<'
        if !p.at(S::Gt) {
            set_element(p);
            while p.eat(S::Comma) {
                if p.at(S::Gt) {
                    break;
                }
                set_element(p);
            }
        }
        let _ = p.eat(S::Gt);
        p.finish_node();
    }
    p.gt_is_binary = saved_gt;
}

/// True if the first top-level separator of an angle-bracketed
/// literal is `:` (i.e. it's a map).
fn angle_first_sep_is_colon(p: &Parser) -> bool {
    let mut i = 1usize;
    let mut depth = 0i32;
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBracket | S::LBrace | S::Lt => depth += 1,
            S::RParen | S::RBracket | S::RBrace => depth -= 1,
            S::Gt if depth == 0 => return false,
            S::Gt => depth -= 1,
            S::ShiftRight if depth == 0 => return false,
            S::ShiftRight => depth -= 2,
            S::UShiftRight if depth == 0 => return false,
            S::UShiftRight => depth -= 3,
            S::Comma if depth == 0 => return false,
            S::Colon if depth == 0 => return true,
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

fn map_entries_until_gt(p: &mut Parser) {
    while !p.at(S::Gt) && !p.at_eof() {
        let before = p.position();
        expr(p);
        p.expect(S::Colon);
        expr(p);
        if !p.eat(S::Comma) {
            break;
        }
        if p.position() == before {
            p.err_and_bump("stuck in angle map literal");
            break;
        }
    }
}
