//! L0028 `CollapsibleIf` (pedantic) — flag an `if` whose entire body is
//! another `if` (neither having an `else`): the two conditions can be
//! combined with `&&`, saving a level of nesting.
//!
//! ```leekscript
//! if (a) {
//!   if (b) { doIt() }     // → if (a && b) { doIt() }
//! }
//! ```
//!
//! Inspired by clippy's `collapsible_if`. No autofix: joining the
//! conditions can need parentheses (`a || b` and `c` join as
//! `(a || b) && c`) which the lint layer doesn't reprint.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{IfStmt, Stmt};

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct CollapsibleIf;

static META: LintMeta = LintMeta {
    name: "collapsible-if",
    code: codes::COLLAPSIBLE_IF,
    group: LintGroup::Pedantic,
    description: "`if` containing only another `if` — combine the conditions with `&&`",
};

impl LintPass for CollapsibleIf {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let Stmt::If(outer) = s else { return };
        if outer.else_branch.is_some() {
            return;
        }
        let Some(inner) = lone_inner_if(&outer.then_branch) else {
            return;
        };
        if inner.else_branch.is_some() {
            return;
        }
        cx.emit(
            Diagnostic::new(
                codes::COLLAPSIBLE_IF,
                leek_diagnostics::Severity::Hint,
                outer.span,
                "this `if` can be collapsed into its parent".to_string(),
            )
            .with_label(inner.cond.span, "join this condition with `&&`")
            .with_note(
                "`if (a) { if (b) { … } }` is `if (a && b) { … }` — one level less to indent (parenthesize `a`/`b` if they contain `||`)",
            ),
        );
    }
}

/// The inner `if` when `then` is exactly one `if` statement —
/// either bare or as the sole statement of a block.
fn lone_inner_if(then: &Stmt) -> Option<&IfStmt> {
    match then {
        Stmt::If(i) => Some(i),
        Stmt::Block(b) => match &b.stmts[..] {
            [Stmt::If(i)] => Some(i),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(CollapsibleIf, src)
    }

    #[test]
    fn flags_nested_lone_if() {
        let d =
            run("function f(a, b) {\n  if (a) {\n    if (b) { return 1 }\n  }\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_when_outer_has_more_statements() {
        let d = run(
            "function f(a, b) {\n  if (a) {\n    debug(\"hi\")\n    if (b) { return 1 }\n  }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_when_inner_has_else() {
        let d = run(
            "function f(a, b) {\n  if (a) {\n    if (b) { return 1 } else { return 2 }\n  }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_when_outer_has_else() {
        let d = run(
            "function f(a, b) {\n  if (a) {\n    if (b) { return 1 }\n  } else { return 2 }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
