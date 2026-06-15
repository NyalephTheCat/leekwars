//! Constant folding — a deliberately small, semantics-safe subset.
//!
//! We fold only operations whose result is unambiguous under official
//! LeekScript semantics: integer `+ - *` (skipping overflow), integer
//! comparisons, boolean `! && ||` and equality, and ternaries with a
//! literal boolean condition. Division, power, bitwise, and real
//! arithmetic are left untouched to avoid any version-dependent or
//! precision divergence.

use leek_hir::{BinaryOp, Expr, ExprKind, Literal, Stmt, UnaryOp};
use leek_hir::{walk_expr_children_mut, walk_stmt_child_exprs_mut, walk_stmt_child_stmts_mut};

/// Fold all expressions reachable from a statement (and its children).
pub(crate) fn fold_stmt(s: &mut Stmt) {
    walk_stmt_child_exprs_mut(s, &mut fold_expr);
    walk_stmt_child_stmts_mut(s, &mut fold_stmt);
}

/// Fold an expression bottom-up.
pub(crate) fn fold_expr(e: &mut Expr) {
    walk_expr_children_mut(e, &mut fold_expr);
    if let Some(repl) = reduced(e) {
        *e = repl;
    }
}

fn reduced(e: &Expr) -> Option<Expr> {
    match &e.kind {
        ExprKind::Binary(op, l, r) => fold_binary(*op, l, r).map(|lit| lit_expr(lit, e)),
        ExprKind::Unary(op, x) => fold_unary(*op, x).map(|lit| lit_expr(lit, e)),
        ExprKind::Ternary(cond, then, els) => match as_bool(cond) {
            Some(true) => Some((**then).clone()),
            Some(false) => Some((**els).clone()),
            None => None,
        },
        _ => None,
    }
}

fn lit_expr(lit: Literal, orig: &Expr) -> Expr {
    Expr {
        kind: ExprKind::Literal(lit),
        ty: orig.ty.clone(),
        span: orig.span,
    }
}

fn as_int(e: &Expr) -> Option<i64> {
    match &e.kind {
        ExprKind::Literal(Literal::Int(i)) => Some(*i),
        _ => None,
    }
}

fn as_bool(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Literal(Literal::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn fold_binary(op: BinaryOp, l: &Expr, r: &Expr) -> Option<Literal> {
    use BinaryOp as B;
    if let (Some(a), Some(b)) = (as_int(l), as_int(r)) {
        return match op {
            B::Add => a.checked_add(b).map(Literal::Int),
            B::Sub => a.checked_sub(b).map(Literal::Int),
            B::Mul => a.checked_mul(b).map(Literal::Int),
            B::Eq => Some(Literal::Bool(a == b)),
            B::Ne => Some(Literal::Bool(a != b)),
            B::Lt => Some(Literal::Bool(a < b)),
            B::Le => Some(Literal::Bool(a <= b)),
            B::Gt => Some(Literal::Bool(a > b)),
            B::Ge => Some(Literal::Bool(a >= b)),
            _ => None,
        };
    }
    if let (Some(a), Some(b)) = (as_bool(l), as_bool(r)) {
        return match op {
            B::And => Some(Literal::Bool(a && b)),
            B::Or => Some(Literal::Bool(a || b)),
            B::Eq => Some(Literal::Bool(a == b)),
            B::Ne => Some(Literal::Bool(a != b)),
            _ => None,
        };
    }
    None
}

fn fold_unary(op: UnaryOp, x: &Expr) -> Option<Literal> {
    match op {
        UnaryOp::Neg => as_int(x).and_then(i64::checked_neg).map(Literal::Int),
        UnaryOp::Not => as_bool(x).map(|b| Literal::Bool(!b)),
        _ => None,
    }
}
