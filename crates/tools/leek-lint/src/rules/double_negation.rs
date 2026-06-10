//! L0013 `DoubleNegation` — flag `!!x` / `not not x`, which is just
//! `x` (booleanized). Ships a machine-applicable autofix that strips
//! the leading `!!`.

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, TextEdit, codes};
use leek_hir::{Expr, ExprKind, UnaryOp};
use leek_span::Span;

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct DoubleNegation;

static META: LintMeta = LintMeta {
    name: "double-negation",
    code: codes::DOUBLE_NEGATION,
    group: LintGroup::Complexity,
    description: "`!!x` is just `x` — the double negation is redundant",
};

impl LintPass for DoubleNegation {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if let ExprKind::Unary(UnaryOp::Not, inner) = &e.kind
            && let ExprKind::Unary(UnaryOp::Not, innermost) = &inner.kind
        {
            cx.emit(diagnostic(e.span, innermost.span));
        }
    }
}

fn diagnostic(outer: Span, operand: Span) -> Diagnostic {
    // Delete the `!!` (everything from the outer `!` up to the operand).
    let fix = Suggestion {
        message: "remove the double negation".to_string(),
        edits: vec![TextEdit {
            span: Span::new(outer.source, outer.start, operand.start),
            replacement: String::new(),
        }],
        applicability: Applicability::MachineApplicable,
    };
    Diagnostic::new(
        codes::DOUBLE_NEGATION,
        leek_diagnostics::Severity::Hint,
        outer,
        "double negation is redundant".to_string(),
    )
    .with_note("`!!x` is just `x` — remove the extra `!`")
    .with_suggestion(fix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DoubleNegation, src)
    }

    #[test]
    fn flags_double_bang() {
        let d = run("function f(x) {\n  return !!x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert_eq!(
            d[0].suggestions[0].applicability,
            Applicability::MachineApplicable
        );
    }

    #[test]
    fn ignores_single_negation() {
        let d = run("function f(x) {\n  return !x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
