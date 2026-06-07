//! L0004 `EmptyBlock` — flag `if`, `else`, `while`, `for`,
//! `foreach`, and `do-while` whose body is an empty block.
//!
//! Severity is Hint: the construct is sometimes intentional (e.g.
//! polling spin-wait) but more often a leftover stub.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{Block, Def, HirFile, Stmt};

use super::for_each_stmt;
use crate::LintRule;

pub struct EmptyBlock;

impl LintRule for EmptyBlock {
    fn name(&self) -> &'static str {
        "empty-block"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::EMPTY_BLOCK
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        check_block(&file.main, out);
        for def in &file.defs {
            match def {
                Def::Function(fun) => {
                    if let Some(body) = &fun.body {
                        check_block(&body.stmts, out);
                    }
                }
                Def::Class(cls) => {
                    for m in cls.methods.iter().chain(cls.constructors.iter()) {
                        if let Some(body) = &m.body {
                            check_block(&body.stmts, out);
                        }
                    }
                }
                Def::Global(_) | Def::Local(_) => {}
            }
        }
    }
}

fn check_block(stmts: &[Stmt], out: &mut Vec<Diagnostic>) {
    let wrapper = Block {
        stmts: stmts.to_vec(),
        span: leek_span::Span::synthetic(),
    };
    for_each_stmt(&wrapper, &mut |s| match s {
        Stmt::If(i) => {
            if let Stmt::Block(b) = &*i.then_branch
                && b.stmts.is_empty()
            {
                out.push(empty(b.span, "if"));
            }
            if let Some(else_b) = &i.else_branch
                && let Stmt::Block(b) = &**else_b
                && b.stmts.is_empty()
            {
                out.push(empty(b.span, "else"));
            }
        }
        Stmt::While(w) => {
            if let Stmt::Block(b) = &*w.body
                && b.stmts.is_empty()
            {
                out.push(empty(b.span, "while"));
            }
        }
        Stmt::DoWhile(dw) => {
            if let Stmt::Block(b) = &*dw.body
                && b.stmts.is_empty()
            {
                out.push(empty(b.span, "do-while"));
            }
        }
        Stmt::For(fr) => {
            if let Stmt::Block(b) = &*fr.body
                && b.stmts.is_empty()
            {
                out.push(empty(b.span, "for"));
            }
        }
        Stmt::Foreach(fe) => {
            if let Stmt::Block(b) = &*fe.body
                && b.stmts.is_empty()
            {
                out.push(empty(b.span, "foreach"));
            }
        }
        _ => {}
    });
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
