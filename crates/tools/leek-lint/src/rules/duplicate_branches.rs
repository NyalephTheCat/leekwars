//! L0009 `DuplicateBranches` — flag `if (c) X else X` where the `then`
//! and `else` branches are structurally identical.
//!
//! When both arms do the same thing the condition is pointless — almost
//! always a copy-paste bug where one arm was meant to differ. Comparison
//! is span-insensitive and binding-aware (see [`super::structural`]); a
//! branch that declares its own locals won't collide, so we never report
//! a false duplicate.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::Stmt;

use super::structural::stmt_key;
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct DuplicateBranches;

static META: LintMeta = LintMeta {
    name: "duplicate-branches",
    code: codes::DUPLICATE_BRANCHES,
    group: LintGroup::Suspicious,
    description: "`if` whose then- and else-branches are identical — the condition has no effect",
};

impl LintPass for DuplicateBranches {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        if let Stmt::If(i) = s
            && let Some(else_b) = &i.else_branch
            // An `else if` chain has an `If` else-branch; skip those (the
            // inner `if` is visited separately) so we only compare a real
            // then/else pair.
            && !matches!(&**else_b, Stmt::If(_))
            && !is_empty_branch(&i.then_branch)
            && stmt_key(&i.then_branch) == stmt_key(else_b)
        {
            cx.emit(diagnostic(i.span));
        }
    }
}

/// An empty `{ }` branch is the `EmptyBlock` lint's territory, not ours.
fn is_empty_branch(s: &Stmt) -> bool {
    matches!(s, Stmt::Block(b) if b.stmts.is_empty())
}

fn diagnostic(span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::DUPLICATE_BRANCHES,
        span,
        "both branches of this `if` are identical".to_string(),
    )
    .with_note(
        "the condition has no effect — e.g. `if (c) { f() } else { f() }` is just `f()`. \
         Did one branch mean to do something different?",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DuplicateBranches, src)
    }

    #[test]
    fn flags_identical_branches() {
        let d = run("function f(x) {\n  if (x) { return 1 } else { return 1 }\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_differing_branches() {
        let d = run("function f(x) {\n  if (x) { return 1 } else { return 2 }\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_branch_with_local_decl() {
        // Each branch declares its own `y` (distinct DefIds) → not a
        // reported duplicate (conservative).
        let d = run("function f(x) {\n  if (x) { var y = 1 } else { var y = 1 }\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_else_if_chain() {
        let d = run("function f(x, z) {\n  if (x) { return 1 } else if (z) { return 2 }\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
