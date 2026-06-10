//! L0039 `ManualRangeCheck` (nursery, LeekScript 4+) — flag the
//! two-comparison range test and teach interval membership:
//!
//! ```leekscript
//! if (0 <= x && x < 10) { … }   // → if (x in [0..10[) { … }
//! if (x < 0 || x > 10) { … }    // → if (x not in [0..10]) { … }
//! ```
//!
//! `x in [a..b]` says "x is in this range" in one operator, and the
//! bracket carries the inclusivity (`[a..b[` excludes the upper
//! bound) — no `<` vs `<=` typo surface.
//!
//! Detection: an `&&` whose two operands are ordering comparisons
//! that bound the *same* side-effect-free expression from below and
//! above (an `||` of the complements is the negated form). All four
//! operands must be side-effect-free because `&&`/`||` short-circuit
//! the second comparison while an interval evaluates both bounds.
//!
//! Gated on [`crate::LintOptions::version`] ≥ 4 — older scripts don't
//! have intervals.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Expr, ExprKind};

use super::structural::{expr_key, has_side_effect};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct ManualRangeCheck {
    /// Target language version; the lint is silent below 4.
    pub version: u8,
}

static META: LintMeta = LintMeta {
    name: "manual-range-check",
    code: codes::MANUAL_RANGE_CHECK,
    group: LintGroup::Nursery,
    description: "two comparisons testing a range — LeekScript 4 intervals say `x in [a..b]`",
};

impl LintPass for ManualRangeCheck {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_expr(&mut self, cx: &mut LintCx<'_, '_>, e: &Expr) {
        if self.version < 4 {
            return;
        }
        let ExprKind::Binary(op, lhs, rhs) = &e.kind else {
            return;
        };
        // `&&` joins the bounds directly; `||` joins their
        // complements (`x < lo || x > hi` = "outside [lo..hi]").
        let negated = match op {
            BinaryOp::And => false,
            BinaryOp::Or => true,
            _ => return,
        };
        let Some(a) = comparison_bounds(lhs, negated) else {
            return;
        };
        let Some(b) = comparison_bounds(rhs, negated) else {
            return;
        };
        // A subject bounded from below by one comparison and from
        // above by the other is a range test on that subject.
        let Some((lower, upper)) = pair_bounds(&a, &b).or_else(|| pair_bounds(&b, &a)) else {
            return;
        };
        let sketch = format!(
            "x {}in {}low..high{}",
            if negated { "not " } else { "" },
            if lower { '[' } else { ']' },
            if upper { ']' } else { '[' },
        );
        cx.emit(
            Diagnostic::new(
                codes::MANUAL_RANGE_CHECK,
                leek_diagnostics::Severity::Hint,
                e.span,
                "this pair of comparisons tests a range".to_string(),
            )
            .with_note(format!(
                "LeekScript 4 intervals test ranges in one operator: `{sketch}` — the brackets carry the inclusivity (`[` includes the bound, `]`/`[` on the other side excludes it)"
            )),
        );
    }
}

/// Which way a comparison bounds a subject.
#[derive(Clone, Copy)]
enum Bound {
    /// Subject is bounded from below; `true` = bound included.
    Lower(bool),
    /// Subject is bounded from above; `true` = bound included.
    Upper(bool),
}

/// The two readings of an ordering comparison, as
/// `(subject_key, bound_on_subject)` pairs — `lo <= x` bounds `x`
/// from below *and* bounds `lo` from above. `None` unless the expr
/// is an ordering comparison with side-effect-free operands.
/// With `negated`, returns the bounds of the comparison's complement
/// (for the `||`-of-exclusions form).
fn comparison_bounds(e: &Expr, negated: bool) -> Option<[(String, Bound); 2]> {
    let ExprKind::Binary(op, l, r) = &e.kind else {
        return None;
    };
    let op = if negated { complement(*op)? } else { *op };
    if has_side_effect(l) || has_side_effect(r) {
        return None;
    }
    let (lk, rk) = (expr_key(l), expr_key(r));
    Some(match op {
        // `l < r`: l is below r, r is above l.
        BinaryOp::Lt => [(lk, Bound::Upper(false)), (rk, Bound::Lower(false))],
        BinaryOp::Le => [(lk, Bound::Upper(true)), (rk, Bound::Lower(true))],
        BinaryOp::Gt => [(lk, Bound::Lower(false)), (rk, Bound::Upper(false))],
        BinaryOp::Ge => [(lk, Bound::Lower(true)), (rk, Bound::Upper(true))],
        _ => return None,
    })
}

/// `!(l < r)` is `l >= r` and so on.
fn complement(op: BinaryOp) -> Option<BinaryOp> {
    Some(match op {
        BinaryOp::Lt => BinaryOp::Ge,
        BinaryOp::Le => BinaryOp::Gt,
        BinaryOp::Gt => BinaryOp::Le,
        BinaryOp::Ge => BinaryOp::Lt,
        _ => return None,
    })
}

/// `Some((lower_inclusive, upper_inclusive))` when some subject gets
/// a lower bound from `a` and an upper bound from `b`.
fn pair_bounds(a: &[(String, Bound); 2], b: &[(String, Bound); 2]) -> Option<(bool, bool)> {
    for (ka, ba) in a {
        for (kb, bb) in b {
            if ka != kb {
                continue;
            }
            if let (Bound::Lower(lo), Bound::Upper(hi)) = (ba, bb) {
                return Some((*lo, *hi));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{lint_one, lint_one_v};
    use leek_syntax::Version;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(ManualRangeCheck { version: 4 }, src)
    }

    #[test]
    fn flags_half_open_range() {
        let d = run("function f(x) {\n  if (0 <= x && x < 10) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("[low..high["), "{d:?}");
    }

    #[test]
    fn flags_closed_range_reversed_operands() {
        // `x >= lo && hi >= x` — both subjects on different sides.
        let d = run("function f(x) {\n  if (x >= 1 && 10 >= x) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("[low..high]"), "{d:?}");
    }

    #[test]
    fn flags_negated_range_via_or() {
        let d = run("function f(x) {\n  if (x < 0 || x > 10) { return 1 }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("not in [low..high]"), "{d:?}");
    }

    #[test]
    fn ignores_unrelated_comparisons() {
        let d =
            run("function f(a, b, c, d) {\n  if (a < b && c < d) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_two_lower_bounds() {
        let d = run("function f(x) {\n  if (x > 0 && x > 10) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_side_effecting_bound() {
        let d = run(
            "function f(x) {\n  var i = 0\n  if (i++ <= x && x < 10) { return 1 }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn silent_below_v4() {
        let d = lint_one_v(
            ManualRangeCheck { version: 1 },
            "function f(x) {\n  if (0 <= x && x < 10) { return 1 }\n  return 0\n}\n",
            Version::V1,
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
