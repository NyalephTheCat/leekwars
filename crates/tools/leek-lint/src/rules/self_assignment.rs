//! L0011 `SelfAssignment` — flag `x = x`, `this.n = this.n`, etc.: an
//! assignment whose target and value are the same side-effect-free
//! place. It does nothing — usually a leftover or a typo.

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, TextEdit, codes};
use leek_hir::{BinaryOp, Expr, ExprKind};

use super::structural::{expr_key, has_side_effect};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct SelfAssignment;

static META: LintMeta = LintMeta {
    name: "self-assignment",
    code: codes::SELF_ASSIGNMENT,
    group: LintGroup::Correctness,
    description: "assignment of a value to itself — has no effect",
};

impl LintPass for SelfAssignment {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        // Only plain `=`; compound forms (`x += x`) are not no-ops.
        if let ExprKind::Binary(BinaryOp::Assign, lhs, rhs) = &e.kind
            && !has_side_effect(lhs)
            && expr_key(lhs) == expr_key(rhs)
        {
            cx.emit(diagnostic(e.span));
        }
    }
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::SELF_ASSIGNMENT,
        span,
        "this assignment has no effect (assigns a value to itself)".to_string(),
    )
    .with_note("`x = x` does nothing — remove it, or assign the value you intended")
    .with_suggestion(Suggestion {
        message: "remove the assignment".to_string(),
        edits: vec![TextEdit {
            span,
            replacement: String::new(),
        }],
        // Deleting the expression leaves the statement's `;` behind
        // (a harmless empty statement), so flag for a human glance.
        applicability: Applicability::MaybeIncorrect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(SelfAssignment, src)
    }

    #[test]
    fn flags_variable_self_assignment() {
        let d = run("function f() {\n  var x = 1\n  x = x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_real_assignment() {
        let d = run("function f(y) {\n  var x = 1\n  x = y\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_compound_assignment() {
        let d = run("function f() {\n  var x = 1\n  x += x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
