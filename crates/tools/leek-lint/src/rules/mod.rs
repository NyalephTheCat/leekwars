//! Individual lint rule implementations.
//!
//! Each rule lives in its own module and is a unit struct
//! implementing [`crate::LintRule`].

pub mod assignment_in_condition;
pub mod constant_condition;
pub mod deprecated_feature;
pub mod division_by_zero;
pub mod double_negation;
pub mod duplicate_branches;
pub mod duplicate_case;
pub mod duplicate_condition;
pub mod duplicate_include;
pub mod empty_block;
pub mod identical_operands;
pub mod negated_comparison;
pub mod redundant_boolean;
pub mod redundant_ternary;
pub mod self_assignment;
pub mod self_comparison;
pub mod shadowed_binding;
pub(crate) mod structural;
pub mod unnecessary_else;
pub mod unreachable_code;
pub mod unused_expression;
pub mod unused_parameter;
pub mod unused_variable;

// ---- Shared HIR walker helpers ----

use leek_hir::{Block, Def, Expr, HirFile, Stmt};
use leek_span::Span;

/// Visit every `Block` reachable from `file` — the main block plus
/// each function/method/constructor body plus every nested
/// statement-block. The callback runs once per block.
pub(crate) fn for_each_block(file: &HirFile, f: &mut impl FnMut(&Block)) {
    // Synthesize a `Block` wrapper for the main statements so
    // walking semantics are uniform. The span is a sentinel — the
    // visitor doesn't read it.
    let main = Block {
        stmts: file.main.clone(),
        span: Span::synthetic(),
    };
    visit_blocks(&main, f);
    for def in &file.defs {
        match def {
            Def::Function(fun) => {
                if let Some(body) = &fun.body {
                    visit_blocks(body, f);
                }
            }
            Def::Class(cls) => {
                for m in cls.methods.iter().chain(cls.constructors.iter()) {
                    if let Some(body) = &m.body {
                        visit_blocks(body, f);
                    }
                }
            }
            Def::Global(_) | Def::Local(_) => {}
        }
    }
}

fn visit_blocks(block: &Block, f: &mut impl FnMut(&Block)) {
    f(block);
    for s in &block.stmts {
        visit_blocks_in_stmt(s, f);
    }
}

fn visit_blocks_in_stmt(s: &Stmt, f: &mut impl FnMut(&Block)) {
    match s {
        Stmt::Block(b) => visit_blocks(b, f),
        Stmt::If(i) => {
            visit_blocks_in_stmt(&i.then_branch, f);
            if let Some(e) = &i.else_branch {
                visit_blocks_in_stmt(e, f);
            }
        }
        Stmt::While(w) => visit_blocks_in_stmt(&w.body, f),
        Stmt::DoWhile(d) => visit_blocks_in_stmt(&d.body, f),
        Stmt::For(fr) => {
            if let Some(init) = &fr.init {
                visit_blocks_in_stmt(init, f);
            }
            visit_blocks_in_stmt(&fr.body, f);
        }
        Stmt::Foreach(fe) => visit_blocks_in_stmt(&fe.body, f),
        Stmt::Switch(sw) => {
            for arm in &sw.arms {
                let synthetic = Block {
                    stmts: arm.body.clone(),
                    span: Span::synthetic(),
                };
                visit_blocks(&synthetic, f);
            }
        }
        _ => {}
    }
}

/// Visit every [`Stmt`] inside `block`, including those nested in
/// child block-like statements. Order is source order.
pub(crate) fn for_each_stmt(block: &Block, f: &mut impl FnMut(&Stmt)) {
    for s in &block.stmts {
        f(s);
        descend_stmt(s, f);
    }
}

