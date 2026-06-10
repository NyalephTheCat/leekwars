//! L0037 `IntervalLoop` (nursery, LeekScript 4+) — flag the classic
//! three-clause counting loop and teach v4's interval iteration:
//!
//! ```leekscript
//! for (var i = 0; i <= 10; i++) { … }   // → for (var i in [0..10]) { … }
//! for (var i = 0; i < n; i++)  { … }    // → for (var i in [0..n[) { … }
//! ```
//!
//! The interval form has no header to get subtly wrong (`<=` vs `<`,
//! `i++` vs `++j` typos) and reads as "every i in this range". Only
//! fires when the loop is a plain count-up: start literal, `<`/`<=`
//! bound, `++` step, and a body that never writes the counter (an
//! interval can't be fast-forwarded mid-flight).
//!
//! Gated on [`crate::LintOptions::version`] ≥ 4 — older scripts don't
//! have intervals.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, DefId, Expr, ExprKind, Literal, NameRef, PostfixOp, Stmt, UnaryOp};

use super::{for_each_expr_deep, for_each_stmt};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct IntervalLoop {
    /// Target language version; the lint is silent below 4.
    pub version: u8,
}

static META: LintMeta = LintMeta {
    name: "interval-loop",
    code: codes::INTERVAL_LOOP,
    group: LintGroup::Nursery,
    description: "C-style counting loop — LeekScript 4 intervals say the same with less ceremony",
};

impl LintPass for IntervalLoop {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        if self.version < 4 {
            return;
        }
        let Stmt::For(fr) = s else { return };
        let Some((counter, name)) = literal_counter(fr.init.as_deref()) else {
            return;
        };
        let Some(inclusive) = upper_bound(fr.cond.as_ref(), counter) else {
            return;
        };
        if !step_is_increment(fr.step.as_ref(), counter) {
            return;
        }
        if body_writes(&fr.body, counter) {
            return;
        }
        let sketch = if inclusive {
            format!("for (var {name} in [start..end])")
        } else {
            format!("for (var {name} in [start..end[)")
        };
        cx.emit(
            Diagnostic::new(
                codes::INTERVAL_LOOP,
                leek_diagnostics::Severity::Hint,
                fr.span,
                "this counting loop can be an interval iteration".to_string(),
            )
            .with_note(format!(
                "LeekScript 4 intervals iterate ranges directly: `{sketch}` — no `++`/bound clause to get wrong{}",
                if inclusive {
                    ""
                } else {
                    " (`[a..b[` excludes the upper bound, matching `<`)"
                }
            )),
        );
    }
}

/// `var i = <int literal>` → the counter's def and name.
fn literal_counter(init: Option<&Stmt>) -> Option<(DefId, &str)> {
    let Some(Stmt::VarDecl(v)) = init else {
        return None;
    };
    match v.init.as_ref().map(|e| &e.kind) {
        Some(ExprKind::Literal(Literal::Int(_))) => Some((v.def, &v.name)),
        _ => None,
    }
}

/// `i < bound` → `Some(false)`, `i <= bound` → `Some(true)`.
fn upper_bound(cond: Option<&Expr>, counter: DefId) -> Option<bool> {
    let cond = cond?;
    match &cond.kind {
        ExprKind::Binary(BinaryOp::Lt, lhs, _) if is_counter(lhs, counter) => Some(false),
        ExprKind::Binary(BinaryOp::Le, lhs, _) if is_counter(lhs, counter) => Some(true),
        _ => None,
    }
}

/// `i++`, `++i`, or `i += 1`.
fn step_is_increment(step: Option<&Expr>, counter: DefId) -> bool {
    let Some(step) = step else { return false };
    match &step.kind {
        ExprKind::Postfix(PostfixOp::PostInc, e) | ExprKind::Unary(UnaryOp::PreInc, e) => {
            is_counter(e, counter)
        }
        ExprKind::Binary(BinaryOp::AddAssign, lhs, rhs) => {
            is_counter(lhs, counter) && matches!(&rhs.kind, ExprKind::Literal(Literal::Int(1)))
        }
        _ => false,
    }
}

fn is_counter(e: &Expr, counter: DefId) -> bool {
    matches!(&e.kind, ExprKind::Name(NameRef::Local(d)) if *d == counter)
}

/// True when the body assigns / increments the counter.
fn body_writes(body: &Stmt, counter: DefId) -> bool {
    let mut writes = false;
    for_each_stmt(std::slice::from_ref(body), &mut |s| {
        leek_hir::walk_stmt_child_exprs(s, &mut |e| {
            for_each_expr_deep(e, &mut |e| {
                let target = match &e.kind {
                    ExprKind::Binary(op, lhs, _) if op.is_assignment() => lhs,
                    ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, t) => t,
                    ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec, t) => t,
                    _ => return,
                };
                if is_counter(target, counter) {
                    writes = true;
                }
            });
        });
    });
    writes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{lint_one, lint_one_v};
    use leek_syntax::Version;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(IntervalLoop { version: 4 }, src)
    }

    #[test]
    fn flags_simple_count_up() {
        let d = run(
            "function f(n) {\n  var t = 0\n  for (var i = 0; i < n; i++) {\n    t += i\n  }\n  return t\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("[a..b["), "{d:?}");
    }

    #[test]
    fn flags_inclusive_bound() {
        let d = run(
            "function f() {\n  var t = 0\n  for (var i = 1; i <= 10; i++) {\n    t += i\n  }\n  return t\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("[start..end]"), "{d:?}");
    }

    #[test]
    fn ignores_when_body_writes_counter() {
        let d = run(
            "function f(n) {\n  var t = 0\n  for (var i = 0; i < n; i++) {\n    if (t > 5) { i += 2 }\n    t += i\n  }\n  return t\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_non_unit_step() {
        let d = run(
            "function f(n) {\n  var t = 0\n  for (var i = 0; i < n; i += 2) {\n    t += i\n  }\n  return t\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn silent_below_v4() {
        let d = lint_one_v(
            IntervalLoop { version: 1 },
            "function f(n) {\n  var t = 0\n  for (var i = 0; i < n; i++) {\n    t += i\n  }\n  return t\n}\n",
            Version::V1,
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
