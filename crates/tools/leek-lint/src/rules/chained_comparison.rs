//! L0033 `ChainedComparison` — flag `a < b < c`, which does **not**
//! test "b between a and c": comparisons are left-associative, so it
//! parses as `(a < b) < c` — a boolean compared against `c`.
//!
//! ```leekscript
//! if (0 < x < 10) { … }     // really: (0 < x) < 10 — always true!
//! if (0 < x && x < 10) { … }  // what was meant
//! ```
//!
//! A classic trap for people coming from math notation (or Python,
//! where chaining works). Modeled on `pylint`'s `chained-comparison`
//! and clippy's `double_comparisons` family.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Expr, ExprKind};

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct ChainedComparison;

static META: LintMeta = LintMeta {
    name: "chained-comparison",
    code: codes::CHAINED_COMPARISON,
    group: LintGroup::Suspicious,
    description: "`a < b < c` compares a boolean with `c` — write `a < b && b < c`",
};

impl LintPass for ChainedComparison {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        let ExprKind::Binary(op, lhs, rhs) = &e.kind else {
            return;
        };
        if !is_ordering(*op) {
            return;
        }
        // Left-associative parse puts the inner comparison on the
        // left; check the right too in case of explicit grouping.
        let inner_is_comparison = [lhs, rhs]
            .into_iter()
            .any(|side| matches!(&side.kind, ExprKind::Binary(inner, ..) if is_ordering(*inner)));
        if !inner_is_comparison {
            return;
        }
        cx.emit(
            Diagnostic::warning(
                codes::CHAINED_COMPARISON,
                e.span,
                "chained comparison does not test a range".to_string(),
            )
            .with_note(
                "`a < b < c` is `(a < b) < c` — the boolean result of the first comparison is compared with `c`. For a range test, write `a < b && b < c`",
            ),
        );
    }
}

fn is_ordering(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(ChainedComparison, src)
    }

    #[test]
    fn flags_three_way_chain() {
        let d = run("function f(x) {\n  if (0 < x < 10) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_mixed_chain() {
        let d = run("function f(a, b, c) {\n  return a <= b < c\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_plain_comparison() {
        let d = run("function f(a, b) {\n  return a < b\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_correct_range_test() {
        let d = run("function f(x) {\n  if (0 < x && x < 10) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
