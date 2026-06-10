//! Statement productions.

use leek_syntax::SyntaxKind as S;

use crate::parser::Parser;

use super::{expr, types};

pub(super) fn stmt(p: &mut Parser) {
    let cp = p.checkpoint();
    stmt_at(p, cp);
}

/// True if `kind` can extend the preceding expression — binary
/// operators, `.`, `[`, `(`, postfix `++`/`--`, `?` ternary,
/// `is`/`instanceof`/`as`/`in`, etc. Used to disambiguate `{}` at
/// statement start: followed by an extender it's an object literal,
/// otherwise it's an empty block.
fn can_continue_expr(kind: S) -> bool {
    matches!(
        kind,
        S::Plus
            | S::Minus
            | S::Star
            | S::Slash
            | S::Backslash
            | S::Percent
            | S::StarStar
            | S::Amp
            | S::Pipe
            | S::Caret
            | S::AmpAmp
            | S::PipePipe
            | S::ShiftLeft
            | S::ShiftRight
            | S::UShiftRight
            | S::EqEq
            | S::NotEq
            | S::EqEqEq
            | S::NotEqEq
            | S::Lt
            | S::Le
            | S::Gt
            | S::Ge
            | S::Eq
            | S::PlusEq
            | S::MinusEq
            | S::StarEq
            | S::SlashEq
            | S::BackslashEq
            | S::PercentEq
            | S::StarStarEq
            | S::QuestionQuestionEq
            | S::AmpEq
            | S::PipeEq
            | S::CaretEq
            | S::ShiftLeftEq
            | S::ShiftRightEq
            | S::UShiftRightEq
            | S::Question
            | S::QuestionQuestion
            | S::Dot
            | S::LBracket
            | S::LParen
            | S::PlusPlus
            | S::MinusMinus
            | S::KwAnd
            | S::KwOr
            | S::KwIs
            | S::KwInstanceof
            | S::KwAs
            | S::KwIn
    )
}

/// Statement entry-point that accepts a checkpoint set by the caller
/// (typically before any leading annotations). Used by
/// `top_level_item` so annotations end up wrapped inside the
/// declaration node for var-decls / typed-decls.
pub(super) fn stmt_at(p: &mut Parser, cp: rowan::Checkpoint) {
    match p.current() {
        S::KwVar => var_decl_stmt_at(p, cp),
        S::KwGlobal => global_decl_stmt_at(p, cp),
        S::KwReturn => return_stmt(p),
        S::KwIf => if_stmt(p),
        S::KwWhile => while_stmt(p),
        S::KwDo => do_while_stmt(p),
        S::KwFor => for_or_foreach_stmt(p),
        S::KwSwitch => switch_stmt(p),
        S::KwBreak => break_stmt(p),
        S::KwContinue => continue_stmt(p),
        S::KwInclude => include_stmt(p),
        S::KwImport => import_stmt(p),
        S::LBrace => {
            // `{}` followed by a token that continues an expression
            // (`{} % 5`, `{} + 1`) is an object literal at the start
            // of an expression statement, not an empty block. The
            // brace_collection path handles the literal — fall
            // through to expr_stmt.
            if p.nth(1) == S::RBrace && can_continue_expr(p.nth(2)) {
                expr_stmt(p);
            } else {
                block(p);
            }
        }
        // Bare semicolon: empty statement. Tolerated quietly to match
        // the Java reference which accepts `} ; var x …`.
        S::Semicolon => {
            p.bump();
        }
        S::Eof => {}
        // Experimental `type Name = T` alias declaration. `type` is a
        // contextual identifier (not a keyword), so this only triggers
        // on the exact `type IDENT =` shape — `type(x)` calls and
        // `type` variables keep parsing as expressions.
        _ if p.features().types
            && p.at(S::Ident)
            && p.current_text() == "type"
            && p.nth(1) == S::Ident
            && p.nth(2) == S::Eq =>
        {
            type_alias_decl(p);
        }
        // Experimental `interface Name { … }` declaration. Unlike
        // `type`, `interface` is a *reserved* keyword (v3+) lexed as
        // `KwInterface`, so the flag just unlocks the production —
        // without it the token stays an error like upstream.
        S::KwInterface if p.features().interfaces => {
            interface_decl(p);
        }
        // Experimental `enum Name { A, B = 10 }` declaration. `enum`
        // is a reserved keyword (v3+) like `interface`, so the flag
        // just unlocks the production.
        S::KwEnum if p.features().enums => {
            enum_decl(p);
        }
        _ if types::looks_like_typed_var_decl(p) => typed_var_decl_stmt_at(p, cp),
        _ => expr_stmt(p),
    }
}

