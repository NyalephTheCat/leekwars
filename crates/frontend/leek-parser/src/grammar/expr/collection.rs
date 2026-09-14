//! Array, map, set, and object literals — `[...]`, `{...}`,
//! `<...>` forms. Each shape is disambiguated from sibling shapes
//! by scanning ahead for the first top-level separator.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

use super::{close_list, comma_optional_list, expr};

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
    let mut ternaries = TernaryDepth::default();
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBracket | S::LBrace => depth += 1,
            S::RParen | S::RBracket | S::RBrace if depth == 0 => return false,
            S::RParen | S::RBracket | S::RBrace => depth -= 1,
            S::Comma if depth == 0 => return false,
            S::Colon if depth == 0 && !ternaries.close() => return true,
            S::Question if depth == 0 => ternaries.open(p.nth(i + 1)),
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// How many ternary `?`s are still waiting for their `:` at the depth
/// being scanned.
///
/// Without this a lone `[cond ? a : b]` reads as a map literal keyed on
/// `cond ? a`, because its `:` is the first one at bracket depth 0.
/// Upstream never faces the question: `readArrayOrMapOrInterval` reads
/// the whole first expression — ternary and all — and only *then* looks
/// at the token in front of it. Pairing each `:` with a pending `?`
/// reproduces that answer without parsing twice, and still calls
/// `[a ? b : c : v]` a map, since its second `:` has no `?` left to
/// pair with.
#[derive(Default)]
struct TernaryDepth(u32);

impl TernaryDepth {
    /// Record a `?` that opens a ternary. `next` is the token after it:
    /// `?.` is optional member access, not a ternary. (`??` and `??=`
    /// are their own token kinds and never reach here.)
    fn open(&mut self, next: S) {
        if next != S::Dot {
            self.0 += 1;
        }
    }

    /// Consume a `:` — `true` if it closed a pending ternary rather
    /// than separating a key from a value.
    fn close(&mut self) -> bool {
        if self.0 == 0 {
            return false;
        }
        self.0 -= 1;
        true
    }
}

/// `[k: v, k: v, …]` map literal — the canonical map syntax. The comma
/// between entries is optional, as in upstream's `readMap`.
fn bracket_map_literal(p: &mut Parser) {
    assert!(p.at(S::LBracket));
    p.start_node(S::MapExpr);
    p.bump(); // '['
    let stuck = map_entries_until(p, S::RBracket);
    close_list(p, S::RBracket, stuck);
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
    // `{a: 1 b: 2}` parses the same as `{a: 1, b: 2}`, matching the
    // `if (get().getType() == VIRG) skip()` of upstream's object loop
    // (`WordCompiler.java:1992`).
    let stuck = map_entries_until(p, S::RBrace);
    close_list(p, S::RBrace, stuck);
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
    let mut ternaries = TernaryDepth::default();
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen | S::LBracket | S::LBrace => depth += 1,
            S::RParen | S::RBracket => depth -= 1,
            S::RBrace if depth == 0 => return false,
            S::RBrace => depth -= 1,
            S::Comma if depth == 0 => return false,
            S::Colon if depth == 0 && !ternaries.close() => return true,
            S::Question if depth == 0 => ternaries.open(p.nth(i + 1)),
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

/// Parse `key: value key: value …` until `closer` (not consumed),
/// comma between entries optional. Reports whether the list ended stuck
/// — see [`comma_optional_list`].
fn map_entries_until(p: &mut Parser, closer: S) -> bool {
    comma_optional_list(p, closer, |p| {
        expr(p); // key
        // An entry with no `:` is not a map entry, and guessing at a
        // value would report the rest of the literal as damage too.
        // Upstream throws `SIMPLE_ARRAY` here; we stop the list with
        // the one message `expect` just emitted.
        if !p.expect(S::Colon) {
            return false;
        }
        expr(p); // value
        true
    })
}

/// Array literal: `[]`, `[e1, e2, …]` or — commas being optional
/// upstream — `[e1 e2 …]`. Trailing comma allowed.
fn array_literal(p: &mut Parser) {
    assert!(p.at(S::LBracket));
    p.start_node(S::ArrayExpr);
    p.bump(); // '['
    let stuck = comma_optional_list(p, S::RBracket, |p| {
        expr(p);
        true
    });
    close_list(p, S::RBracket, stuck);
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
        // The angle spelling of a map literal is the bracket one with
        // a different closer, optional comma included.
        let _stuck = map_entries_until(p, S::Gt);
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
    let mut ternaries = TernaryDepth::default();
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
            S::Colon if depth == 0 && !ternaries.close() => return true,
            S::Question if depth == 0 => ternaries.open(p.nth(i + 1)),
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}
