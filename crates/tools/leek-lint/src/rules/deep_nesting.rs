//! L0027 `DeepNesting` (pedantic) — flag control flow nested more than
//! [`MAX_DEPTH`] levels deep.
//!
//! ```leekscript
//! for (...) {
//!   if (...) {
//!     for (...) {
//!       if (...) {
//!         while (...) { ... }   // ← five levels in
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! Each level multiplies the state a reader must hold; early returns
//! (`if (!ok) return`) and extracted helpers flatten the pyramid.
//! Inspired by clippy's `excessive_nesting`.
//!
//! Only the statement that *opens* the offending level is reported —
//! anything nested deeper is inside that statement and would be noise.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::Stmt;

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct DeepNesting;

/// A control-flow statement sitting at this depth (so its body runs
/// one deeper) triggers the lint.
const MAX_DEPTH: usize = 4;

static META: LintMeta = LintMeta {
    name: "deep-nesting",
    code: codes::DEEP_NESTING,
    group: LintGroup::Pedantic,
    description: "control flow nested more than 4 levels deep — flatten with early returns or helpers",
};

impl LintPass for DeepNesting {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        // Fire exactly at the threshold: the statement opening level
        // MAX_DEPTH + 1. Deeper statements live inside this one and
        // stay silent, so each pyramid yields one finding.
        if cx.depth != MAX_DEPTH || !opens_a_level(s) {
            return;
        }
        cx.emit(
            Diagnostic::new(
                codes::DEEP_NESTING,
                leek_diagnostics::Severity::Hint,
                s.span(),
                format!("this nests control flow more than {MAX_DEPTH} levels deep"),
            )
            .with_note(
                "deep nesting is hard to follow — invert conditions into early `return`/`continue`, or extract the inner levels into a function",
            ),
        );
    }
}

fn opens_a_level(s: &Stmt) -> bool {
    matches!(
        s,
        Stmt::If(_)
            | Stmt::While(_)
            | Stmt::DoWhile(_)
            | Stmt::For(_)
            | Stmt::Foreach(_)
            | Stmt::Switch(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DeepNesting, src)
    }

    #[test]
    fn flags_five_levels() {
        let d = run(
            "function f(x) {\n  if (x) {\n    if (x) {\n      if (x) {\n        if (x) {\n          if (x) { return 1 }\n        }\n      }\n    }\n  }\n  return 0\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn one_finding_even_when_deeper() {
        // Seven levels: still a single finding, on the level-5 opener.
        let d = run(
            "function f(x) {\n  if (x) {\n    if (x) {\n      if (x) {\n        if (x) {\n          if (x) {\n            if (x) {\n              if (x) { return 1 }\n            }\n          }\n        }\n      }\n    }\n  }\n  return 0\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_four_levels() {
        let d = run(
            "function f(x) {\n  if (x) {\n    if (x) {\n      if (x) {\n        if (x) { return 1 }\n      }\n    }\n  }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn else_if_chain_does_not_count_as_nesting() {
        let d = run(
            "function f(x) {\n  if (x == 1) { return 1 }\n  else if (x == 2) { return 2 }\n  else if (x == 3) { return 3 }\n  else if (x == 4) { return 4 }\n  else if (x == 5) { return 5 }\n  else if (x == 6) { return 6 }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
