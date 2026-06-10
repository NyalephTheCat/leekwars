//! L0004 `EmptyBlock` — flag `if`, `else`, `while`, `for`,
//! `foreach`, and `do-while` whose body is an empty block.
//!
//! Severity is Hint: the construct is sometimes intentional (e.g.
//! polling spin-wait) but more often a leftover stub.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::Stmt;

use crate::LintGroup;
use crate::pass::{LintCx, LintMeta, LintPass};

pub struct EmptyBlock;

static META: LintMeta = LintMeta {
    name: "empty-block",
    code: codes::EMPTY_BLOCK,
    group: LintGroup::Style,
    description: "control-flow construct with an empty body",
};

impl LintPass for EmptyBlock {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_stmt(&mut self, cx: &mut LintCx<'_, '_>, s: &Stmt) {
        match s {
            Stmt::If(i) => {
                if let Some(span) = empty_block_span(&i.then_branch) {
                    cx.emit(empty(span, "if"));
                }
                if let Some(else_b) = &i.else_branch
                    && let Some(span) = empty_block_span(else_b)
                {
                    cx.emit(empty(span, "else"));
                }
            }
            Stmt::While(w) => {
                if let Some(span) = empty_block_span(&w.body) {
                    cx.emit(empty(span, "while"));
                }
            }
            Stmt::DoWhile(dw) => {
                if let Some(span) = empty_block_span(&dw.body) {
                    cx.emit(empty(span, "do-while"));
                }
            }
            Stmt::For(fr) => {
                if let Some(span) = empty_block_span(&fr.body) {
                    cx.emit(empty(span, "for"));
                }
            }
            Stmt::Foreach(fe) => {
                if let Some(span) = empty_block_span(&fe.body) {
                    cx.emit(empty(span, "foreach"));
                }
            }
            _ => {}
        }
    }
}

fn empty_block_span(s: &Stmt) -> Option<leek_span::Span> {
    match s {
        Stmt::Block(b) if b.stmts.is_empty() => Some(b.span),
        _ => None,
    }
}

fn empty(span: leek_span::Span, what: &str) -> Diagnostic {
    use leek_diagnostics::Severity;
    Diagnostic::new(
        codes::EMPTY_BLOCK,
        Severity::Hint,
        span,
        format!("empty {what} body"),
    )
    .with_note("if this is intentional, leave a comment explaining why")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(EmptyBlock, src)
    }

    #[test]
    fn flags_empty_if_body() {
        let d = run("function f(x) {\n  if (x > 0) {}\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn flags_empty_else() {
        let d = run("function f(x) {\n  if (x > 0) { return 1 } else {}\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("else"), "{d:?}");
    }

    #[test]
    fn flags_empty_while() {
        let d = run("function f(x) {\n  while (x > 0) {}\n  return 0\n}\n");
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_nonempty_bodies() {
        let d = run("function f(x) {\n  if (x > 0) { return 1 }\n  return 0\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
