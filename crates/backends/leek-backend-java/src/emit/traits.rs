//! Shared emission interfaces for Java backend submodules.

use leek_hir::{Expr, Stmt};

/// Expression emission — implemented by [`super::Emitter`].
pub trait EmitExpr {
    fn write_expr(&self, buf: &mut String, e: &Expr, parens_if_negative: bool);
}

/// Statement emission — implemented by [`super::Emitter`].
pub trait EmitStmt {
    fn emit_stmt(&mut self, s: &Stmt);
}