/// `type IDENT = <type> [;]` — experimental alias declaration. The
/// node is not a statement at the AST layer (no `Stmt::cast` arm), so
/// HIR lowering skips it entirely; only the type checker reads it.
fn type_alias_decl(p: &mut Parser) {
    p.start_node(S::TypeAliasDecl);
    p.bump(); // contextual `type`
    p.expect(S::Ident); // alias name
    p.expect(S::Eq);
    types::ty(p);
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `interface IDENT { member* }` — experimental interface
/// declaration. Members are typed fields (`integer hp`) or bodiless
/// method signatures (`real area()`); a member with a `ParamList` is
/// a method. Like [`type_alias_decl`] the node has no `Stmt::cast`
/// arm, so HIR lowering skips it — interfaces are pure type
/// information with no runtime footprint.
fn interface_decl(p: &mut Parser) {
    p.start_node(S::InterfaceDecl);
    p.bump(); // `interface` keyword
    p.expect(S::Ident); // interface name
    p.expect(S::LBrace);
    while !p.at(S::RBrace) && !p.at_eof() {
        let before = p.position();
        interface_member(p);
        if !p.at(S::RBrace) && !p.at_eof() && p.position() == before {
            let kind = p.current();
            p.err_and_bump(format!("unexpected token in interface body: {kind:?}"));
        }
    }
    let _ = p.expect(S::RBrace);
    p.finish_node();
}

/// One interface member: `<type> IDENT` (field) or
/// `<type> IDENT ( params )` (method signature), each with an
/// optional trailing `;`.
fn interface_member(p: &mut Parser) {
    p.start_node(S::InterfaceMember);
    types::ty(p); // member type / method return type
    let _ = p.expect(S::Ident);
    if p.at(S::LParen) {
        super::decls::param_list(p);
    }
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `enum IDENT { variant (, variant)* ,? }` — experimental enum
/// declaration. Unlike `type`/`interface` the node DOES reach HIR:
/// lowering turns it into a class with static final integer fields,
/// so `Color.RED` is an ordinary static read at runtime.
fn enum_decl(p: &mut Parser) {
    p.start_node(S::EnumDecl);
    p.bump(); // `enum` keyword
    p.expect(S::Ident); // enum name
    p.expect(S::LBrace);
    while !p.at(S::RBrace) && !p.at_eof() {
        let before = p.position();
        enum_member(p);
        if !p.eat(S::Comma) && !p.at(S::RBrace) && !p.at_eof() && p.position() == before {
            let kind = p.current();
            p.err_and_bump(format!("unexpected token in enum body: {kind:?}"));
        }
    }
    let _ = p.expect(S::RBrace);
    p.finish_node();
}

/// One enum variant: `IDENT` or `IDENT = (-)? INT`. The value is
/// restricted to an integer literal (variants are compile-time
/// integer constants), so anything fancier is a parse error rather
/// than a silently-misbehaving runtime expression.
fn enum_member(p: &mut Parser) {
    p.start_node(S::EnumMember);
    let _ = p.expect(S::Ident);
    if p.eat(S::Eq) {
        let _ = p.eat(S::Minus);
        if !p.eat(S::IntLiteral) {
            let kind = p.current();
            p.err_and_bump(format!(
                "enum variant value must be an integer literal, found {kind:?}"
            ));
        }
    }
    p.finish_node();
}

/// `{ stmt* }` — block statement.
pub(super) fn block(p: &mut Parser) {
    assert!(p.at(S::LBrace));
    p.start_node(S::Block);
    p.bump(); // '{'
    while !p.at(S::RBrace) && !p.at_eof() {
        let before = p.position();
        stmt(p);
        // Defensive: force-bump on no progress.
        if !p.at(S::RBrace) && !p.at_eof() && p.position() == before {
            let kind = p.current();
            p.err_and_bump(format!("unexpected token in block: {kind:?}"));
        }
    }
    p.expect(S::RBrace);
    p.finish_node();
}

/// `if (expr) stmt [else stmt]`. `else` greedily attaches to the
/// nearest `if`, which is the standard dangling-else resolution and
/// what the Java reference does.
fn if_stmt(p: &mut Parser) {
    assert!(p.at(S::KwIf));
    p.start_node(S::IfStmt);
    p.bump(); // 'if'
    p.expect(S::LParen);
    super::expr::expr(p);
    p.expect(S::RParen);
    stmt(p); // then-branch
    if p.eat(S::KwElse) {
        stmt(p); // else-branch
    }
    p.finish_node();
}

/// `while (expr) stmt`.
fn while_stmt(p: &mut Parser) {
    assert!(p.at(S::KwWhile));
    p.start_node(S::WhileStmt);
    p.bump(); // 'while'
    p.expect(S::LParen);
    super::expr::expr(p);
    p.expect(S::RParen);
    stmt(p);
    p.finish_node();
}

fn break_stmt(p: &mut Parser) {
    assert!(p.at(S::KwBreak));
    p.start_node(S::BreakStmt);
    p.bump();
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

fn continue_stmt(p: &mut Parser) {
    assert!(p.at(S::KwContinue));
    p.start_node(S::ContinueStmt);
    p.bump();
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `do stmt while ( expr ) [;]`
fn do_while_stmt(p: &mut Parser) {
    assert!(p.at(S::KwDo));
    p.start_node(S::DoWhileStmt);
    p.bump(); // 'do'
    stmt(p);
    p.expect(S::KwWhile);
    p.expect(S::LParen);
    super::expr::expr(p);
    p.expect(S::RParen);
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `for (` then one of:
///   `var x in expr) stmt`                   foreach
///   `var k : var v in expr) stmt`           foreach key/value
///   `[init?] ; [cond?] ; [step?] ) stmt`    C-style
///
/// We look at the second non-trivia token after `(` to disambiguate.
fn for_or_foreach_stmt(p: &mut Parser) {
    assert!(p.at(S::KwFor));
    // Disambiguate by scanning the head for `in` outside the C-style
    // `; … ;` slots. If we see `in` first, it's a foreach.
    if is_foreach_head(p) {
        foreach_stmt(p);
    } else {
        for_stmt(p);
    }
}

/// Returns true if the head between `(` and `)` contains an `in`
/// keyword and no `;` — the signature of a foreach.
fn is_foreach_head(p: &Parser) -> bool {
    // `p.nth(0)` is the `for` keyword.
    // Walk from offset 1 forward, with paren-depth tracking.
    let mut i = 1usize;
    let mut depth = 0i32;
    let cap = 128;
    let mut steps = 0;
    while steps < cap {
        match p.nth(i) {
            S::LParen => depth += 1,
            S::RParen => {
                depth -= 1;
                if depth <= 0 {
                    return false;
                }
            }
            S::KwIn if depth == 1 => return true,
            S::Semicolon if depth == 1 => return false,
            S::Eof => return false,
            _ => {}
        }
        i += 1;
        steps += 1;
    }
    false
}

fn for_stmt(p: &mut Parser) {
    p.start_node(S::ForStmt);
    p.bump(); // 'for'
    p.expect(S::LParen);
    // init: var-decl, typed-decl, or expr — all optional
    if p.at(S::Semicolon) {
        p.expect(S::Semicolon);
    } else {
        match p.current() {
            S::KwVar => var_decl_stmt(p),
            _ if types::looks_like_typed_var_decl(p) => typed_var_decl_stmt(p),
            _ => expr_stmt(p),
        }
    }
    // cond
    if !p.at(S::Semicolon) {
        super::expr::expr(p);
    }
    p.expect(S::Semicolon);
    // step
    if !p.at(S::RParen) {
        super::expr::expr(p);
    }
    p.expect(S::RParen);
    stmt(p);
    p.finish_node();
}

/// `for ([var] [@] [type] IDENT [: [var] [@] [type] IDENT] in expr) stmt`.
/// The `@`-prefix marks a reference binding (legacy v1 form).
fn foreach_stmt(p: &mut Parser) {
    p.start_node(S::ForeachStmt);
    p.bump(); // 'for'
    p.expect(S::LParen);
    // First binding.
    let _ = p.eat(S::KwVar);
    let _ = p.eat(S::At); // optional reference marker
    if types::looks_like_type_then_name(p) {
        types::ty(p);
    }
    p.expect(S::Ident);
    // Optional `: <second binding>` for key/value foreach.
    if p.eat(S::Colon) {
        let _ = p.eat(S::KwVar);
        let _ = p.eat(S::At);
        if types::looks_like_type_then_name(p) {
            types::ty(p);
        }
        p.expect(S::Ident);
    }
    p.expect(S::KwIn);
    super::expr::expr(p);
    p.expect(S::RParen);
    stmt(p);
    p.finish_node();
}

/// `switch (expr) { (case expr : stmts)* (default : stmts)? }`
fn switch_stmt(p: &mut Parser) {
    assert!(p.at(S::KwSwitch));
    p.start_node(S::SwitchStmt);
    p.bump(); // 'switch'
    p.expect(S::LParen);
    super::expr::expr(p);
    p.expect(S::RParen);
    p.expect(S::LBrace);
    while !p.at(S::RBrace) && !p.at_eof() {
        if p.at(S::KwCase) || p.at(S::KwDefault) {
            switch_case(p);
        } else {
            let before = p.position();
            stmt(p);
            if !p.at(S::RBrace) && !p.at_eof() && p.position() == before {
                let kind = p.current();
                p.err_and_bump(format!("unexpected token in switch body: {kind:?}"));
            }
        }
    }
    p.expect(S::RBrace);
    p.finish_node();
}

fn switch_case(p: &mut Parser) {
    p.start_node(S::SwitchCase);
    if p.eat(S::KwCase) {
        super::expr::expr(p);
    } else {
        p.expect(S::KwDefault);
    }
    p.expect(S::Colon);
    // Body: zero or more statements until next case/default/closing brace.
    while !matches!(p.current(), S::KwCase | S::KwDefault | S::RBrace | S::Eof) {
        let before = p.position();
        stmt(p);
        if p.position() == before {
            let kind = p.current();
            p.err_and_bump(format!("unexpected token in case body: {kind:?}"));
            break;
        }
    }
    p.finish_node();
}

/// `include("name") [;]`
pub(super) fn include_stmt(p: &mut Parser) {
    assert!(p.at(S::KwInclude));
    p.start_node(S::IncludeStmt);
    p.bump(); // 'include'
    p.expect(S::LParen);
    if p.at(S::StringLiteral) {
        p.bump();
    } else {
        p.error("expected string literal for include name");
    }
    p.expect(S::RParen);
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `import "name" [;]`, `import a.b.c [;]`, or `import("name") [;]`
pub(super) fn import_stmt(p: &mut Parser) {
    assert!(p.at(S::KwImport));
    p.start_node(S::ImportStmt);
    p.bump(); // 'import'

    let has_parens = p.eat(S::LParen);
    if p.at(S::StringLiteral) {
        p.bump();
    } else if p.at(S::Ident) {
        p.bump();
        while p.eat(S::Dot) {
            if p.at(S::Ident) {
                p.bump();
            } else {
                p.error("expected identifier after `.` in import path");
                break;
            }
        }
    } else {
        p.error("expected string literal or dotted identifier for import name");
    }

    if has_parens {
        p.expect(S::RParen);
    }
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `<type> IDENT [= expr] [, IDENT [= expr]]* [;]`
///
/// Identical to `var_decl_stmt` except the leading keyword is the
/// type itself (parsed inside `VarDeclStmt`).
fn typed_var_decl_stmt(p: &mut Parser) {
    typed_var_decl_stmt_inner(p, None);
}

fn typed_var_decl_stmt_inner(p: &mut Parser, cp: Option<rowan::Checkpoint>) {
    if let Some(cp) = cp {
        p.start_node_at(cp, S::VarDeclStmt);
    } else {
        p.start_node(S::VarDeclStmt);
    }
    types::ty(p);
    if !p.expect(S::Ident) {
        skip_to_stmt_end(p);
        p.finish_node();
        return;
    }
    if p.eat(S::Eq) {
        super::expr::expr_top(p);
    }
    while p.at(S::Comma) {
        p.bump();
        if !p.expect(S::Ident) {
            break;
        }
        if p.eat(S::Eq) {
            super::expr::expr_top(p);
        }
    }
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `var IDENT [= expr] [, IDENT [= expr]]* [;]`. The caller passes a
/// checkpoint so any preceding annotations wrap into the decl node.
fn var_decl_stmt_at(p: &mut Parser, cp: rowan::Checkpoint) {
    var_decl_stmt_inner(p, Some(cp));
}

fn typed_var_decl_stmt_at(p: &mut Parser, cp: rowan::Checkpoint) {
    typed_var_decl_stmt_inner(p, Some(cp));
}

/// `global [type] IDENT [= expr] [, [type] IDENT [= expr]]* [;]`
/// Reuses the `VarDeclStmt` node kind — `KwGlobal` as the first token
/// disambiguates from local declarations at the AST layer.
fn global_decl_stmt_at(p: &mut Parser, cp: rowan::Checkpoint) {
    assert!(p.at(S::KwGlobal));
    p.start_node_at(cp, S::VarDeclStmt);
    p.bump(); // 'global'

    // Optional leading type (e.g. `global integer x = 0`).
    if types::looks_like_type_then_name(p) {
        types::ty(p);
    }
    if !p.expect(S::Ident) {
        skip_to_stmt_end(p);
        p.finish_node();
        return;
    }
    if p.eat(S::Eq) {
        expr::expr(p);
    }
    while p.at(S::Comma) {
        p.bump();
        if types::looks_like_type_then_name(p) {
            types::ty(p);
        }
        if !p.expect(S::Ident) {
            break;
        }
        if p.eat(S::Eq) {
            expr::expr(p);
        }
    }
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `var IDENT [= expr] [, IDENT [= expr]]* [;]`
///
/// First slice supports the untyped `var` form only. The typed form
/// (`integer x = 5;`) needs a type-disambiguation lookahead and lands
/// once we have a real type parser.
fn var_decl_stmt(p: &mut Parser) {
    var_decl_stmt_inner(p, None);
}

fn var_decl_stmt_inner(p: &mut Parser, cp: Option<rowan::Checkpoint>) {
    assert!(p.at(S::KwVar));
    if let Some(cp) = cp {
        p.start_node_at(cp, S::VarDeclStmt);
    } else {
        p.start_node(S::VarDeclStmt);
    }
    p.bump(); // 'var'

    if !p.expect(S::Ident) {
        // Recovery: skip junk up to ';' so the rest of the file keeps parsing.
        skip_to_stmt_end(p);
        p.finish_node();
        return;
    }

    if p.eat(S::Eq) {
        expr::expr_top(p);
    }

    // Trailing commas → additional declarators sharing the var keyword.
    while p.at(S::Comma) {
        p.bump();
        if !p.expect(S::Ident) {
            break;
        }
        if p.eat(S::Eq) {
            expr::expr_top(p);
        }
    }

    // Semicolon optional in Leekscript.
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// `return [?] [expr] [;]` — the optional `?` is the "soft-return"
/// form documented in `doc/grammar.md` §4.
fn return_stmt(p: &mut Parser) {
    assert!(p.at(S::KwReturn));
    p.start_node(S::ReturnStmt);
    p.bump(); // 'return'
    let _ = p.eat(S::Question); // optional soft-return marker
    if !p.at_eof() && !p.at(S::Semicolon) && expr::can_start_expr(p.current()) {
        expr::expr(p);
    }
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// Any expression followed by an optional `;`.
fn expr_stmt(p: &mut Parser) {
    if !expr::can_start_expr(p.current()) {
        p.err_and_bump(format!("unexpected token: {:?}", p.current()));
        return;
    }
    p.start_node(S::ExprStmt);
    expr::expr_top(p);
    let _ = p.eat(S::Semicolon);
    p.finish_node();
}

/// Discard tokens until we reach a statement boundary. The boundary
/// token itself is not consumed.
fn skip_to_stmt_end(p: &mut Parser) {
    while !p.at_eof() && !p.at_any(&[S::Semicolon, S::RBrace]) {
        p.bump();
    }
    let _ = p.eat(S::Semicolon);
}
