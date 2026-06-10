//! L0019 `RedundantTernary` — flag pointless ternaries:
//!
//! - `cond ? a : a` — both arms equal, so the result is `a` regardless.
//!   Auto-fixed to `a` when `cond` is side-effect-free.
//! - `cond ? true : false` / `cond ? false : true` — this just yields
//!   the boolean value of `cond`. Flagged (no fix, since the exact
//!   rewrite depends on `cond` already being boolean).

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, TextEdit, codes};
use leek_hir::{Expr, ExprKind, Literal};
use leek_span::Span;

use super::structural::{expr_key, has_side_effect};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct RedundantTernary;

static META: LintMeta = LintMeta {
    name: "redundant-ternary",
    code: codes::REDUNDANT_TERNARY,
    group: LintGroup::Complexity,
    description: "ternary with identical arms or boolean-literal arms",
};

impl LintPass for RedundantTernary {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Ternary(cond, then, els) = &e.kind {
            if expr_key(then) == expr_key(els) {
                cx.emit(identical_arms(e.span, then.span, cond));
            } else if let (Some(t), Some(f)) = (bool_lit(then), bool_lit(els))
                && t != f
            {
                cx.emit(boolean_ternary(e.span));
            }
        }
    }
}

fn bool_lit(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Literal(Literal::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn identical_arms(expr: Span, then: Span, cond: &Expr) -> Diagnostic {
    let mut d = Diagnostic::new(
        codes::REDUNDANT_TERNARY,
        leek_diagnostics::Severity::Hint,
        expr,
        "both branches of this ternary are identical".to_string(),
    )
    .with_note("the condition has no effect — `c ? a : a` is just `a`");
    // Only safe to drop the condition when it has no side effects.
    if !has_side_effect(cond) {
        d = d.with_suggestion(Suggestion {
            message: "use the branch value directly".to_string(),
            edits: vec![
                // Delete the `cond ? ` prefix.
                TextEdit {
                    span: Span::new(expr.source, expr.start, then.start),
                    replacement: String::new(),
                },
                // Delete the ` : <else>` suffix.
                TextEdit {
                    span: Span::new(expr.source, then.end, expr.end),
                    replacement: String::new(),
                },
            ],
            applicability: Applicability::MachineApplicable,
        });
    }
    d
}

fn boolean_ternary(expr: Span) -> Diagnostic {
    Diagnostic::new(
        codes::REDUNDANT_TERNARY,
        leek_diagnostics::Severity::Hint,
        expr,
        "ternary returning boolean literals is redundant".to_string(),
    )
    .with_note("`c ? true : false` is just the boolean value of `c` — drop the ternary")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(RedundantTernary, src)
    }

    #[test]
    fn flags_identical_arms_with_fix() {
        let d = run("function f(c, a) {\n  return c ? a : a\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert_eq!(
            d[0].suggestions[0].applicability,
            Applicability::MachineApplicable
        );
    }

    #[test]
    fn flags_boolean_ternary_no_fix() {
        let d = run("function f(c) {\n  return c ? true : false\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].suggestions.is_empty(), "{d:?}");
    }

    #[test]
    fn ignores_distinct_arms() {
        let d = run("function f(c, a, b) {\n  return c ? a : b\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn no_fix_when_condition_has_side_effects() {
        let d = run("function f(a) {\n  return rand() ? a : a\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].suggestions.is_empty(), "side-effecting cond: {d:?}");
    }
}
