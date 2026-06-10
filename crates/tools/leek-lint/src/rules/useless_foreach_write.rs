//! L0036 `UselessForeachWrite` (nursery) — flag assignments to a
//! by-value `foreach` binding:
//!
//! ```leekscript
//! for (var x in arr) {
//!     x = x * 2        // modifies the copy — arr is unchanged!
//! }
//! for (var @x in arr) {
//!     x = x * 2        // @ makes x an alias — this rewrites arr
//! }
//! ```
//!
//! A by-value binding is a fresh copy each iteration, so writing to
//! it does nothing visible outside the loop body. Leekscript's `@`
//! reference bindings are the language's own answer — exactly the
//! feature this nursery group exists to teach.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{DefId, Expr, ExprKind, NameRef, PostfixOp, Stmt, UnaryOp};

use super::for_each_expr_deep;
use super::for_each_stmt;
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct UselessForeachWrite;

static META: LintMeta = LintMeta {
    name: "useless-foreach-write",
    code: codes::USELESS_FOREACH_WRITE,
    group: LintGroup::Nursery,
    description: "assignment to a by-value foreach binding — use `var @x` to write through",
};

impl LintPass for UselessForeachWrite {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let Stmt::Foreach(fe) = s else { return };
        if fe.value.is_by_ref {
            return; // `@` binding — writes are the point
        }
        let mut findings = Vec::new();
        for_each_stmt(std::slice::from_ref(fe.body.as_ref()), &mut |st| {
            leek_hir::walk_stmt_child_exprs(st, &mut |e| {
                for_each_expr_deep(e, &mut |e| {
                    if let Some(span) = write_to(e, fe.value.def) {
                        findings.push(diagnostic(&fe.value.name, span));
                    }
                });
            });
        });
        for d in findings {
            cx.emit(d);
        }
    }
}

/// The span of `e` when it writes to binding `def` (assignment or
/// increment/decrement directly on the name — `x[i] = …` is a write
/// *through* the copy and is just as lost, but indexing a copy of an
/// array still aliases the elements in Leekscript, so stay quiet).
fn write_to(e: &Expr, def: DefId) -> Option<leek_span::Span> {
    let target = match &e.kind {
        ExprKind::Binary(op, lhs, _) if op.is_assignment() => lhs,
        ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, t) => t,
        ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec, t) => t,
        _ => return None,
    };
    matches!(&target.kind, ExprKind::Name(NameRef::Local(d)) if *d == def).then_some(e.span)
}

fn diagnostic(name: &str, span: leek_span::Span) -> Diagnostic {
    Diagnostic::new(
        codes::USELESS_FOREACH_WRITE,
        leek_diagnostics::Severity::Hint,
        span,
        format!("writing to `{name}` does not modify the collection"),
    )
    .with_note(format!(
        "`{name}` is a by-value copy that is overwritten on the next iteration — declare it `var @{name}` to make it a reference that writes through to the collection"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(UselessForeachWrite, src)
    }

    #[test]
    fn flags_assignment_to_by_value_binding() {
        let d =
            run("function f(arr) {\n  for (var x in arr) {\n    x = x * 2\n  }\n  return arr\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("@x"), "{d:?}");
    }

    #[test]
    fn flags_increment_of_binding() {
        let d = run("function f(arr) {\n  for (var x in arr) {\n    x++\n  }\n  return arr\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_by_ref_binding() {
        let d = run(
            "function f(arr) {\n  for (var @x in arr) {\n    x = x * 2\n  }\n  return arr\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_read_only_binding() {
        let d = run(
            "function f(arr) {\n  var total = 0\n  for (var x in arr) {\n    total += x\n  }\n  return total\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
