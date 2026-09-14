//! Closure-capture queries shared by the MIR charge model and the Java
//! emitter.
//!
//! Upstream boxes any function or lambda parameter that a *nested*
//! closure captures — read or write — at the callee's entry (`final var
//! u_a = new Box<>(AI.this, p_a)`). That single fact has two consumers
//! that must agree:
//!
//! - `leek-backend-java` emits the box, so its answer decides the shape
//!   of the generated Java;
//! - `leek-mir` charges the 2-arg `Box` constructor 1 op per call, so
//!   its answer decides the op count.
//!
//! Each crate used to keep its own copy of this walk, with a comment in
//! `leek-mir` asking the next editor to keep them in step by hand. They
//! live here instead, so a divergence has to be written down as a
//! parameter rather than happening quietly on one side.
//!
//! The walk is scope-free on purpose: [`DefId`]s are unique per binding,
//! so a nested parameter or local can never alias the `def` being asked
//! about.

use crate::ir::{DefId, Expr, ExprKind, LambdaBody, NameRef, Stmt};
use crate::visit::{walk_expr_children, walk_stmt_child_exprs, walk_stmt_child_stmts};

/// True when `def` is referenced anywhere inside `e`, descending through
/// nested lambda bodies (unlike [`captured_in_expr`], which treats a lambda
/// as a leaf until it finds one).
fn refs_def_deep(e: &Expr, def: DefId) -> bool {
    match &e.kind {
        ExprKind::Name(NameRef::Local(id)) if *id == def => true,
        ExprKind::Lambda(l) => match &l.body {
            LambdaBody::Expr(b) => refs_def_deep(b, def),
            LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt_refs_def_deep(s, def)),
        },
        _ => {
            // `walk_expr_children` doesn't surface a `Callee::Function`
            // name (it's a NameRef, not a child Expr) — check it here so
            // a captured first-class callable param (`a()`) is caught.
            if let ExprKind::Call(c) = &e.kind
                && matches!(&c.callee, crate::ir::Callee::Function(NameRef::Local(id)) if *id == def)
            {
                return true;
            }
            let mut found = false;
            walk_expr_children(e, &mut |c| found = found || refs_def_deep(c, def));
            found
        }
    }
}

fn stmt_refs_def_deep(s: &Stmt, def: DefId) -> bool {
    let mut found = false;
    walk_stmt_child_exprs(s, &mut |e| found = found || refs_def_deep(e, def));
    if !found {
        walk_stmt_child_stmts(s, &mut |c| found = found || stmt_refs_def_deep(c, def));
    }
    found
}

/// True when some lambda inside `e` references `def`. `e` itself being a
/// reference to `def` does not count — the question is whether a *closure*
/// captures it.
fn captured_in_expr(e: &Expr, def: DefId) -> bool {
    if let ExprKind::Lambda(l) = &e.kind {
        return match &l.body {
            LambdaBody::Expr(b) => refs_def_deep(b, def),
            LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt_refs_def_deep(s, def)),
        };
    }
    let mut found = false;
    walk_expr_children(e, &mut |c| found = found || captured_in_expr(c, def));
    found
}

/// True when a lambda nested anywhere inside `stmts` references `def`
/// (at any lambda-nesting depth).
///
/// See the module docs for what both consumers do with the answer.
#[must_use]
pub fn captured_by_nested_lambda_stmts(stmts: &[Stmt], def: DefId) -> bool {
    fn walk(s: &Stmt, def: DefId) -> bool {
        let mut found = false;
        walk_stmt_child_exprs(s, &mut |e| found = found || captured_in_expr(e, def));
        if !found {
            walk_stmt_child_stmts(s, &mut |c| found = found || walk(c, def));
        }
        found
    }
    stmts.iter().any(|s| walk(s, def))
}

/// [`captured_by_nested_lambda_stmts`] for a lambda's own body — its
/// params get the same Box treatment when an inner lambda captures them
/// (`x -> y -> x + 1` boxes `x` at the outer lambda's entry).
#[must_use]
pub fn captured_by_nested_lambda_body(body: &LambdaBody, def: DefId) -> bool {
    match body {
        LambdaBody::Expr(e) => captured_in_expr(e, def),
        LambdaBody::Block(b) => captured_by_nested_lambda_stmts(&b.stmts, def),
    }
}
