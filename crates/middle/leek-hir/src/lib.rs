//! High-level intermediate representation for Leekscript.
//!
//! HIR sits between the AST (rowan-backed CST + typed view) and the
//! backends. Every name use is resolved to a [`DefId`], every
//! expression carries its inferred [`Type`], and a handful of sugar
//! shapes can be desugared here so backends downstream don't each
//! need their own rules.
//!
//! The include, scope and version rules this lowering implements are
//! written up in `docs/semantics.md`.

pub mod captures;
pub mod fold;
pub mod ir;
pub mod lower;
pub mod pipeline;
pub mod transform;
pub mod visit;

pub use captures::{captured_by_nested_lambda_body, captured_by_nested_lambda_stmts};
pub use fold::fold_map;
pub use ir::*;
pub use lower::{
    LowerUnit, finish, lower_file, lower_file_versioned, lower_file_with_prelude, lower_files,
    lower_one, prelude_tree,
};
pub use transform::fold_constants;
pub use visit::{
    Flow, HirVisitor, HirVisitorMut, OnExpr, OnStmt, Visit, VisitMut, Visitable, VisitableMut,
    walk_expr_children, walk_expr_children_mut, walk_lambda_stmts_in_expr, walk_stmt_child_exprs,
    walk_stmt_child_exprs_mut, walk_stmt_child_stmts, walk_stmt_child_stmts_mut, walk_stmts_deep,
};

/// Re-export the type model so HIR consumers don't need to take a
/// direct dependency on `leek-types`.
pub use leek_types::Type;
