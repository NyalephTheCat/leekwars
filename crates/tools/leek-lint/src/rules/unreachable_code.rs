//! L0005 `UnreachableCode` — flag statements that follow a definite
//! control-flow exit (`return`, `break`, `continue`).
//!
//! ## Detection
//!
//! For every statement sequence (the driver's `check_block` fires for
//! body statements, `{}` block contents, and switch-arm bodies), emit
//! one finding the first time a statement appears after a
//! "terminator". Subsequent siblings in the same block aren't
//! reported again — one diagnostic per unreachable region is
//! friendlier than N.
//!
//! ## Known limitations
//!
//! - **Conditional terminators** (`if (cond) { return … }`) aren't
//!   treated as definite returns yet — the rule only fires on
//!   straight-line `return`/`break`/`continue`. Catching the
//!   `if (a) return else return` shape requires the same
//!   `stmt_definitely_returns` analysis the Java backend uses;
//!   factor that out in a later slice if useful.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::Stmt;
use leek_span::Span;

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct UnreachableCode;

static META: LintMeta = LintMeta {
    name: "unreachable-code",
    code: codes::UNREACHABLE_CODE,
    group: LintGroup::Correctness,
    description: "statement after a `return`/`break`/`continue` — never runs",
};

impl LintPass for UnreachableCode {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_block(&mut self, cx: &mut LintCx<'_, '_>, stmts: &[Stmt]) {
        let mut iter = stmts.iter();
        while let Some(stmt) = iter.next() {
            if is_terminator(stmt) {
                // Everything after this in the same block is
                // unreachable. Emit one diagnostic spanning the first
                // such statement; siblings inherit the same annotation.
                if let Some(next) = iter.next() {
                    cx.emit(diagnostic(next.span(), terminator_kind(stmt)));
                }
                break;
            }
        }
    }
}

fn is_terminator(s: &Stmt) -> bool {
    matches!(s, Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_))
}

fn terminator_kind(s: &Stmt) -> &'static str {
    match s {
        Stmt::Return(_) => "return",
        Stmt::Break(_) => "break",
        Stmt::Continue(_) => "continue",
        _ => "terminator",
    }
}

fn diagnostic(span: Span, kind: &str) -> Diagnostic {
    Diagnostic::warning(
        codes::UNREACHABLE_CODE,
        span,
        format!("unreachable statement: previous `{kind}` already exits this block"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(UnreachableCode, src)
    }

    #[test]
    fn flags_statement_after_return() {
        let diags = run("function f() { return 1\nvar dead = 2 }\n");
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert!(diags[0].message.contains("return"));
    }

    #[test]
    fn flags_statement_after_break_in_loop() {
        let diags = run("while (true) { break\nvar dead = 2 }\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("break"));
    }

    #[test]
    fn empty_block_no_findings() {
        let diags = run("function f() { return 1 }\n");
        assert!(diags.is_empty(), "got {diags:?}");
    }

    #[test]
    fn one_finding_per_region_not_per_stmt() {
        let diags = run("function f() { return 1\nvar a = 1\nvar b = 2\nvar c = 3 }\n");
        assert_eq!(diags.len(), 1, "got {diags:?}");
    }
}
