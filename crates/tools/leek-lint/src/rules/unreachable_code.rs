//! L0005 `UnreachableCode` — flag statements that follow a definite
//! control-flow exit (`return`, `break`, `continue`).
//!
//! ## Detection
//!
//! Walk every block (the main block + each function/method body
//! plus nested blocks) and emit one finding the first time a
//! statement appears after a "terminator". Subsequent siblings in
//! the same block aren't reported again — one diagnostic per
//! unreachable region is friendlier than N.
//!
//! ## Known limitations
//!
//! - **Conditional terminators** (`if (cond) { return … }`) aren't
//!   treated as definite returns yet — the rule only fires on
//!   straight-line `return`/`break`/`continue`. Catching the
//!   `if (a) return else return` shape requires the same
//!   `stmt_definitely_returns` analysis the Java backend uses;
//!   factor that out in a later slice if useful.
//! - **Switch arms** aren't analyzed individually — each arm is
//!   walked as its own straight-line block.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Block, HirFile, Stmt};
use leek_span::Span;

use super::for_each_block;
use crate::LintRule;

pub struct UnreachableCode;

impl LintRule for UnreachableCode {
    fn name(&self) -> &'static str {
        "unreachable-code"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::UNREACHABLE_CODE
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for_each_block(file, &mut |block: &Block| {
            check_block(block, out);
        });
    }
}

fn check_block(block: &Block, out: &mut Vec<Diagnostic>) {
    let mut iter = block.stmts.iter();
    while let Some(stmt) = iter.next() {
        if is_terminator(stmt) {
            // Everything after this in the same block is
            // unreachable. Emit one diagnostic spanning the first
            // such statement; siblings inherit the same annotation.
            if let Some(next) = iter.next() {
                out.push(diagnostic(next.span(), terminator_kind(stmt)));
            }
            break;
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
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_parser::parse;
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    fn run(src: &str) -> Vec<Diagnostic> {
        let source = SourceId::new(1).unwrap();
        let parsed = parse(src, source, Version::V4);
        let root = SyntaxNode::new_root(parsed.green);
        let ast = SourceFile::cast(root).unwrap();
        let (hir, _) = leek_hir::lower_file(&ast, source);
        let mut out = Vec::new();
        UnreachableCode.check(&hir, &mut out);
        out
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
