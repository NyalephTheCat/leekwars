//! Recursive walk that prepends block-entry charges.

use leek_hir::{
    Block, DoWhileStmt, Expr, ExprKind, ForStmt, ForeachStmt, HirFile, IfStmt, LambdaBody, Stmt,
    SwitchStmt, WhileStmt,
};

use crate::cost::{charge_stmt_for_block_start, stmt_cost, stmts_cost};
use crate::opts::ChargeOpts;

pub(crate) fn charge_block(block: &mut Block, opts: ChargeOpts) {
    walk_block_recurse(block, opts);
    let total = stmts_cost(&block.stmts, opts);
    if total > 0 {
        block
            .stmts
            .insert(0, charge_stmt_for_block_start(&block.stmts, total));
    }
}

pub(crate) fn walk_main(stmts: &mut [Stmt], opts: ChargeOpts) {
    for s in stmts {
        walk_stmt_recurse(s, opts);
    }
}

fn walk_block_recurse(block: &mut Block, opts: ChargeOpts) {
    for s in &mut block.stmts {
        walk_stmt_recurse(s, opts);
    }
}

fn walk_stmt_recurse(s: &mut Stmt, opts: ChargeOpts) {
    match s {
        Stmt::If(IfStmt {
            then_branch,
            else_branch,
            ..
        }) => {
            wrap_branch(then_branch, opts);
            if let Some(e) = else_branch {
                wrap_branch(e, opts);
            }
        }
        Stmt::While(WhileStmt { body, .. })
        | Stmt::DoWhile(DoWhileStmt { body, .. })
        | Stmt::For(ForStmt { body, .. })
        | Stmt::Foreach(ForeachStmt { body, .. }) => {
            wrap_branch(body, opts);
        }
        Stmt::Switch(SwitchStmt { arms, .. }) => {
            for arm in arms {
                let cost = stmts_cost(&arm.body, opts);
                if cost > 0 {
                    arm.body
                        .insert(0, charge_stmt_for_block_start(&arm.body, cost));
                }
                for s in &mut arm.body {
                    walk_stmt_recurse(s, opts);
                }
            }
        }
        Stmt::Block(b) => charge_block(b, opts),
        _ => {}
    }
    // Lambdas are leaves to the normal expression walk, so their bodies are
    // never charged by the block/statement recursion above. Descend into any
    // lambda appearing in this statement's immediate expressions and charge
    // its body explicitly, so lambda-body static cost is accounted.
    leek_hir::walk_stmt_child_exprs_mut(s, &mut |e| charge_lambdas_in_expr(e, opts));
}

/// Charge the body block of every lambda reachable from `e`. A block-bodied
/// lambda gets the same block-entry charge a function body would; an
/// expression-bodied lambda has no block but may still nest further lambdas.
/// `walk_expr_children_mut` treats a lambda as a leaf, so the body is only ever
/// reached (and thus charged) through this explicit descent — no double-charge.
fn charge_lambdas_in_expr(e: &mut Expr, opts: ChargeOpts) {
    if let ExprKind::Lambda(l) = &mut e.kind {
        match &mut l.body {
            LambdaBody::Block(b) => charge_block(b, opts),
            LambdaBody::Expr(inner) => charge_lambdas_in_expr(inner, opts),
        }
    }
    leek_hir::walk_expr_children_mut(e, &mut |child| charge_lambdas_in_expr(child, opts));
}

/// If the branch is a `Block`, attach its own charge as we'd for a
/// normal block. If it's a single statement, wrap it in a one-stmt
/// block + charge so backends see a uniform shape.
fn wrap_branch(branch: &mut Box<Stmt>, opts: ChargeOpts) {
    match branch.as_mut() {
        Stmt::Block(b) => charge_block(b, opts),
        other => {
            let cost = stmt_cost(other, opts);
            if cost > 0 {
                let span = other.span();
                let inner = std::mem::replace(other, Stmt::Continue(span));
                let mut new_block = Block {
                    stmts: vec![inner],
                    span,
                };
                walk_block_recurse(&mut new_block, opts);
                new_block.stmts.insert(0, Stmt::Charge(cost));
                *branch.as_mut() = Stmt::Block(new_block);
            }
        }
    }
}

pub(crate) fn charge_file_defs(file: &mut HirFile, opts: ChargeOpts) {
    for def in &mut file.defs {
        match def {
            leek_hir::Def::Function(f) => {
                if let Some(b) = &mut f.body {
                    charge_block(b, opts);
                }
            }
            leek_hir::Def::Class(c) => {
                for m in c.methods.iter_mut().chain(c.constructors.iter_mut()) {
                    if let Some(b) = &mut m.body {
                        charge_block(b, opts);
                    }
                }
            }
            leek_hir::Def::Global(_) | leek_hir::Def::Local(_) => {}
        }
    }
}
