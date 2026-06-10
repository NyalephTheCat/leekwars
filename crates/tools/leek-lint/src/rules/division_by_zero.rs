//! L0016 `DivisionByZero` — flag a division or modulo by a literal
//! zero: `x / 0`, `n % 0`, `a \ 0`. The result is never useful (a fault
//! or a non-finite value), so it's almost always a mistake.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Expr, ExprKind, Literal};

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct DivisionByZero;

static META: LintMeta = LintMeta {
    name: "division-by-zero",
    code: codes::DIVISION_BY_ZERO,
    group: LintGroup::Correctness,
    description: "division or modulo by a literal zero — faults at runtime",
};

impl LintPass for DivisionByZero {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Binary(op, _, rhs) = &e.kind
            && is_division(*op)
            && is_zero(rhs)
        {
            cx.emit(diagnostic(*op, e.span));
        }
    }
}

fn is_division(op: BinaryOp) -> bool {
    matches!(op, BinaryOp::Div | BinaryOp::IntDiv | BinaryOp::Mod)
}

fn is_zero(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(Literal::Int(0)) => true,
        ExprKind::Literal(Literal::Real(r)) => *r == 0.0,
        _ => false,
    }
}

fn diagnostic(op: BinaryOp, span: leek_span::Span) -> Diagnostic {
    let (verb, what) = match op {
        BinaryOp::Mod => ("take", "modulo by zero"),
        _ => ("divide", "division by zero"),
    };
    Diagnostic::warning(codes::DIVISION_BY_ZERO, span, what.to_string()).with_note(format!(
        "you can't {verb} by zero — this faults or yields a non-finite value at runtime"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DivisionByZero, src)
    }

    #[test]
    fn flags_div_by_zero() {
        let d = run("function f(x) {\n  return x / 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_mod_by_zero() {
        let d = run("function f(x) {\n  return x % 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_nonzero_divisor() {
        let d = run("function f(x) {\n  return x / 2\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
