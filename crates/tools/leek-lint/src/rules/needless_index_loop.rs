//! L0032 `NeedlessIndexLoop` (pedantic) — flag a counting `for` loop
//! whose counter is only ever used to index one array:
//!
//! ```leekscript
//! for (var i = 0; i < count(enemies); i++) {
//!     attack(enemies[i])          // i exists only to read enemies[i]
//! }
//! // → for (var enemy in enemies) { attack(enemy) }
//! ```
//!
//! `foreach` says what the loop is *about* (the elements), can't go
//! out of bounds, and skips the per-iteration `count()`/index ops.
//! Inspired by clippy's `needless_range_loop`.
//!
//! Conservative on purpose: the counter must start at 0 and step by
//! `++`/`+= 1`, every use must read `arr[i]` for one fixed
//! side-effect-free `arr`, and any *write* through `arr[i]` bails (a
//! by-value `foreach` binding wouldn't update the array).

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, DefId, Expr, ExprKind, Literal, NameRef, PostfixOp, Stmt, UnaryOp};

use super::structural::{expr_key, has_side_effect};
use super::{for_each_expr_deep, for_each_stmt};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct NeedlessIndexLoop;

static META: LintMeta = LintMeta {
    name: "needless-index-loop",
    code: codes::NEEDLESS_INDEX_LOOP,
    group: LintGroup::Pedantic,
    description: "counting loop whose index only reads `arr[i]` — use `for (var x in arr)`",
};

impl LintPass for NeedlessIndexLoop {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let Stmt::For(fr) = s else { return };
        // Header: `var i = 0; i < …; i++` (or `++i` / `i += 1`).
        let Some(counter) = counter_from_zero(fr.init.as_deref()) else {
            return;
        };
        if !cond_is_upper_bound(fr.cond.as_ref(), counter) {
            return;
        }
        if !step_is_increment(fr.step.as_ref(), counter) {
            return;
        }

        // Body: every use of `i` must be `arr[i]` for one fixed `arr`,
        // and never on the receiving end of a write.
        let mut uses = CounterUses::default();
        scan_body(&fr.body, counter, &mut uses);
        if uses.written_through_index || uses.indexed_arrays.is_empty() {
            return;
        }
        if uses.bare_refs != uses.indexed_count() {
            return; // `i` is also used as a value (e.g. logged) — keep the index
        }
        let [(array_key, _)] = &uses.indexed_arrays[..] else {
            return; // indexes more than one array
        };
        let _ = array_key;

        cx.emit(
            Diagnostic::new(
                codes::NEEDLESS_INDEX_LOOP,
                leek_diagnostics::Severity::Hint,
                fr.span,
                "this loop's counter is only used to index one array".to_string(),
            )
            .with_note(
                "iterate the elements directly: `for (var x in arr) { … }` — no bounds to get wrong, and it skips the per-iteration index ops",
            ),
        );
    }
}

/// The counter's `DefId` when `init` is `var i = 0`.
fn counter_from_zero(init: Option<&Stmt>) -> Option<DefId> {
    let Some(Stmt::VarDecl(v)) = init else {
        return None;
    };
    match v.init.as_ref().map(|e| &e.kind) {
        Some(ExprKind::Literal(Literal::Int(0))) => Some(v.def),
        _ => None,
    }
}

/// `i < <bound>` (strict, counter on the left).
fn cond_is_upper_bound(cond: Option<&Expr>, counter: DefId) -> bool {
    let Some(cond) = cond else { return false };
    matches!(
        &cond.kind,
        ExprKind::Binary(BinaryOp::Lt, lhs, _) if is_counter(lhs, counter)
    )
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

#[derive(Default)]
struct CounterUses {
    /// Every reference to the counter (including those inside `arr[i]`).
    bare_refs: usize,
    /// `(expr_key(arr), uses)` per distinct indexed array.
    indexed_arrays: Vec<(String, usize)>,
    /// `arr[i] = …`, `arr[i]++`, … — rewriting breaks under by-value
    /// `foreach`.
    written_through_index: bool,
}

impl CounterUses {
    fn indexed_count(&self) -> usize {
        self.indexed_arrays.iter().map(|(_, n)| n).sum()
    }

    fn record_index(&mut self, key: String) {
        match self.indexed_arrays.iter_mut().find(|(k, _)| *k == key) {
            Some((_, n)) => *n += 1,
            None => self.indexed_arrays.push((key, 1)),
        }
    }
}

fn scan_body(body: &Stmt, counter: DefId, uses: &mut CounterUses) {
    for_each_stmt(std::slice::from_ref(body), &mut |s| {
        leek_hir::walk_stmt_child_exprs(s, &mut |e| {
            for_each_expr_deep(e, &mut |e| scan_expr(e, counter, uses));
        });
    });
}

fn scan_expr(e: &Expr, counter: DefId, uses: &mut CounterUses) {
    match &e.kind {
        ExprKind::Name(NameRef::Local(d)) if *d == counter => uses.bare_refs += 1,
        ExprKind::Index(base, idx) if is_counter(idx, counter) => {
            if has_side_effect(base) {
                // Can't prove it's the same array each iteration.
                uses.written_through_index = true;
            } else {
                uses.record_index(expr_key(base));
            }
        }
        // Writes through `arr[i]`: plain/compound assignment or ++/--.
        ExprKind::Binary(op, lhs, _) if op.is_assignment() && indexes_counter(lhs, counter) => {
            uses.written_through_index = true;
        }
        ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, t)
        | ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec, t)
            if indexes_counter(t, counter) =>
        {
            uses.written_through_index = true;
        }
        _ => {}
    }
}

fn indexes_counter(e: &Expr, counter: DefId) -> bool {
    matches!(&e.kind, ExprKind::Index(_, idx) if is_counter(idx, counter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(NeedlessIndexLoop, src)
    }

    #[test]
    fn flags_pure_index_read_loop() {
        let d = run(
            "function f(arr) {\n  var total = 0\n  for (var i = 0; i < count(arr); i++) {\n    total += arr[i]\n  }\n  return total\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_when_counter_used_as_value() {
        let d = run(
            "function f(arr) {\n  var total = 0\n  for (var i = 0; i < count(arr); i++) {\n    total += arr[i] * i\n  }\n  return total\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_when_writing_through_index() {
        let d = run(
            "function f(arr) {\n  for (var i = 0; i < count(arr); i++) {\n    arr[i] = arr[i] + 1\n  }\n  return arr\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_two_arrays() {
        let d = run(
            "function f(a, b) {\n  var total = 0\n  for (var i = 0; i < count(a); i++) {\n    total += a[i] + b[i]\n  }\n  return total\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_nonzero_start() {
        let d = run(
            "function f(arr) {\n  var total = 0\n  for (var i = 1; i < count(arr); i++) {\n    total += arr[i]\n  }\n  return total\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
