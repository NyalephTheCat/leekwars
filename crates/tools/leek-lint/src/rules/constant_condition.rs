//! L0006 `ConstantCondition` — flag `if` / `while` / `do-while`
//! whose condition is a compile-time constant.
//!
//! Catches accidentally-trivial branches like `if (5)` (which
//! Leekscript truthiness coerces to `true`) and `while (true)`
//! style infinite loops that were probably meant to be conditional.
//!
//! ## Idiom carve-outs
//!
//! - `while (true) { … break; }` is the canonical infinite-loop
//!   idiom (and what `for (;;)` translates to). To keep the rule
//!   friendly, we only flag `while (true)` when the body doesn't
//!   contain a `break` or `return` anywhere.
//! - `do { … } while (false)` is sometimes used as a labeled
//!   one-shot block in C-style languages. Flag it anyway — Leek
//!   has `if`/`block` for that. The user can `@allow` if they
//!   actually meant it.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Block, Expr, ExprKind, HirFile, Literal, Stmt};
use leek_span::Span;

use super::for_each_block;
use crate::LintRule;

pub struct ConstantCondition;

impl LintRule for ConstantCondition {
    fn name(&self) -> &'static str {
        "constant-condition"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::CONSTANT_CONDITION
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        for_each_block(file, &mut |block: &Block| {
            for stmt in &block.stmts {
                check_stmt(stmt, out);
            }
        });
    }
}

fn check_stmt(s: &Stmt, out: &mut Vec<Diagnostic>) {
    match s {
        Stmt::If(i) => {
            if let Some(verdict) = constant_truthiness(&i.cond) {
                out.push(diagnostic(i.cond.span, "if", verdict));
            }
        }
        Stmt::While(w) => {
            if let Some(verdict) = constant_truthiness(&w.cond) {
                if verdict && body_breaks_or_returns(&w.body) {
                    // Idiomatic `while (true) { … break }`.
                    return;
                }
                out.push(diagnostic(w.cond.span, "while", verdict));
            }
        }
        Stmt::DoWhile(d) => {
            if let Some(verdict) = constant_truthiness(&d.cond) {
                if verdict && body_breaks_or_returns(&d.body) {
                    return;
                }
                out.push(diagnostic(d.cond.span, "do-while", verdict));
            }
        }
        _ => {}
    }
}

/// Return `Some(true)`/`Some(false)` when `e` evaluates to a known
/// constant truthy or falsy value at compile time. Returns `None`
/// for any expression that depends on runtime state.
fn constant_truthiness(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Literal(lit) => match lit {
            Literal::Bool(b) => Some(*b),
            Literal::Null | Literal::Int(0) => Some(false),
            Literal::Int(_) => Some(true),
            Literal::Real(r) => Some(*r != 0.0),
            Literal::String(s) => Some(!s.is_empty()),
        },
        // `!const` flips. Recurse so `!false`, `!!true`, etc. all
        // collapse cleanly.
        ExprKind::Unary(leek_hir::UnaryOp::Not, inner) => constant_truthiness(inner).map(|b| !b),
        _ => None,
    }
}

/// Heuristic: does the loop body contain a top-level `break`,
/// `return`, or any nested terminator that would make `while (true)`
/// the canonical idiom rather than a mistake? Walks one level into
/// blocks / if / nested loops; doesn't follow lambdas.
fn body_breaks_or_returns(body: &Stmt) -> bool {
    fn walk(s: &Stmt) -> bool {
        match s {
            Stmt::Break(_) | Stmt::Return(_) => true,
            Stmt::Block(b) => b.stmts.iter().any(walk),
            Stmt::If(i) => walk(&i.then_branch) || i.else_branch.as_deref().is_some_and(walk),
            Stmt::While(w) => walk(&w.body),
            Stmt::DoWhile(d) => walk(&d.body),
            Stmt::For(fr) => walk(&fr.body),
            Stmt::Foreach(fe) => walk(&fe.body),
            Stmt::Switch(sw) => sw.arms.iter().any(|a| a.body.iter().any(walk)),
            _ => false,
        }
    }
    walk(body)
}

fn diagnostic(span: Span, kind: &str, value: bool) -> Diagnostic {
    let truthy = if value { "always true" } else { "always false" };
    Diagnostic::warning(
        codes::CONSTANT_CONDITION,
        span,
        format!("`{kind}` condition is {truthy} — the body is unconditional"),
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
        ConstantCondition.check(&hir, &mut out);
        out
    }

    #[test]
    fn flags_if_true() {
        let d = run("if (true) { var x = 1 }\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("always true"));
    }

    #[test]
    fn flags_if_false() {
        let d = run("if (false) { var x = 1 }\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("always false"));
    }

    #[test]
    fn flags_if_integer() {
        let d = run("if (5) { var x = 1 }\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("always true"));
    }

    #[test]
    fn flags_if_zero() {
        let d = run("if (0) { var x = 1 }\n");
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn carves_out_while_true_with_break() {
        let d = run("while (true) { break }\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn flags_while_true_without_break() {
        let d = run("while (true) { var x = 1 }\n");
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn ignores_variable_condition() {
        let d = run("var x = 1\nif (x) { var y = 1 }\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
