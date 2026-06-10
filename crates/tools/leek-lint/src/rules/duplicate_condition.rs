//! L0017 `DuplicateCondition` — flag an `if … else if` chain that tests
//! the same (side-effect-free) condition twice. A later arm with a
//! condition identical to an earlier one is unreachable.

use std::collections::HashSet;

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::Stmt;

use super::structural::{expr_key, has_side_effect};
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

#[derive(Default)]
pub struct DuplicateCondition {
    /// Spans of `else if` statements already consumed as part of a
    /// chain. The driver visits every `If` — including the inner ones
    /// of an `else if` chain — but a chain must be checked exactly
    /// once, from its head.
    chain_links: HashSet<(u32, u32)>,
}

static META: LintMeta = LintMeta {
    name: "duplicate-condition",
    code: codes::DUPLICATE_CONDITION,
    group: LintGroup::Correctness,
    description: "`else if` testing a condition an earlier arm already tested — never runs",
};

impl LintPass for DuplicateCondition {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let Stmt::If(head) = s else { return };
        if self.chain_links.contains(&(head.span.start, head.span.end)) {
            return; // interior of a chain we already walked
        }
        let mut seen: Vec<String> = Vec::new();
        let mut cur = head;
        loop {
            // Only deterministic conditions can be "definitely"
            // duplicated; a side-effecting condition might legitimately
            // differ on re-eval.
            if !has_side_effect(&cur.cond) {
                let key = expr_key(&cur.cond);
                if seen.iter().any(|k| k == &key) {
                    cx.emit(diagnostic(cur.cond.span));
                } else {
                    seen.push(key);
                }
            }
            match cur.else_branch.as_deref() {
                Some(Stmt::If(next)) => {
                    self.chain_links.insert((next.span.start, next.span.end));
                    cur = next;
                }
                _ => break,
            }
        }
    }
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::DUPLICATE_CONDITION,
        span,
        "this condition is identical to an earlier branch in the chain".to_string(),
    )
    .with_note("the earlier branch always wins, so this arm can never run — did you mean a different condition?")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DuplicateCondition::default(), src)
    }

    #[test]
    fn flags_repeated_condition() {
        let d = run(
            "function f(x) {\n  if (x > 0) { return 1 } else if (x > 0) { return 2 }\n  return 0\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_distinct_conditions() {
        let d = run(
            "function f(x) {\n  if (x > 0) { return 1 } else if (x < 0) { return 2 }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn flags_third_arm_duplicate() {
        let d = run(
            "function f(x) {\n  if (x == 1) { return 1 } else if (x == 2) { return 2 } else if (x == 1) { return 3 }\n  return 0\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }
}
