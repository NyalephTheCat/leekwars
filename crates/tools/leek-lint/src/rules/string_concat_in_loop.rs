//! L0035 `StringConcatInLoop` (nursery) — flag building a string by
//! repeated concatenation inside a loop:
//!
//! ```leekscript
//! var msg = ""
//! for (var e in enemies) {
//!     msg += getName(e) + ", "   // copies the whole string every pass
//! }
//! ```
//!
//! Each `+=` copies everything accumulated so far, so the loop costs
//! O(n²) ops as the string grows. Collecting the pieces in an array
//! and calling `join(parts, ", ")` once is linear — a classic
//! ops-budget teaching moment.
//!
//! Heuristic: a compound `+=` (or `x = x + …`) inside a loop body
//! whose right side contains a string literal. The HIR is untyped, so
//! purely-variable concats (`msg += part`) aren't recognized — the
//! string literal is the tell.

use std::collections::HashSet;

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Expr, ExprKind, Literal, Stmt};

use super::structural::expr_key;
use super::{for_each_expr, for_each_expr_deep, for_each_stmt};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

#[derive(Default)]
pub struct StringConcatInLoop {
    /// Spans already reported. A nested loop's body is scanned once
    /// from each enclosing loop; only the first report counts.
    reported: HashSet<(u32, u32)>,
}

static META: LintMeta = LintMeta {
    name: "string-concat-in-loop",
    code: codes::STRING_CONCAT_IN_LOOP,
    group: LintGroup::Nursery,
    description: "string built by `+=` in a loop — collect parts and `join` once instead",
};

impl LintPass for StringConcatInLoop {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let body = match s {
            Stmt::While(w) => &w.body,
            Stmt::DoWhile(d) => &d.body,
            Stmt::For(f) => &f.body,
            Stmt::Foreach(fe) => &fe.body,
            _ => return,
        };
        let mut findings = Vec::new();
        for_each_stmt(std::slice::from_ref(body.as_ref()), &mut |st| {
            leek_hir::walk_stmt_child_exprs(st, &mut |e| {
                for_each_expr_deep(e, &mut |e| {
                    if is_string_append(e) && self.reported.insert((e.span.start, e.span.end)) {
                        findings.push(diagnostic(e.span));
                    }
                });
            });
        });
        for d in findings {
            cx.emit(d);
        }
    }
}

/// `x += <…"literal"…>` or `x = x + <…"literal"…>`.
fn is_string_append(e: &Expr) -> bool {
    let ExprKind::Binary(op, lhs, rhs) = &e.kind else {
        return false;
    };
    match op {
        BinaryOp::AddAssign => contains_string_literal(rhs),
        BinaryOp::Assign => {
            // `x = x + …` (the accumulator reappears on the right).
            let ExprKind::Binary(BinaryOp::Add, a, b) = &rhs.kind else {
                return false;
            };
            let target = expr_key(lhs);
            (expr_key(a) == target || expr_key(b) == target) && contains_string_literal(rhs)
        }
        _ => false,
    }
}

fn contains_string_literal(e: &Expr) -> bool {
    let mut found = false;
    for_each_expr(e, &mut |e| {
        if matches!(&e.kind, ExprKind::Literal(Literal::String(_))) {
            found = true;
        }
    });
    found
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::new(
        codes::STRING_CONCAT_IN_LOOP,
        leek_diagnostics::Severity::Hint,
        span,
        "string concatenation inside a loop".to_string(),
    )
    .with_note(
        "each `+=` copies the whole string accumulated so far, so the loop costs O(n²) ops — push the pieces into an array and `join(parts, …)` once after the loop",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(StringConcatInLoop::default(), src)
    }

    #[test]
    fn flags_compound_append_in_foreach() {
        let d = run(
            "function f(items) {\n  var msg = \"\"\n  for (var e in items) {\n    msg += e + \", \"\n  }\n  return msg\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_self_assign_append_in_for() {
        let d = run(
            "function f(n) {\n  var s = \"\"\n  for (var i = 0; i < n; i++) {\n    s = s + \"x\"\n  }\n  return s\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn one_finding_in_nested_loops() {
        let d = run(
            "function f(n) {\n  var s = \"\"\n  for (var i = 0; i < n; i++) {\n    for (var j = 0; j < n; j++) {\n      s += \"x\"\n    }\n  }\n  return s\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_numeric_accumulation() {
        let d = run(
            "function f(items) {\n  var total = 0\n  for (var e in items) {\n    total += e\n  }\n  return total\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_concat_outside_loop() {
        let d = run("function f(a) {\n  var s = \"\"\n  s += a + \"!\"\n  return s\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
