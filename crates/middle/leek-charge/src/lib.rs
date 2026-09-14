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
//! Backends opt in. Java-exact mode keeps the per-instruction `ai.ops(1)`
//! shape to mirror the reference; Java-clean and native opt in to the
//! block-level charges for tighter loops.
//!
//! ## Run it exactly once
//!
//! The pass is **not** idempotent: an inserted `Charge` is itself a
//! statement, so it costs `per_stmt` on a second run and every block's
//! budget inflates. `charging_twice_inflates_the_budget` pins that.

mod cost;
mod opts;
mod walk;

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
    use leek_hir::{
        Block, Def, Expr, ExprKind, Function, HirFile, IfStmt, LambdaBody, LambdaExpr, Literal,
        Stmt, SwitchArm, SwitchStmt, Type, WhileStmt,
    };
    use leek_span::Span;

    fn empty_file() -> HirFile {
        HirFile::default()
    }

    fn span() -> Span {
        Span::synthetic()
    }

    fn int(n: i64) -> Expr {
        Expr {
            kind: ExprKind::Literal(Literal::Int(n)),
            ty: Type::Integer,
            span: span(),
        }
    }

    fn add(l: Expr, r: Expr) -> Expr {
        Expr {
            kind: ExprKind::Binary(leek_hir::BinaryOp::Add, Box::new(l), Box::new(r)),
            ty: Type::Integer,
            span: span(),
        }
    }

    fn main_of(stmts: Vec<Stmt>) -> HirFile {
        HirFile {
            main: stmts,
            ..HirFile::default()
        }
    }

    /// The `Charge` payload a block starts with, or `None` if it has none.
    fn entry_charge(stmts: &[Stmt]) -> Option<u64> {
        match stmts.first() {
            Some(Stmt::Charge(n)) => Some(*n),
            _ => None,
        }
    }

    #[test]
    fn empty_input_produces_empty_output() {
        let charged = add_charges(&empty_file(), ChargeOpts::default());
        assert!(charged.main.is_empty());
    }

    #[test]
    fn nonempty_main_gets_a_charge() {
        let f = main_of(vec![Stmt::Return(Some(int(42)))]);
        let charged = add_charges(&f, ChargeOpts::default());
        // `return 42;` = one statement + one expression.
        assert_eq!(entry_charge(&charged.main), Some(2));
        // The original statements survive, unmoved, after the charge.
        assert_eq!(charged.main.len(), 2);
        assert!(matches!(charged.main[1], Stmt::Return(Some(_))));
    }

    #[test]
    fn function_body_gets_a_charge() {
        let mut f = HirFile::default();
        f.defs.push(Def::Function(Function {
            name: "f".into(),
            span: span(),
            params: vec![],
            return_type: None,
            body: Some(Block {
                stmts: vec![
                    Stmt::Return(Some(add(int(7), int(1)))), // 1 + 3
                    Stmt::Break(span()),                     // 1
                ],
                span: span(),
            }),
            backend_directives: vec![],
        }));
        let charged = add_charges(&f, ChargeOpts::default());
        let Def::Function(f) = &charged.defs[0] else {
            panic!("expected a function def")
        };
        let body = f.body.as_ref().unwrap();
        assert_eq!(entry_charge(&body.stmts), Some(5));
    }

    #[test]
    fn lambda_body_gets_a_charge() {
        // A block-bodied lambda's body used to be a charge "blind spot": the
        // expression walk treats a lambda as a leaf, so its body never received
        // a block-entry charge and lambda-heavy programs were under-counted.
        let lambda = Expr {
            kind: ExprKind::Lambda(LambdaExpr {
                params: vec![],
                body: LambdaBody::Block(Block {
                    stmts: vec![Stmt::Return(Some(add(int(1), int(2))))], // 1 + 3
                    span: span(),
                }),
            }),
            ty: Type::Function,
            span: span(),
        };
        let f = main_of(vec![Stmt::Expr(lambda)]);
        let charged = add_charges(&f, ChargeOpts::default());

        // The lambda itself is a leaf expression, so main pays 1 (statement)
        // + 1 (the lambda expression) and nothing for the deferred body…
        assert_eq!(entry_charge(&charged.main), Some(2));

        // …while the body block carries its own entry charge.
        let Some(Stmt::Expr(Expr {
            kind: ExprKind::Lambda(l),
            ..
        })) = charged.main.get(1)
        else {
            panic!("expected the lambda statement to survive the walk")
        };
        let LambdaBody::Block(b) = &l.body else {
            panic!("expected a block-bodied lambda")
        };
        assert_eq!(entry_charge(&b.stmts), Some(4));
    }

    /// A single-statement branch is promoted to a one-statement block so
    /// backends see a uniform shape, and the charge it gets is exactly the
    /// promoted statement's own cost.
    #[test]
    fn a_single_statement_branch_is_promoted_to_a_charged_block() {
        let f = main_of(vec![Stmt::If(IfStmt {
            cond: int(1),
            then_branch: Box::new(Stmt::Expr(add(int(1), int(2)))), // 1 + 3
            else_branch: Some(Box::new(Stmt::Break(span()))),       // 1
            soft: false,
            span: span(),
        })]);
        let charged = add_charges(&f, ChargeOpts::default());
        // main: the `if` statement + its condition. The branches are not
        // counted here — they pay their own way.
        assert_eq!(entry_charge(&charged.main), Some(2));

        let Some(Stmt::If(i)) = charged.main.get(1) else {
            panic!("expected the if to survive the walk")
        };
        let Stmt::Block(then_block) = i.then_branch.as_ref() else {
            panic!("then branch should have been promoted to a block")
        };
        assert_eq!(entry_charge(&then_block.stmts), Some(4));
        assert_eq!(then_block.stmts.len(), 2, "charge + the original statement");

        let Some(else_branch) = &i.else_branch else {
            panic!("else branch missing")
        };
        let Stmt::Block(else_block) = else_branch.as_ref() else {
            panic!("else branch should have been promoted to a block")
        };
        assert_eq!(entry_charge(&else_block.stmts), Some(1));
    }

    /// Loop bodies go through the same promotion, and an already-block body
    /// is charged in place rather than wrapped a second time.
    #[test]
    fn a_block_bodied_loop_is_charged_in_place() {
        let f = main_of(vec![Stmt::While(WhileStmt {
            cond: add(int(1), int(2)), // 3
            body: Box::new(Stmt::Block(Block {
                stmts: vec![Stmt::Expr(int(0)), Stmt::Break(span())], // 2 + 1
                span: span(),
            })),
            span: span(),
        })]);
        let charged = add_charges(&f, ChargeOpts::default());
        assert_eq!(entry_charge(&charged.main), Some(4)); // stmt + cond

        let Some(Stmt::While(w)) = charged.main.get(1) else {
            panic!("expected the while to survive the walk")
        };
        let Stmt::Block(b) = w.body.as_ref() else {
            panic!("body should still be a block")
        };
        assert_eq!(entry_charge(&b.stmts), Some(3));
        assert_eq!(b.stmts.len(), 3, "no extra wrapping block was introduced");
    }

    /// Each switch arm is charged separately: only one arm runs, so summing
    /// them into the discriminant's charge would over-bill every fight.
    #[test]
    fn each_switch_arm_carries_its_own_charge() {
        let f = main_of(vec![Stmt::Switch(SwitchStmt {
            discriminant: int(1),
            arms: vec![
                SwitchArm {
                    case: Some(int(1)),
                    body: vec![Stmt::Expr(add(int(1), int(2)))], // 4
                },
                SwitchArm {
                    case: None,
                    body: vec![Stmt::Break(span())], // 1
                },
                SwitchArm {
                    case: Some(int(2)),
                    body: vec![], // nothing to charge
                },
            ],
            span: span(),
        })]);
        let charged = add_charges(&f, ChargeOpts::default());
        assert_eq!(entry_charge(&charged.main), Some(2)); // stmt + discriminant

        let Some(Stmt::Switch(s)) = charged.main.get(1) else {
            panic!("expected the switch to survive the walk")
        };
        assert_eq!(entry_charge(&s.arms[0].body), Some(4));
        assert_eq!(entry_charge(&s.arms[1].body), Some(1));
        assert_eq!(
            entry_charge(&s.arms[2].body),
            None,
            "an empty arm must not gain a zero charge"
        );
    }

    /// `add_charges` computes main's cost *before* recursing, while
    /// `walk::charge_block` computes a nested block's cost *after*. The two
    /// must agree: if the recursion ever started mutating the statement list
    /// it is about to price, the inserted `Charge` nodes would feed back into
    /// `stmts_cost` and inflate every nested block by `per_stmt` apiece.
    #[test]
    fn main_and_a_nested_block_price_the_same_statements_identically() {
        let body = || {
            vec![
                Stmt::Expr(add(int(1), int(2))),
                Stmt::If(IfStmt {
                    cond: int(1),
                    then_branch: Box::new(Stmt::Break(span())),
                    else_branch: None,
                    soft: false,
                    span: span(),
                }),
                Stmt::Return(None),
            ]
        };
        let opts = ChargeOpts::default();

        let as_main = add_charges(&main_of(body()), opts);
        let as_block = add_charges(
            &main_of(vec![Stmt::Block(Block {
                stmts: body(),
                span: span(),
            })]),
            opts,
        );

        let Some(Stmt::Block(b)) = as_block.main.get(1) else {
            panic!("expected the block to survive the walk")
        };
        assert_eq!(entry_charge(&as_main.main), entry_charge(&b.stmts));
    }

    /// The pass must run exactly once. An inserted `Charge` is a statement,
    /// so a second run pays `per_stmt` for it and every block's budget grows.
    /// Pinned deliberately: this is documented behaviour, not a bug to fix by
    /// accident.
    #[test]
    fn charging_twice_inflates_the_budget() {
        let f = main_of(vec![Stmt::Expr(add(int(1), int(2)))]); // 4
        let opts = ChargeOpts::default();
        let once = add_charges(&f, opts);
        assert_eq!(entry_charge(&once.main), Some(4));
        let twice = add_charges(&once, opts);
        assert_eq!(
            entry_charge(&twice.main),
            Some(5),
            "the second run bills the first run's Charge statement"
        );
    }

    /// Zero weights are a legitimate "measure nothing" configuration, and it
    /// must leave the tree byte-identical — no zero-valued `Charge` nodes, no
    /// single-statement branches promoted to blocks.
    #[test]
    fn zero_weights_insert_nothing_and_promote_nothing() {
        let f = main_of(vec![Stmt::If(IfStmt {
            cond: add(int(1), int(2)),
            then_branch: Box::new(Stmt::Expr(int(0))),
            else_branch: None,
            soft: false,
            span: span(),
        })]);
        let charged = add_charges(
            &f,
            ChargeOpts {
                per_stmt: 0,
                per_expr: 0,
            },
        );
        assert_eq!(charged, f);
    }

    /// The cost weights reach every level of the walk, not just the top.
    #[test]
    fn nondefault_weights_reach_nested_blocks() {
        let f = main_of(vec![Stmt::While(WhileStmt {
            cond: int(1),
            body: Box::new(Stmt::Expr(add(int(1), int(2)))),
            span: span(),
        })]);
        let opts = ChargeOpts {
            per_stmt: 3,
            per_expr: 7,
        };
        let charged = add_charges(&f, opts);
        // main: the `while` statement + its one-leaf condition.
        assert_eq!(entry_charge(&charged.main), Some(3 + 7));

        let Some(Stmt::While(w)) = charged.main.get(1) else {
            panic!("expected the while to survive the walk")
        };
        let Stmt::Block(b) = w.body.as_ref() else {
            panic!("body should have been promoted to a block")
        };
        // the promoted statement + its three expression nodes.
        assert_eq!(entry_charge(&b.stmts), Some(3 + 3 * 7));
    }
}
