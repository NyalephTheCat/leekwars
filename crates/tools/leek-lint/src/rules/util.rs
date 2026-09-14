//! Predicates shared by more than one rule.
//!
//! Small HIR shape tests that two lints would otherwise each spell out.
//! Structural comparison lives next door in [`super::structural`]; this module
//! is for the one-shape questions ("is this a `++`?", "is this a bool
//! literal?").

use leek_hir::{BinaryOp, DefId, Expr, ExprKind, Literal, NameRef, PostfixOp, UnaryOp};

/// `i++`, `++i`, or `i += 1`.
pub(crate) fn step_is_increment(step: Option<&Expr>, counter: DefId) -> bool {
    let Some(step) = step else { return false };
    match &step.kind {
        ExprKind::Postfix(PostfixOp::PostInc, e) | ExprKind::Unary(UnaryOp::PreInc, e) => {
            is_counter(e, counter)
        }
        ExprKind::Binary(BinaryOp::AddAssign, lhs, rhs) => {
            is_counter(lhs, counter) && matches!(&rhs.kind, ExprKind::Literal(Literal::Int(1)))
        }
        _ => false,
    }
}

/// Whether `e` is a bare reference to the local `counter`.
pub(crate) fn is_counter(e: &Expr, counter: DefId) -> bool {
    matches!(&e.kind, ExprKind::Name(NameRef::Local(d)) if *d == counter)
}

/// The value of `e` when it is a boolean literal.
pub(crate) fn bool_lit(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Literal(Literal::Bool(b)) => Some(*b),
        _ => None,
    }
}
