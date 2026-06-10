//! L0010 `SelfComparison` — flag a comparison whose two operands are
//! the same side-effect-free expression: `x == x`, `a < a`,
//! `this.n != this.n`. Such a comparison is constant (always true or
//! always false) — usually a typo for a different variable.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Expr, ExprKind};

use super::structural::{expr_key, has_side_effect};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct SelfComparison;

static META: LintMeta = LintMeta {
    name: "self-comparison",
    code: codes::SELF_COMPARISON,
    group: LintGroup::Suspicious,
    description: "comparison whose two sides are identical — always true or always false",
};

impl LintPass for SelfComparison {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Binary(op, a, b) = &e.kind
            && is_comparison(*op)
            && !has_side_effect(a)
            && expr_key(a) == expr_key(b)
        {
            cx.emit(diagnostic(*op, e.span));
        }
    }
}

fn is_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::IdentityEq
            | BinaryOp::IdentityNe
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge
    )
}

fn diagnostic(op: BinaryOp, span: leek_span::Span) -> Diagnostic {
    // Equality/`<=`/`>=` are always true; `!=`/`<`/`>` always false.
    let constant = match op {
        BinaryOp::Eq | BinaryOp::IdentityEq | BinaryOp::Le | BinaryOp::Ge => "always true",
        _ => "always false",
    };
    Diagnostic::warning(
        codes::SELF_COMPARISON,
        span,
        "both sides of this comparison are identical".to_string(),
    )
    .with_note(format!(
        "this comparison is {constant} — e.g. `x == x` is always true. \
         Did you mean to compare against a different variable, like `x == y`?"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(SelfComparison, src)
    }

    #[test]
    fn flags_self_equality() {
        let d = run("function f(x) {\n  return x == x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes.iter().any(|n| n.contains("always true")));
    }

    #[test]
    fn flags_self_less_than() {
        let d = run("function f(x) {\n  return x < x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes.iter().any(|n| n.contains("always false")));
    }

    #[test]
    fn ignores_different_operands() {
        let d = run("function f(x, y) {\n  return x == y\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_calls() {
        // `rand() == rand()` may legitimately differ — not flagged.
        let d = run("var x = rand() == rand()\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
