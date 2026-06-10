//! L0014 `IdenticalOperands` — flag a logical/bitwise op whose two
//! operands are the same side-effect-free expression: `x && x`,
//! `a || a`, `f & f`. The whole expression equals one operand, so it
//! ships a machine-applicable autofix.

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, TextEdit, codes};
use leek_hir::{BinaryOp, Expr, ExprKind};
use leek_span::Span;

use super::structural::{expr_key, has_side_effect};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct IdenticalOperands;

static META: LintMeta = LintMeta {
    name: "identical-operands",
    code: codes::IDENTICAL_OPERANDS,
    group: LintGroup::Suspicious,
    description: "logical/bitwise expression whose two operands are identical",
};

impl LintPass for IdenticalOperands {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Binary(op, a, b) = &e.kind
            && collapses_to_operand(*op)
            && !has_side_effect(a)
            && expr_key(a) == expr_key(b)
        {
            cx.emit(diagnostic(*op, e.span, a.span));
        }
    }
}

/// Ops where `x OP x == x`: logical and/or (short-circuit) and bitwise
/// and/or (idempotent). Excludes `xor`/`-`/`/` (which collapse to a
/// *constant*, a different rewrite handled elsewhere or not at all).
fn collapses_to_operand(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::And | BinaryOp::Or | BinaryOp::BitAnd | BinaryOp::BitOr
    )
}

fn diagnostic(op: BinaryOp, expr: Span, operand: Span) -> Diagnostic {
    let word = match op {
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
        BinaryOp::BitAnd => "&",
        _ => "|",
    };
    let mut d = Diagnostic::warning(
        codes::IDENTICAL_OPERANDS,
        expr,
        "both operands of this expression are identical".to_string(),
    );
    // Bitwise `&`/`|` are idempotent on integers, so `x & x` is exactly
    // `x` — safe to rewrite. Logical `&&`/`||` are *not* (Leekscript's
    // short-circuit coerces to a boolean in some cases, so `x && x` can
    // differ from `x`), so we flag the redundancy but offer no fix.
    if matches!(op, BinaryOp::BitAnd | BinaryOp::BitOr) {
        d = d
            .with_note(format!(
                "`x {word} x` is just `x` — did you mean a different operand?"
            ))
            .with_suggestion(Suggestion {
                message: "use the operand directly".to_string(),
                // Drop the ` OP <rhs>` tail, leaving just the (left) operand.
                edits: vec![TextEdit {
                    span: Span::new(expr.source, operand.end, expr.end),
                    replacement: String::new(),
                }],
                applicability: Applicability::MachineApplicable,
            });
    } else {
        d = d.with_note(format!(
            "both sides of this `{word}` are the same — did you mean a different operand, like `x {word} y`?"
        ));
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(IdenticalOperands, src)
    }

    #[test]
    fn flags_logical_and_without_fix() {
        // `&&` is flagged (redundant), but NOT auto-fixed: `x && x` can
        // differ from `x` because Leekscript's `&&` may coerce to bool.
        let d = run("function f(x) {\n  return x && x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].suggestions.is_empty(), "no autofix for &&: {d:?}");
    }

    #[test]
    fn flags_bitwise_or_with_fix() {
        let d = run("function f(x) {\n  return x | x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert_eq!(
            d[0].suggestions[0].applicability,
            Applicability::MachineApplicable
        );
    }

    #[test]
    fn ignores_distinct_operands() {
        let d = run("function f(x, y) {\n  return x && y\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_calls() {
        let d = run("var x = rand() && rand()\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
