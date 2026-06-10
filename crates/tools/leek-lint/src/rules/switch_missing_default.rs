//! L0029 `SwitchMissingDefault` (pedantic) — flag a `switch` with no
//! `default` arm. When none of the `case`s match, execution silently
//! falls through the whole statement; an explicit `default` documents
//! whether that's intended (even if its body is just a comment or a
//! debug log). Inspired by the classic C/Java style checks.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::Stmt;

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct SwitchMissingDefault;

static META: LintMeta = LintMeta {
    name: "switch-missing-default",
    code: codes::SWITCH_MISSING_DEFAULT,
    group: LintGroup::Pedantic,
    description: "`switch` without a `default` arm — unmatched values silently do nothing",
};

impl LintPass for SwitchMissingDefault {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        let Stmt::Switch(sw) = s else { return };
        if sw.arms.iter().any(|arm| arm.case.is_none()) {
            return;
        }
        cx.emit(
            Diagnostic::new(
                codes::SWITCH_MISSING_DEFAULT,
                leek_diagnostics::Severity::Hint,
                sw.span,
                "this `switch` has no `default` arm".to_string(),
            )
            .with_note(
                "a value matching no `case` silently skips the whole `switch` — add `default:` to handle (or deliberately ignore) the leftover values",
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(SwitchMissingDefault, src)
    }

    #[test]
    fn flags_switch_without_default() {
        let d = run(
            "function f(x) {\n  switch (x) {\n    case 1: return 1\n    case 2: return 2\n  }\n  return 0\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_switch_with_default() {
        let d = run(
            "function f(x) {\n  switch (x) {\n    case 1: return 1\n    default: return 2\n  }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
