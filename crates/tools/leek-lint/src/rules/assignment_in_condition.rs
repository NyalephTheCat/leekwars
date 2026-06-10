//! L0015 `AssignmentInCondition` — flag an assignment used as a
//! condition: `if (x = 5)`, `while (n = next())`. This is almost always
//! a typo for `==`; the few intentional uses read more clearly written
//! out, so we always warn.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Expr, ExprKind, Stmt};

use super::structural::is_assignment;
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct AssignmentInCondition;

static META: LintMeta = LintMeta {
    name: "assignment-in-condition",
    code: codes::ASSIGNMENT_IN_CONDITION,
    group: LintGroup::Suspicious,
    description: "assignment used as a condition — likely a typo for `==`",
};

impl LintPass for AssignmentInCondition {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let cond = match s {
            Stmt::If(i) => Some(&i.cond),
            Stmt::While(w) => Some(&w.cond),
            Stmt::DoWhile(d) => Some(&d.cond),
            Stmt::For(f) => f.cond.as_ref(),
            _ => None,
        };
        if let Some(cond) = cond
            && let Some(span) = assignment_span(cond)
        {
            cx.emit(diagnostic(span));
        }
    }
}

/// The span of `cond` when it's an assignment expression, else `None`.
fn assignment_span(cond: &Expr) -> Option<leek_span::Span> {
    match &cond.kind {
        ExprKind::Binary(op, ..) if is_assignment(*op) => Some(cond.span),
        _ => None,
    }
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::ASSIGNMENT_IN_CONDITION,
        span,
        "assignment used as a condition".to_string(),
    )
    .with_note(
        "this assigns and then tests the result — likely a typo for `==`. \
         If the assignment is intentional, move it out of the condition.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(AssignmentInCondition, src)
    }

    #[test]
    fn flags_if_assignment() {
        let d = run("function f() {\n  var x = 0\n  if (x = 5) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_while_assignment() {
        let d = run("function f() {\n  var x = 0\n  while (x = 5) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_equality_condition() {
        let d = run("function f(x) {\n  if (x == 5) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
