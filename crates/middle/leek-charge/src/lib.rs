//! Optional HIR pass: insert static op-budget [`Charge`] instructions.
//!
//! Canonical HIR has no `Charge` nodes — every backend that enforces
//! a budget can tick per-instruction on its own. That's correct but
//! noisy for backends that want a tight inner loop (the native code
//! generator, the bytecode interpreter), so this pass walks the
//! tree once, sums each block's constant per-statement / per-
//! expression cost, and prepends a single
//! [`Stmt::Charge`](leek_hir::Stmt::Charge) at the block's entry.
//!
//! ## What this pass does NOT do
//!
//! Dynamic, input-scaled costs (`replace(s, a, b)` ≈ `len(s) * len(a)`,
//! `clone(deep)`, etc.) live inside each builtin's implementation —
//! the interpreter's `replace` bumps its op-counter directly; the
//! Java runtime's `replace` calls `ai.ops(...)` itself. The pass has
//! no business reasoning about per-builtin formulas.
//!
//! ## Optional by design
//!
//! Backends opt in. `leek-backend-interp` runs the uncharged form by default
//! (per-statement tick); only when given a `charged_hir(file, opts)`
//! result does it use the block-level charges and skip the tick.
//! Java-exact mode keeps the per-instruction `ai.ops(1)` shape to
//! mirror the reference; Java-clean / interp-bytecode / native opt
//! in for tighter loops.

mod cost;
mod opts;
mod walk;

pub mod pipeline;

pub use opts::ChargeOpts;

use leek_hir::HirFile;

use cost::{charge_stmt_for_block_start, stmts_cost};
use walk::charge_file_defs;
use walk::walk_main;

/// Walk `hir`, prepending a single static [`Stmt::Charge`] to every
/// block, returning the rewritten file. The input is not mutated;
/// the canonical query (`hir(file)` in `leek-db`) stays unchanged.
pub fn add_charges(hir: &HirFile, opts: ChargeOpts) -> HirFile {
    let mut out = hir.clone();
    charge_file_defs(&mut out, opts);
    let main_cost = stmts_cost(&out.main, opts);
    if main_cost > 0 {
        out.main
            .insert(0, charge_stmt_for_block_start(&out.main, main_cost));
    }
    walk_main(&mut out.main, opts);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_hir::{Def, Function, HirFile, Stmt};

    fn empty_file() -> HirFile {
        HirFile::default()
    }

    #[test]
    fn empty_input_produces_empty_output() {
        let charged = add_charges(&empty_file(), ChargeOpts::default());
        assert!(charged.main.is_empty());
    }

    #[test]
    fn nonempty_main_gets_a_charge() {
        use leek_hir::{Expr, ExprKind, Literal, Type};
        use leek_span::Span;
        let span = Span::synthetic();
        let mut f = HirFile::default();
        f.main.push(Stmt::Return(Some(Expr {
            kind: ExprKind::Literal(Literal::Int(42)),
            ty: Type::Integer,
            span,
        })));
        let charged = add_charges(&f, ChargeOpts::default());
        assert!(matches!(charged.main.first(), Some(Stmt::Charge(_))));
    }

    #[test]
    fn function_body_gets_a_charge() {
        use leek_hir::{Block, Expr, ExprKind, Literal, Type};
        use leek_span::Span;
        let span = Span::synthetic();
        let mut f = HirFile::default();
        f.defs.push(Def::Function(Function {
            name: "f".into(),
            span,
            params: vec![],
            return_type: None,
            body: Some(Block {
                stmts: vec![Stmt::Return(Some(Expr {
                    kind: ExprKind::Literal(Literal::Int(7)),
                    ty: Type::Integer,
                    span,
                }))],
                span,
            }),
            backend_directives: vec![],
        }));
        let charged = add_charges(&f, ChargeOpts::default());
        let Def::Function(f) = &charged.defs[0] else {
            panic!()
        };
        let body = f.body.as_ref().unwrap();
        assert!(matches!(body.stmts.first(), Some(Stmt::Charge(_))));
    }

    #[test]
    fn lambda_body_gets_a_charge() {
        // A block-bodied lambda's body used to be a charge "blind spot": the
        // expression walk treats a lambda as a leaf, so its body never received
        // a block-entry charge and lambda-heavy programs were under-counted.
        use leek_hir::{Block, Expr, ExprKind, LambdaBody, LambdaExpr, Literal, Type};
        use leek_span::Span;
        let span = Span::synthetic();
        let int_lit = |n| Expr {
            kind: ExprKind::Literal(Literal::Int(n)),
            ty: Type::Integer,
            span,
        };
        let lambda = Expr {
            kind: ExprKind::Lambda(LambdaExpr {
                params: vec![],
                body: LambdaBody::Block(Block {
                    stmts: vec![Stmt::Return(Some(int_lit(1)))],
                    span,
                }),
            }),
            ty: Type::Function,
            span,
        };
        let mut f = HirFile::default();
        f.main.push(Stmt::Expr(lambda));
        let charged = add_charges(&f, ChargeOpts::default());

        // Find the lambda in the (now-charged) main and assert its body block
        // starts with a Charge.
        let body_charged = charged.main.iter().any(|s| {
            let Stmt::Expr(Expr {
                kind: ExprKind::Lambda(l),
                ..
            }) = s
            else {
                return false;
            };
            matches!(&l.body, LambdaBody::Block(b) if matches!(b.stmts.first(), Some(Stmt::Charge(_))))
        });
        assert!(body_charged, "lambda body block should receive a block-entry charge");
    }
}
