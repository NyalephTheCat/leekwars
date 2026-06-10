//! Flow-sensitive type narrowing.
//!
//! From a boolean condition we extract `(positive, negative)` facts —
//! variable bindings that hold when the condition is true vs. false.
//! Branch checkers (`if` / `while` / ternary) push a scope, apply the
//! relevant facts via [`Checker::apply_narrowings`], and pop it, so the
//! refinement is local to the guarded branch.
//!
//! Supported guards:
//!   - `x instanceof T`        → `x : T` (positive)
//!   - `x == null`             → `x : null` / else non-null
//!   - `x != null`             → `x : non-null` / else null
//!   - `a && b`, `a || b`, `!a`/`not a` combine the above

use super::prelude::*;
use leek_parser::ast::BinaryExpr;

/// One narrowing fact: bind `name` to `ty` within a guarded branch.
pub(crate) type Narrowing = (String, Type);

impl Checker {
    /// Re-declare each narrowed name in the current (already-pushed)
    /// scope so lookups inside the branch see the refined type.
    pub(crate) fn apply_narrowings(&mut self, facts: &[Narrowing]) {
        for (name, ty) in facts {
            self.declare(name, ty.clone());
        }
    }

    /// `(positive, negative)` narrowing facts for a boolean condition.
    pub(crate) fn condition_narrowings(&self, cond: &Expr) -> (Vec<Narrowing>, Vec<Narrowing>) {
        match cond {
            Expr::Paren(p) => p
                .inner()
                .map(|i| self.condition_narrowings(&i))
                .unwrap_or_default(),
            Expr::Unary(u) => {
                // `!cond` / `not cond` swaps the true/false facts.
                if let Some(op) = u.op()
                    && matches!(op.kind(), SyntaxKind::Bang | SyntaxKind::KwNot)
                    && let Some(operand) = u.operand()
                {
                    let (p, n) = self.condition_narrowings(&operand);
                    return (n, p);
                }
                (Vec::new(), Vec::new())
            }
            Expr::Binary(b) => self.binary_narrowings(b),
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn binary_narrowings(&self, b: &BinaryExpr) -> (Vec<Narrowing>, Vec<Narrowing>) {
        let Some(op) = b.op() else {
            return (Vec::new(), Vec::new());
        };
        match op.kind() {
            SyntaxKind::KwInstanceof => {
                // `x instanceof T` parses `T` as a TypeRef (not an
                // expression), so read the narrowed type from it.
                let name = b.lhs().as_ref().and_then(lvalue_name);
                let ty = b
                    .syntax()
                    .children()
                    .find(|n| n.kind() == SyntaxKind::TypeRef)
                    .map(|n| self.resolve_type_node(&n));
                if let (Some(name), Some(ty)) = (name, ty) {
                    return (vec![(name, ty)], Vec::new());
                }
                (Vec::new(), Vec::new())
            }
            SyntaxKind::EqEq | SyntaxKind::EqEqEq => self.null_check_narrowing(b, true),
            SyntaxKind::NotEq | SyntaxKind::NotEqEq => self.null_check_narrowing(b, false),
            // `a && b`: both facts hold when true. A simple negation
            // isn't derivable (De Morgan), so leave the else side empty.
            SyntaxKind::AmpAmp | SyntaxKind::KwAnd => {
                let (lp, _) = b
                    .lhs()
                    .map(|e| self.condition_narrowings(&e))
                    .unwrap_or_default();
                let (rp, _) = b
                    .rhs()
                    .map(|e| self.condition_narrowings(&e))
                    .unwrap_or_default();
                (concat(lp, rp), Vec::new())
            }
            // `a || b`: both facts hold when false.
            SyntaxKind::PipePipe | SyntaxKind::KwOr => {
                let (_, ln) = b
                    .lhs()
                    .map(|e| self.condition_narrowings(&e))
                    .unwrap_or_default();
                let (_, rn) = b
                    .rhs()
                    .map(|e| self.condition_narrowings(&e))
                    .unwrap_or_default();
                (Vec::new(), concat(ln, rn))
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    /// `x == null` (`is_eq`) / `x != null`. Either operand may be the
    /// `null` literal.
    fn null_check_narrowing(
        &self,
        b: &BinaryExpr,
        is_eq: bool,
    ) -> (Vec<Narrowing>, Vec<Narrowing>) {
        let (lhs, rhs) = (b.lhs(), b.rhs());
        let name = match (&lhs, &rhs) {
            (Some(l), Some(r)) if is_null_literal(r) => lvalue_name(l),
            (Some(l), Some(r)) if is_null_literal(l) => lvalue_name(r),
            _ => None,
        };
        let Some(name) = name else {
            return (Vec::new(), Vec::new());
        };
        let cur = self.lookup(&name).cloned().unwrap_or(Type::Any);
        let non_null = strip_nullable(&cur);
        if is_eq {
            // `== null`: true → null, false → non-null.
            (vec![(name.clone(), Type::Null)], vec![(name, non_null)])
        } else {
            // `!= null`: true → non-null, false → null.
            (vec![(name.clone(), non_null)], vec![(name, Type::Null)])
        }
    }
}

/// The simple variable name an expression refers to (`x`, `(x)`), or
/// `None` for anything more complex.
fn lvalue_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => n.ident().map(|t| t.text().to_string()),
        Expr::Paren(p) => p.inner().and_then(|i| lvalue_name(&i)),
        _ => None,
    }
}

fn is_null_literal(e: &Expr) -> bool {
    matches!(e, Expr::Literal(l) if l.token().map(|t| t.kind()) == Some(SyntaxKind::KwNull))
}

fn concat(mut a: Vec<Narrowing>, b: Vec<Narrowing>) -> Vec<Narrowing> {
    a.extend(b);
    a
}
