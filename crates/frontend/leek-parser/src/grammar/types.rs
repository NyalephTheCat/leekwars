//! Type-annotation parser plus lookahead helpers used by other
//! grammar productions to decide whether the cursor is staring at a
//! typed declaration vs. an expression.
//!
//! Spec: `doc/grammar.md` §6.
//!
//! For this slice we cover primitive type names, generic containers
//! (`Array<T>`, `Map<K, V>`, `Set<T>`), user class names, `?`
//! nullability, and `|` unions. `function(...)` function types
//! land alongside lambda typing.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

/// Names recognized as primitive or container type starters.
/// Defined here rather than reused from `leek-syntax::SyntaxKind`
/// because in v1/v2 these are plain idents, not keywords.
const PRIMITIVE_TYPE_NAMES: &[&str] = &[
    "integer", "real", "string", "boolean", "void", "any", "null", "Array", "Map", "Set",
    "Interval", "Object", "Function",
    // Game-side common types — kept here so typed declarations using
    // them resolve, even though the type system proper treats them as
    // class names.
    "Number", "Boolean",
];

/// Cap for token-scan loops in lookaheads. Types in practice are short;
/// this is purely a safety guard against pathological inputs.
const LOOKAHEAD_CAP: usize = 32;

/// True if the n-th upcoming non-trivia token could start a type.
fn is_type_starter(p: &Parser, n: usize) -> bool {
    let kind = p.nth(n);
    if matches!(kind, S::KwBoolean | S::KwVoid | S::KwNull) {
        return true;
    }
    if kind != S::Ident {
        return false;
    }
    let text = p.nth_text(n);
    PRIMITIVE_TYPE_NAMES.contains(&text)
        || text.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// Scan past a type starting at offset `i`, returning the next offset
/// after the type. Returns `None` if no type was recognized.
///
/// Handles: primitive/container ident, generic args `<...>` (including
/// nested generics whose closing `>`s the lexer fused into `>>` /
/// `>>>`), `?` suffix, and `|`-union chain.
fn skip_type(p: &Parser, mut i: usize) -> Option<usize> {
    if !is_type_starter(p, i) {
        return None;
    }
    i += 1;
    // Optional generic args. Treat each `>>` as two closers and `>>>`
    // as three, since nested generics like `Map<K, Array<V>>` produce
    // fused tokens at the lexer level.
    if p.nth(i) == S::Lt {
        i += 1;
        let mut depth: i32 = 1;
        let mut steps = 0;
        while depth > 0 && steps < LOOKAHEAD_CAP {
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
        if depth < 0 {
            // Fused-greater overshot — type ended earlier in the run.
            // For lookahead purposes that still counts as a valid
            // type ending at this position.
            return Some(i);
        }
        if depth != 0 {
            return None;
        }
    }
    // `?` and union chain.
    loop {
        if i >= LOOKAHEAD_CAP {
            return None;
        }
        if p.nth(i) == S::Question {
            i += 1;
            continue;
        }
        if p.nth(i) == S::Pipe {
            i += 1;
            i = skip_type(p, i)?;
            continue;
        }
        break;
    }
    Some(i)
}

/// True if the cursor is at a `type IDENT` pattern. This is the
/// generic predicate used by callers in statement, parameter, and
/// class-member contexts.
pub(crate) fn looks_like_type_then_name(p: &Parser) -> bool {
    let Some(after) = skip_type(p, 0) else {
        return false;
    };
    p.nth(after) == S::Ident
}

/// True if the cursor is at the start of a typed variable declaration
/// statement: `type IDENT <followed by a stmt-end or stmt-start>`.
///
/// Semicolons are optional in Leekscript, so a following Ident or
/// keyword is also a valid follower (it would start the next stmt).
/// Expression-continuation tokens (operators, postfix `(`/`[`/`.`,
/// etc.) disqualify the match — those mean the whole `type IDENT`
/// run was actually a function call or similar expression.
pub(crate) fn looks_like_typed_var_decl(p: &Parser) -> bool {
    let Some(after) = skip_type(p, 0) else {
        return false;
    };
    if p.nth(after) != S::Ident {
        return false;
    }
    let next = p.nth(after + 1);
    // Clearly-decl follower tokens.
    if matches!(next, S::Eq | S::Semicolon | S::Comma | S::Eof | S::RBrace) {
        return true;
    }
    // Expression-continuation tokens disqualify: if `T x` were a value
    // expression they'd be the natural follow-up.
    if matches!(
        next,
        S::LParen
            | S::LBracket
            | S::Dot
            | S::PlusPlus
            | S::MinusMinus
            | S::Bang
            | S::Question
            | S::QuestionQuestion
            | S::Plus
            | S::Minus
            | S::Star
            | S::Slash
            | S::Percent
            | S::Backslash
            | S::StarStar
            | S::Lt
            | S::Le
            | S::Gt
            | S::Ge
            | S::EqEq
            | S::NotEq
            | S::EqEqEq
            | S::NotEqEq
            | S::AmpAmp
            | S::PipePipe
            | S::Amp
            | S::Pipe
            | S::Caret
            | S::Arrow
            | S::FatArrow
            | S::Colon
            | S::DotDot
            | S::KwAs
            | S::KwInstanceof
            | S::KwIn
    ) {
        return false;
    }
    // Anything else (Ident, KwVar, KwReturn, KwIf, …) is treated as a
    // statement-start that follows the omitted semicolon.
    true
}

/// Parse a complete type, consuming any `?`-nullability and `|`-union
/// chain. Wraps everything in a `TypeRef` node.
pub(crate) fn ty(p: &mut Parser) {
    let _ = parse_type(p, true);
}

/// Parse a type without consuming a trailing `?` nullability. Used in
/// contexts where a following `?` is ambiguous between nullability and
/// a ternary operator — `instanceof` and `as` are the two such sites.
/// In those positions, `T?` is always the ternary.
pub(crate) fn ty_no_nullable(p: &mut Parser) {
    let _ = parse_type(p, false);
}

/// Returns the number of "extra" closing `>`s consumed beyond the
/// natural close of this type. Non-zero only when nested generics
/// share a fused `>>` / `>>>` token; the surplus closes the outer
/// `<...>` (or its caller).
fn parse_type(p: &mut Parser, nullable: bool) -> u8 {
    // Depth-guard the type grammar (`Array<Array<…>>`, `T => R`) against
    // stack overflow on pathologically nested input.
    if p.enter_recursion() {
        p.error("type nests too deeply to parse");
        p.leave_recursion();
        return 0;
    }
    let extras = parse_type_inner(p, nullable);
    p.leave_recursion();
    extras
}

fn parse_type_inner(p: &mut Parser, nullable: bool) -> u8 {
    p.start_node(S::TypeRef);
    let mut extras = type_atom(p);
    if extras > 0 {
        p.finish_node();
        return extras;
    }
    if nullable {
        while p.at(S::Question) {
            p.bump();
        }
    }
    while p.at(S::Pipe) {
        p.bump();
        extras = type_atom(p);
        if extras > 0 {
            p.finish_node();
            return extras;
        }
        if nullable {
            while p.at(S::Question) {
                p.bump();
            }
        }
    }
    // Optional function-arrow tail: `T => R`.
    if p.at(S::FatArrow) {
        p.bump();
        let arrow_extras = parse_type(p, nullable);
        if arrow_extras > 0 {
            p.finish_node();
            return arrow_extras;
        }
    }
    p.finish_node();
    0
}

fn type_atom(p: &mut Parser) -> u8 {
    match p.current() {
        S::Ident | S::KwBoolean | S::KwVoid | S::KwNull => {
            p.bump();
            if p.at(S::Lt) {
                return generic_args(p);
            }
            0
        }
        // Empty-args function-arrow: `=> T`.
        S::FatArrow => {
            p.bump();
            parse_type(p, true)
        }
        _ => {
            p.error(format!("expected type, found {:?}", p.current()));
            0
        }
    }
}

/// Returns extras (count of `>`s beyond this `<...>`'s natural close).
fn generic_args(p: &mut Parser) -> u8 {
    assert!(p.at(S::Lt));
    p.bump(); // '<'
    if !p.at(S::Gt) && !p.at(S::ShiftRight) && !p.at(S::UShiftRight) {
        loop {
            let extras = parse_type(p, true);
            if extras > 0 {
                // Inner type's fused `>>` already closed our `<...>`.
                // Pass the remainder up.
                return extras - 1;
            }
            if !p.eat(S::Comma) {
                break;
            }
        }
    }
    // Expect a closer. A fused `>>` or `>>>` closes this AND outer
    // generics by 1/2 extras respectively.
    match p.current() {
        S::Gt => {
            p.bump();
            0
        }
        S::ShiftRight => {
            p.bump_remap(S::Gt);
            1
        }
        S::UShiftRight => {
            p.bump_remap(S::Gt);
            2
        }
        _ => {
            p.error(format!("expected `>`, found {:?}", p.current()));
            0
        }
    }
}