/// Visit every nested [`Stmt`] reachable from `s`.
pub(crate) fn descend_stmt(s: &Stmt, f: &mut impl FnMut(&Stmt)) {
    match s {
        Stmt::If(i) => {
            f(&i.then_branch);
            descend_stmt(&i.then_branch, f);
            if let Some(e) = &i.else_branch {
                f(e);
                descend_stmt(e, f);
            }
        }
        Stmt::While(w) => {
            f(&w.body);
            descend_stmt(&w.body, f);
        }
        Stmt::DoWhile(dw) => {
            f(&dw.body);
            descend_stmt(&dw.body, f);
        }
        Stmt::For(fr) => {
            if let Some(init) = &fr.init {
                f(init);
                descend_stmt(init, f);
            }
            f(&fr.body);
            descend_stmt(&fr.body, f);
        }
        Stmt::Foreach(fe) => {
            f(&fe.body);
            descend_stmt(&fe.body, f);
        }
        Stmt::Block(b) => {
            for inner in &b.stmts {
                f(inner);
                descend_stmt(inner, f);
            }
        }
        Stmt::Switch(sw) => {
            for arm in &sw.arms {
                for inner in &arm.body {
                    f(inner);
                    descend_stmt(inner, f);
                }
            }
        }
        Stmt::Expr(_)
        | Stmt::VarDecl(_)
        | Stmt::Return(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => {}
    }
}

/// Visit every [`Expr`] inside `block`, recursing through nested
/// statements and sub-expressions. Used by lints that need to find
/// every reference to something.
pub(crate) fn for_each_expr_in_block(block: &Block, f: &mut impl FnMut(&Expr)) {
    for s in &block.stmts {
        for_each_expr_in_stmt(s, f);
    }
}

pub(crate) fn for_each_expr_in_stmt(s: &Stmt, f: &mut impl FnMut(&Expr)) {
    match s {
        Stmt::Expr(e) => for_each_expr(e, f),
        Stmt::VarDecl(v) => {
            if let Some(init) = &v.init {
                for_each_expr(init, f);
            }
        }
        Stmt::Return(opt) => {
            if let Some(e) = opt {
                for_each_expr(e, f);
            }
        }
        Stmt::If(i) => {
            for_each_expr(&i.cond, f);
            for_each_expr_in_stmt(&i.then_branch, f);
            if let Some(e) = &i.else_branch {
                for_each_expr_in_stmt(e, f);
            }
        }
        Stmt::While(w) => {
            for_each_expr(&w.cond, f);
            for_each_expr_in_stmt(&w.body, f);
        }
        Stmt::DoWhile(dw) => {
            for_each_expr_in_stmt(&dw.body, f);
            for_each_expr(&dw.cond, f);
        }
        Stmt::For(fr) => {
            if let Some(init) = &fr.init {
                for_each_expr_in_stmt(init, f);
            }
            if let Some(c) = &fr.cond {
                for_each_expr(c, f);
            }
            if let Some(s) = &fr.step {
                for_each_expr(s, f);
            }
            for_each_expr_in_stmt(&fr.body, f);
        }
        Stmt::Foreach(fe) => {
            for_each_expr(&fe.iter, f);
            for_each_expr_in_stmt(&fe.body, f);
        }
        Stmt::Block(b) => for_each_expr_in_block(b, f),
        Stmt::Switch(sw) => {
            for_each_expr(&sw.discriminant, f);
            for arm in &sw.arms {
                if let Some(c) = &arm.case {
                    for_each_expr(c, f);
                }
                for inner in &arm.body {
                    for_each_expr_in_stmt(inner, f);
                }
            }
        }
        Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => {}
    }
}

pub(crate) fn for_each_expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    // `walk_expr_children` covers every `ExprKind` variant (the
    // hand-rolled match this replaced silently dropped `Ternary`,
    // `Map`, `Set`, `Object`, `Slice`, `Interval`, `Cast`, and
    // `New`). It treats `Lambda` as a leaf, so the previous
    // skip-lambda-bodies behaviour is preserved: rule authors who
    // need lambda internals special-case `ExprKind::Lambda`.
    leek_hir::walk_expr_children(e, &mut |child| for_each_expr(child, f));
}
