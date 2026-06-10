//! Top-level declarations: functions and classes.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

use super::{expr, stmt, types};

// ---- Functions ----

/// `function name(params) [-> type] { body }`. The version taking a
/// checkpoint wraps any preceding annotations.
/// Experimental: consume a generic angle-bracket group — a parameter list
/// `<T, U>` after a declaration name, or generic arguments `<Array<T>>` on
/// an `extends` base — into a `TypeParamList` node. Brace/paren/`;` stop the
/// scan so a stray `<` can't run away; `>>` counts as two closers.
fn angle_group(p: &mut Parser) {
    if !p.at(S::Lt) {
        return;
    }
    p.start_node(S::TypeParamList);
    let mut depth: i32 = 0;
    loop {
        match p.current() {
            S::Lt => {
                depth += 1;
                p.bump();
            }
            S::Gt => {
                depth -= 1;
                p.bump();
                if depth <= 0 {
                    break;
                }
            }
            S::ShiftRight => {
                depth -= 2;
                p.bump();
                if depth <= 0 {
                    break;
                }
            }
            S::Eof | S::LBrace | S::Semicolon => break,
            _ => p.bump(),
        }
    }
    p.finish_node();
}

/// Parse a generic parameter list after a declaration name, if the
/// experimental `generics` feature is on and a `<` follows.
fn maybe_type_params(p: &mut Parser) {
    if p.features().generics && p.at(S::Lt) {
        angle_group(p);
    }
}

pub(super) fn fn_decl_at(p: &mut Parser, cp: rowan::Checkpoint) {
    assert!(p.at(S::KwFunction));
    p.start_node_at(cp, S::FnDecl);
    p.bump(); // 'function'
    let _ = p.expect(S::Ident);
    maybe_type_params(p); // `function f<T>(…)`
    param_list(p);
    if p.eat(S::Arrow) || p.eat(S::FatArrow) {
        types::ty(p);
    }
    if p.at(S::LBrace) {
        stmt::block(p);
    } else if p.features().function_signatures {
        // Experimental: a bodiless signature — `function f() -> T;`.
        // The trailing `;` is optional.
        let _ = p.eat(S::Semicolon);
    } else {
        p.error("expected `{` to open function body");
    }
    p.finish_node();
}

/// `(param, param, ...)` — used by `function`, methods, and anonymous
/// functions. Parameters may be untyped or typed; trailing default
/// values allowed.
pub(super) fn param_list(p: &mut Parser) {
    if !p.expect(S::LParen) {
        return;
    }
    p.start_node(S::ParamList);
    if !p.at(S::RParen) {
        param(p);
        while p.eat(S::Comma) {
            param(p);
        }
    }
    p.finish_node();
    let _ = p.expect(S::RParen);
}

/// `[@] [type] IDENT [= expr]`
fn param(p: &mut Parser) {
    p.start_node(S::Param);
    let _ = p.eat(S::At); // legacy by-reference, deprecated in v≥2
    if types::looks_like_type_then_name(p) {
        types::ty(p);
    }
    let _ = p.expect(S::Ident);
    if p.eat(S::Eq) {
        expr::expr(p);
    }
    p.finish_node();
}

/// Parameter parser exposed for `(params -> body)` lambdas, where the
/// caller drives the loop because the terminator is `->` not `)`.
pub(super) fn inner_param(p: &mut Parser) {
    param(p);
}

// ---- Classes ----

/// `class Name [extends Parent] { class_member* }`. The version taking
/// a checkpoint wraps any preceding annotations.
pub(super) fn class_decl_at(p: &mut Parser, cp: rowan::Checkpoint) {
    assert!(p.at(S::KwClass));
    p.start_node_at(cp, S::ClassDecl);
    p.bump(); // 'class'
    let _ = p.expect(S::Ident);
    maybe_type_params(p); // `class Box<T> { … }`
    if p.eat(S::KwExtends) {
        let _ = p.expect(S::Ident);
        maybe_type_params(p); // `extends Base<T>`
    }
    // Experimental `implements I1, I2` clause. `implements` is a
    // *reserved* keyword (v3+) lexed as `KwImplements`; the flag
    // unlocks the production. Wrapped in its own node so the class's
    // name/parent token scans stay unaffected.
    if p.features().interfaces && p.at(S::KwImplements) {
        p.start_node(S::ImplementsClause);
        p.bump(); // `implements` keyword
        loop {
            let _ = p.expect(S::Ident);
            if !p.eat(S::Comma) {
                break;
            }
        }
        p.finish_node();
    }
    class_body(p);
    p.finish_node();
}

fn class_body(p: &mut Parser) {
    if !p.expect(S::LBrace) {
        return;
    }
    p.start_node(S::ClassBody);
    while !p.at(S::RBrace) && !p.at_eof() {
        let before = p.position();
        class_member(p);
        if !p.at(S::RBrace) && !p.at_eof() && p.position() == before {
            let kind = p.current();
            p.err_and_bump(format!("unexpected token in class body: {kind:?}"));
        }
    }
    let _ = p.expect(S::RBrace);
    p.finish_node();
}

fn class_member(p: &mut Parser) {
    let cp = p.checkpoint();
    super::consume_annotations(p);

    // Consume any modifiers (access + static + final). Order is flexible
    // in the reference grammar. Accept both keyword forms and the
    // ident text — at v1/v2 `final` lexes as an `Ident` rather than
    // `KwFinal`, but class bodies still treat it as a modifier.
    loop {
        let kind = p.current();
        let is_kw_modifier = matches!(
            kind,
            S::KwPublic | S::KwPrivate | S::KwProtected | S::KwStatic | S::KwFinal
        );
        let is_ident_final = kind == S::Ident && p.current_text() == "final";
        if is_kw_modifier || is_ident_final {
            p.bump();
        } else {
            break;
        }
    }

    // Constructor?
    if p.at(S::KwConstructor) {
        p.start_node_at(cp, S::ClassConstructor);
        p.bump(); // 'constructor'
        param_list(p);
        if p.at(S::LBrace) {
            stmt::block(p);
        }
        p.finish_node();
        return;
    }

    // Method or field. The grammar shape is identical up to the
    // distinguishing token:
    //   [type] IDENT '(' …    → method
    //   [type] IDENT '='|';'   → field
    // So we peek to decide.
    if types::looks_like_type_then_name(p) {
        types::ty(p);
    }
    if p.at(S::Ident) {
        let after_name = p.nth(1);
        // A method is `name(` — or, with generics on, `name<…>(`.
        let is_method = after_name == S::LParen || (p.features().generics && after_name == S::Lt);
        if is_method {
            // Method.
            p.start_node_at(cp, S::ClassMethod);
            p.bump(); // method name
            maybe_type_params(p); // `T m<U>(…)`
            param_list(p);
            if p.eat(S::Arrow) || p.eat(S::FatArrow) {
                types::ty(p);
            }
            if p.at(S::LBrace) {
                stmt::block(p);
            }
            p.finish_node();
            return;
        }
        // Field.
        p.start_node_at(cp, S::ClassField);
        p.bump(); // field name
        if p.eat(S::Eq) {
            expr::expr(p);
        }
        let _ = p.eat(S::Semicolon);
        p.finish_node();
    }

    // Couldn't classify — let the outer loop's force-bump kick in by
    // not consuming anything here. The caller treats no-progress as an
    // error.
}
