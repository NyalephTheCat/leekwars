//! Shared lowering interfaces for HIR (and reference for MIR lowering).

use leek_parser::ast::{Expr as AstExpr, Stmt as AstStmt};

use crate::ir::{Expr, Stmt};

/// Expression lowering — implemented by [`super::Lowerer`].
pub trait LowerExpr {
    fn lower_expr(&mut self, e: &AstExpr) -> Expr;
}

/// Statement lowering — implemented by [`super::Lowerer`].
pub trait LowerStmt {
    fn lower_stmt(&mut self, stmt: &AstStmt) -> Stmt;
}
