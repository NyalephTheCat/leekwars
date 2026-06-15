//! Dead-code elimination — conservative, only provably-unreachable code.
//!
//! - Statements after an unconditional `return`/`break`/`continue` in a
//!   block are dropped.
//! - An `if` with a literal-boolean condition collapses to its taken
//!   branch (or is removed when the untaken branch is absent).

use leek_hir::{Block, ExprKind, IfStmt, Literal, Stmt};

pub(crate) fn run_stmts(stmts: &mut Vec<Stmt>) {
    for s in stmts.iter_mut() {
        run_stmt(s);
    }
    // Drop empty blocks left behind by collapsing constant `if`s.
    stmts.retain(|s| !is_empty_block(s));
    if let Some(pos) = stmts.iter().position(is_terminator) {
        stmts.truncate(pos + 1);
    }
}

fn is_empty_block(s: &Stmt) -> bool {
    matches!(s, Stmt::Block(b) if b.stmts.is_empty())
}

fn run_stmt(s: &mut Stmt) {
    // Collapse `if` with a constant condition before recursing.
    if let Stmt::If(i) = s
        && let Some(b) = if_const_cond(i)
    {
        let taken = if b {
            Some((*i.then_branch).clone())
        } else {
            i.else_branch.as_ref().map(|e| (**e).clone())
        };
        *s = taken.unwrap_or_else(|| {
            Stmt::Block(Block {
                stmts: Vec::new(),
                span: i.span,
            })
        });
        run_stmt(s);
        return;
    }

    match s {
        Stmt::Block(b) => run_stmts(&mut b.stmts),
        Stmt::If(i) => {
            run_stmt(&mut i.then_branch);
            if let Some(e) = &mut i.else_branch {
                run_stmt(e);
            }
        }
        Stmt::While(w) => run_stmt(&mut w.body),
        Stmt::DoWhile(d) => run_stmt(&mut d.body),
        Stmt::For(f) => run_stmt(&mut f.body),
        Stmt::Foreach(fe) => run_stmt(&mut fe.body),
        Stmt::Switch(sw) => {
            for arm in &mut sw.arms {
                run_stmts(&mut arm.body);
            }
        }
        _ => {}
    }
}

fn if_const_cond(i: &IfStmt) -> Option<bool> {
    match &i.cond.kind {
        ExprKind::Literal(Literal::Bool(b)) => Some(*b),
        _ => None,
    }
}

fn is_terminator(s: &Stmt) -> bool {
    matches!(s, Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_))
}
