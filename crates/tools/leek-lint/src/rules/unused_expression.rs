//! L0022 `UnusedExpression` — flag an expression statement whose value
//! is computed and then thrown away, with no side effect to justify it.
//!
//! ```leekscript
//! x == 5     // typo: meant `x = 5`? the comparison result is discarded
//! a + b      // computed, then dropped
//! ```
//!
//! These are almost always typos — a `==` that should be `=`, or a line
//! left dangling after an edit. A statement that *does* something (a
//! call, `new`, `++`/`--`, or any assignment) is fine and never flagged.
//!
//! ## Implicit return
//!
//! Leekscript implicitly returns the trailing expression of a block
//! (`var x = 5\nx + 1` evaluates to `6`), so the **last** statement of
//! every block is exempt — its value may be the block's result. Only
//! non-terminal expression statements are reported.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Expr, ExprKind, Stmt};

use super::structural::has_side_effect;
use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct UnusedExpression;

static META: LintMeta = LintMeta {
    name: "unused-expression",
    code: codes::UNUSED_EXPRESSION,
    group: LintGroup::Suspicious,
    description: "expression statement whose value is discarded without side effects",
};

impl LintPass for UnusedExpression {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_block(&mut self, cx: &mut LintCx<'_, '_>, stmts: &[Stmt]) {
        // Exempt the trailing statement: its value may be the block's
        // implicit return. Everything before it is in statement
        // position, so a discarded value there is dead.
        let Some((_, init)) = stmts.split_last() else {
            return;
        };
        for stmt in init {
            if let Stmt::Expr(e) = stmt
                && !has_side_effect(e)
            {
                cx.emit(diagnostic(e));
            }
        }
    }
}

fn diagnostic(e: &Expr) -> Diagnostic {
    let mut d = Diagnostic::warning(
        codes::UNUSED_EXPRESSION,
        e.span,
        "this expression's value is computed but never used".to_string(),
    );
    // The `==`-for-`=` typo is common enough to call out specifically.
    if matches!(&e.kind, ExprKind::Binary(leek_hir::BinaryOp::Eq, ..)) {
        d = d.with_note("did you mean `=` (assignment) instead of `==` (comparison)?");
    } else {
        d = d.with_note(
            "remove the statement, or use its result — bind it with `var`, return it, or pass it to a call",
        );
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(UnusedExpression, src)
    }

    #[test]
    fn flags_dangling_comparison() {
        let d = run("function f(x) {\n  x == 5\n  return x\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("=="), "{d:?}");
    }

    #[test]
    fn flags_dangling_arithmetic() {
        let d = run("function f(a, b) {\n  a + b\n  return a\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_trailing_expression() {
        // Trailing expr is an implicit return — must not be flagged.
        let d = run("function f(a, b) {\n  a + b\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_call_statement() {
        let d = run("function f() {\n  debug(\"hi\")\n  return 1\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_assignment() {
        let d = run("function f(x) {\n  x = 5\n  return x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_increment() {
        let d = run("function f(x) {\n  x++\n  return x\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
