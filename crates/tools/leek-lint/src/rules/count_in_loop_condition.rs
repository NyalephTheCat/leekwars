//! L0034 `CountInLoopCondition` (nursery) — flag `count(arr)` (and
//! other size builtins) inside a loop condition:
//!
//! ```leekscript
//! for (var i = 0; i < count(items); i++) { … }
//! ```
//!
//! The condition re-runs **every iteration**, so the builtin's op
//! cost is paid `n` times. Hoisting it into a variable before the
//! loop pays it once — an easy ops-budget win, and the kind of habit
//! this nursery group exists to teach.
//!
//! Conservative: stays quiet when the loop body *calls anything with
//! the counted collection* (it might grow or shrink it, making the
//! per-iteration re-count load-bearing).

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Callee, Expr, ExprKind, NameRef, Stmt};

use super::structural::{expr_key, has_side_effect};
use super::{for_each_expr, for_each_expr_deep, for_each_stmt};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct CountInLoopCondition;

/// Size/length builtins that are pure reads worth hoisting.
const SIZE_BUILTINS: &[&str] = &["count", "mapSize", "setSize", "length"];

static META: LintMeta = LintMeta {
    name: "count-in-loop-condition",
    code: codes::COUNT_IN_LOOP_CONDITION,
    group: LintGroup::Nursery,
    description: "`count(...)` re-evaluated by every loop iteration — hoist it to save ops",
};

impl LintPass for CountInLoopCondition {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let (cond, body) = match s {
            Stmt::While(w) => (&w.cond, &w.body),
            Stmt::DoWhile(d) => (&d.cond, &d.body),
            Stmt::For(f) => match &f.cond {
                Some(c) => (c, &f.body),
                None => return,
            },
            _ => return,
        };
        let mut findings = Vec::new();
        for_each_expr(cond, &mut |e| {
            if let Some((builtin, arg)) = size_call(e)
                && !body_touches(body, arg)
            {
                findings.push(diagnostic(builtin, e.span));
            }
        });
        for d in findings {
            cx.emit(d);
        }
    }
}

/// `Some((builtin, counted_arg))` when `e` is `count(x)` & co with a
/// side-effect-free argument.
fn size_call(e: &Expr) -> Option<(&str, &Expr)> {
    let ExprKind::Call(call) = &e.kind else {
        return None;
    };
    let Callee::Function(NameRef::Builtin(name)) = &call.callee else {
        return None;
    };
    if !SIZE_BUILTINS.contains(&name.as_str()) {
        return None;
    }
    match &call.args[..] {
        [arg] if !has_side_effect(arg) => Some((name, arg)),
        _ => None,
    }
}

/// True when the loop body mentions the counted collection inside a
/// call or writes to it — re-counting might then be intentional.
fn body_touches(body: &Stmt, counted: &Expr) -> bool {
    let key = expr_key(counted);
    let mut touched = false;
    for_each_stmt(std::slice::from_ref(body), &mut |s| {
        leek_hir::walk_stmt_child_exprs(s, &mut |e| {
            for_each_expr_deep(e, &mut |e| match &e.kind {
                // Passed to any call (e.g. `push(items, x)`).
                ExprKind::Call(c) => {
                    if c.args.iter().any(|a| expr_key(a) == key) {
                        touched = true;
                    }
                }
                // Reassigned wholesale (`items = …`).
                ExprKind::Binary(op, lhs, _) if op.is_assignment() && expr_key(lhs) == key => {
                    touched = true;
                }
                _ => {}
            });
        });
    });
    touched
}

fn diagnostic(builtin: &str, span: leek_span::Span) -> Diagnostic {
    Diagnostic::new(
        codes::COUNT_IN_LOOP_CONDITION,
        leek_diagnostics::Severity::Hint,
        span,
        format!("`{builtin}(...)` runs again on every iteration of this loop"),
    )
    .with_note(format!(
        "the condition is re-evaluated each time around, so this `{builtin}` costs ops every iteration — hoist it: `var n = {builtin}(...)` before the loop, then compare against `n`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(CountInLoopCondition, src)
    }

    #[test]
    fn flags_count_in_for_condition() {
        let d = run(
            "function f(items) {\n  var total = 0\n  for (var i = 0; i < count(items); i++) {\n    total += items[i]\n  }\n  return total\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_count_in_while_condition() {
        let d = run(
            "function f(items) {\n  var i = 0\n  while (i < count(items)) {\n    i++\n  }\n  return i\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_hoisted_count() {
        let d = run(
            "function f(items) {\n  var n = count(items)\n  var total = 0\n  for (var i = 0; i < n; i++) {\n    total += items[i]\n  }\n  return total\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_when_body_mutates_collection() {
        // `pop(items)` shrinks the collection — the re-count is the point.
        let d = run(
            "function f(items) {\n  var i = 0\n  while (i < count(items)) {\n    pop(items)\n  }\n  return i\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
