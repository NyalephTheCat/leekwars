//! L0021 `NegatedComparison` — flag `!(a == b)` and friends, which read
//! more clearly as the negated comparison (`a != b`). A readability hint.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Expr, ExprKind, UnaryOp};

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct NegatedComparison;

static META: LintMeta = LintMeta {
    name: "negated-comparison",
    code: codes::NEGATED_COMPARISON,
    group: LintGroup::Style,
    description: "`!(a == b)` reads more clearly as `a != b`",
};

impl LintPass for NegatedComparison {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Unary(UnaryOp::Not, inner) = &e.kind
            && let ExprKind::Binary(op, ..) = &inner.kind
            && let Some((had, want)) = negation(*op)
        {
            cx.emit(diagnostic(had, want, e.span));
        }
    }
}

/// For a comparison op, the `(spelling, negated-spelling)` pair; `None`
/// for ops without a clean De-Morgan-free negation.
fn negation(op: BinaryOp) -> Option<(&'static str, &'static str)> {
    Some(match op {
        BinaryOp::Eq => ("==", "!="),
        BinaryOp::Ne => ("!=", "=="),
        BinaryOp::Lt => ("<", ">="),
        BinaryOp::Le => ("<=", ">"),
        BinaryOp::Gt => (">", "<="),
        BinaryOp::Ge => (">=", "<"),
        _ => return None,
    })
}

fn diagnostic(had: &str, want: &str, span: leek_span::Span) -> Diagnostic {
    Diagnostic::new(
        codes::NEGATED_COMPARISON,
        leek_diagnostics::Severity::Hint,
        span,
        format!("`!(a {had} b)` is clearer as `a {want} b`"),
    )
    .with_note(format!(
        "rewrite the negated comparison using `{want}` directly"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(NegatedComparison, src)
    }

    #[test]
    fn flags_negated_equality() {
        let d = run("function f(a, b) {\n  return !(a == b)\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("!="), "{d:?}");
    }

    #[test]
    fn flags_negated_less_than() {
        let d = run("function f(a, b) {\n  return !(a < b)\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains(">="), "{d:?}");
    }

    #[test]
    fn ignores_plain_not() {
        let d = run("function f(x) {\n  return !x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
