//! L0024 `UnnecessaryElse` — flag an `else` that follows a `then` branch
//! which always exits (ends in `return`, `break`, or `continue`). The
//! `else` adds a needless level of nesting: its body can be dedented to
//! follow the `if` directly.
//!
//! ```leekscript
//! if (x < 0) {
//!   return -1
//! } else {          // unnecessary — the `then` branch already returned
//!   doStuff()
//! }
//! ```
//!
//! This is a readability hint, not a correctness issue; no autofix is
//! offered because flattening the block requires re-indentation the lint
//! layer doesn't model.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Block, HirFile, Stmt};
use leek_span::Span;

use super::for_each_block;
use crate::LintRule;

pub struct UnnecessaryElse;

impl LintRule for UnnecessaryElse {
    fn name(&self) -> &'static str {
        "unnecessary-else"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::UNNECESSARY_ELSE
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for_each_block(file, &mut |block: &Block| {
            for stmt in &block.stmts {
                if let Stmt::If(i) = stmt
                    && let Some(els) = &i.else_branch
                    && then_always_exits(&i.then_branch)
                {
                    // Don't flag `else if` chains — those aren't redundant
                    // nesting, they're a guard sequence.
                    if matches!(els.as_ref(), Stmt::If(_)) {
                        continue;
                    }
                    out.push(diagnostic(els.span(), exit_kind(&i.then_branch)));
                }
            }
        });
    }
}

/// True when `s` is guaranteed to transfer control out of the enclosing
/// block — a bare terminator, or a block whose last statement is one.
fn then_always_exits(s: &Stmt) -> bool {
    match s {
        Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_) => true,
        Stmt::Block(b) => b.stmts.last().is_some_and(then_always_exits),
        _ => false,
    }
}

/// The kind of terminator at the tail of `s`, for the message.
fn exit_kind(s: &Stmt) -> &'static str {
    match s {
        Stmt::Return(_) => "return",
        Stmt::Break(_) => "break",
        Stmt::Continue(_) => "continue",
        Stmt::Block(b) => b.stmts.last().map_or("exit", exit_kind),
        _ => "exit",
    }
}

fn diagnostic(span: Span, kind: &str) -> Diagnostic {
    Diagnostic::new(
        codes::UNNECESSARY_ELSE,
        leek_diagnostics::Severity::Hint,
        span,
        format!("this `else` is unnecessary — the `if` branch always `{kind}`s"),
    )
    .with_note(
        "drop the `else` and dedent its body; control only reaches it when the `if` was false",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_parser::parse;
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    fn run(src: &str) -> Vec<Diagnostic> {
        let source = SourceId::new(1).unwrap();
        let parsed = parse(src, source, Version::V4);
        let ast = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
        let (hir, _) = leek_hir::lower_file(&ast, source);
        let mut out = Vec::new();
        UnnecessaryElse.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_else_after_return() {
        let d = run(
            "function f(x) {\n  if (x < 0) {\n    return -1\n  } else {\n    return 1\n  }\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("return"), "{d:?}");
    }

    #[test]
    fn flags_else_after_break_in_loop() {
        let d = run(
            "function f(x) {\n  while (true) {\n    if (x) {\n      break\n    } else {\n      x = 1\n    }\n  }\n  return x\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("break"), "{d:?}");
    }

    #[test]
    fn ignores_else_when_then_falls_through() {
        let d = run(
            "function f(x) {\n  if (x < 0) {\n    x = 1\n  } else {\n    x = 2\n  }\n  return x\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_else_if_chain() {
        let d = run(
            "function f(x) {\n  if (x < 0) {\n    return -1\n  } else if (x > 0) {\n    return 1\n  }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
