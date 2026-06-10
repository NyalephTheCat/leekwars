//! L0030 `ManualMinMax` (pedantic) — flag a ternary that re-implements
//! `min`/`max` by hand:
//!
//! ```leekscript
//! a > b ? a : b     // max(a, b)
//! a < b ? a : b     // min(a, b)
//! ```
//!
//! The builtins say what's *meant*, not how it's computed — and they
//! evaluate each operand once. Only side-effect-free operands are
//! compared (a call could legitimately differ between the condition
//! and the arm). Inspired by clippy's `manual_clamp` family.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Expr, ExprKind};

use super::structural::{expr_key, has_side_effect};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct ManualMinMax;

static META: LintMeta = LintMeta {
    name: "manual-min-max",
    code: codes::MANUAL_MIN_MAX,
    group: LintGroup::Pedantic,
    description: "ternary re-implementing `min`/`max` — use the builtin",
};

impl LintPass for ManualMinMax {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        let ExprKind::Ternary(cond, then, els) = &e.kind else {
            return;
        };
        let ExprKind::Binary(op, lhs, rhs) = &cond.kind else {
            return;
        };
        if has_side_effect(lhs) || has_side_effect(rhs) {
            return;
        }
        let (lk, rk) = (expr_key(lhs), expr_key(rhs));
        let (tk, fk) = (expr_key(then), expr_key(els));
        // `a > b ? a : b` picks the larger; `a > b ? b : a` the smaller
        // (and symmetrically for `<`). `>=`/`<=` differ only in which
        // operand wins a tie — same value, same rewrite.
        let builtin = match op {
            BinaryOp::Gt | BinaryOp::Ge if tk == lk && fk == rk => "max",
            BinaryOp::Gt | BinaryOp::Ge if tk == rk && fk == lk => "min",
            BinaryOp::Lt | BinaryOp::Le if tk == lk && fk == rk => "min",
            BinaryOp::Lt | BinaryOp::Le if tk == rk && fk == lk => "max",
            _ => return,
        };
        cx.emit(
            Diagnostic::new(
                codes::MANUAL_MIN_MAX,
                leek_diagnostics::Severity::Hint,
                e.span,
                format!("this ternary is `{builtin}` written by hand"),
            )
            .with_note(format!(
                "`{builtin}(a, b)` says what you mean and evaluates each operand once"
            )),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(ManualMinMax, src)
    }

    #[test]
    fn flags_manual_max() {
        let d = run("function f(a, b) {\n  return a > b ? a : b\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("max"), "{d:?}");
    }

    #[test]
    fn flags_manual_min_via_gt() {
        let d = run("function f(a, b) {\n  return a > b ? b : a\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("min"), "{d:?}");
    }

    #[test]
    fn flags_manual_min_via_le() {
        let d = run("function f(a, b) {\n  return a <= b ? a : b\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("min"), "{d:?}");
    }

    #[test]
    fn ignores_unrelated_ternary() {
        let d = run("function f(a, b, c) {\n  return a > b ? c : b\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_side_effecting_operands() {
        let d = run("function f(b) {\n  return rand() > b ? rand() : b\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
