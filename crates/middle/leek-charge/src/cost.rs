//! Static per-statement and per-expression cost model.

use leek_hir::{Expr, ExprKind, Stmt};

use crate::opts::ChargeOpts;

pub(crate) fn stmts_cost(stmts: &[Stmt], opts: ChargeOpts) -> u64 {
    stmts.iter().map(|s| stmt_cost(s, opts)).sum()
}

/// Per-statement static cost. We recurse into expressions; nested
/// blocks (`if`, `while`, etc.) are not counted here — they receive
/// their own block-entry charge during the recursive walk.
pub(crate) fn stmt_cost(s: &Stmt, opts: ChargeOpts) -> u64 {
    let own = opts.per_stmt;
    let exprs = match s {
        Stmt::Expr(e) => expr_cost(e, opts),
        Stmt::VarDecl(v) => v.init.as_ref().map_or(0, |e| expr_cost(e, opts)),
        Stmt::Return(e) => e.as_ref().map_or(0, |e| expr_cost(e, opts)),
        Stmt::If(i) => expr_cost(&i.cond, opts),
        Stmt::While(w) => expr_cost(&w.cond, opts),
        Stmt::DoWhile(dw) => expr_cost(&dw.cond, opts),
        Stmt::For(f) => {
            f.init.as_ref().map_or(0, |s| stmt_cost(s, opts))
                + f.cond.as_ref().map_or(0, |e| expr_cost(e, opts))
                + f.step.as_ref().map_or(0, |e| expr_cost(e, opts))
        }
        Stmt::Foreach(fe) => expr_cost(&fe.iter, opts),
        Stmt::Switch(s) => expr_cost(&s.discriminant, opts),
        Stmt::Block(_)
        | Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => 0,
    };
    own + exprs
}

/// Per-expression static cost. Dynamic input-scaled cost (for
/// builtins like `replace`) is *not* added here — those self-charge
/// inside the runtime/interpreter.
///
/// Every expression costs `per_expr` plus the cost of its
/// sub-expressions. The only non-uniform case is a ternary, where
/// just one branch runs, so the branches are `max`'d rather than
/// summed; everything else (calls included — a call's cost is its
/// receiver + arguments) is the sum of its immediate children, which
/// [`leek_hir::walk_expr_children`] enumerates. A lambda is a leaf:
/// its body is deferred and not charged here.
pub(crate) fn expr_cost(e: &Expr, opts: ChargeOpts) -> u64 {
    let own = opts.per_expr;
    let children = if let ExprKind::Ternary(c, t, f) = &e.kind {
        // Only one branch runs, so `max` the branches.
        expr_cost(c, opts) + expr_cost(t, opts).max(expr_cost(f, opts))
    } else {
        // Everything else costs the sum of its sub-expressions.
        let mut sum = 0u64;
        leek_hir::walk_expr_children(e, &mut |child| sum += expr_cost(child, opts));
        sum
    };
    own + children
}

pub(crate) fn charge_stmt_for_block_start(_stmts: &[Stmt], total: u64) -> Stmt {
    Stmt::Charge(total)
}

#[cfg(test)]
mod tests {
    //! Value-level tests for the cost model.
    //!
    //! The crate's other tests assert the *shape* of the output
    //! (`matches!(…, Stmt::Charge(_))`), which a mutation returning a
    //! constant would survive. These assert the numbers, because the number
    //! is the whole product: it becomes the op budget the native and
    //! Java-clean backends debit, so a wrong one shows up as wrong in-game op
    //! accounting rather than as a failing test.

    use leek_hir::{
        Block, Call, Callee, DefId, DoWhileStmt, Expr, ExprKind, ForStmt, ForeachBind, ForeachStmt,
        IfStmt, ImportStmt, IncludeStmt, Literal, NameRef, Stmt, SwitchArm, SwitchStmt, Type,
        VarDecl, WhileStmt,
    };
    use leek_span::Span;

    use super::{ChargeOpts, expr_cost, stmt_cost, stmts_cost};

    fn span() -> Span {
        Span::synthetic()
    }

    /// A leaf expression: no children, so its cost is exactly `per_expr`.
    fn lit(n: i64) -> Expr {
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

    fn ternary(c: Expr, t: Expr, f: Expr) -> Expr {
        Expr {
            kind: ExprKind::Ternary(Box::new(c), Box::new(t), Box::new(f)),
            ty: Type::Integer,
            span: span(),
        }
    }

    /// `f(args…)` — a `Callee::Function` carries no receiver expression, so
    /// the cost is the call node plus its arguments.
    fn call(args: Vec<Expr>) -> Expr {
        Expr {
            kind: ExprKind::Call(Box::new(Call {
                callee: Callee::Function(NameRef::Builtin("f".into())),
                args,
                callee_span: span(),
                span: span(),
            })),
            ty: Type::Integer,
            span: span(),
        }
    }

    /// `recv.m(args…)` — the receiver is a child expression too.
    fn method_call(receiver: Expr, args: Vec<Expr>) -> Expr {
        Expr {
            kind: ExprKind::Call(Box::new(Call {
                callee: Callee::Method {
                    receiver,
                    method: "m".into(),
                    optional: false,
                },
                args,
                callee_span: span(),
                span: span(),
            })),
            ty: Type::Integer,
            span: span(),
        }
    }

    const UNIT: ChargeOpts = ChargeOpts {
        per_stmt: 1,
        per_expr: 1,
    };
    /// Two distinct non-unit weights: any hard-coded `1`, and any confusion
    /// of one knob for the other, changes the total.
    const WEIGHTED: ChargeOpts = ChargeOpts {
        per_stmt: 3,
        per_expr: 7,
    };

    #[test]
    fn a_leaf_expression_costs_exactly_per_expr() {
        assert_eq!(expr_cost(&lit(1), UNIT), 1);
        assert_eq!(expr_cost(&lit(1), WEIGHTED), 7);
    }

    #[test]
    fn an_expression_costs_itself_plus_its_children() {
        // `a + b`: the binary node, plus two leaves.
        assert_eq!(expr_cost(&add(lit(1), lit(2)), UNIT), 3);
        assert_eq!(expr_cost(&add(lit(1), lit(2)), WEIGHTED), 3 * 7);
        // Nesting is plain summation: `(a + b) + c`.
        assert_eq!(expr_cost(&add(add(lit(1), lit(2)), lit(3)), UNIT), 5);
    }

    #[test]
    fn a_calls_cost_is_the_receiver_plus_the_arguments() {
        // `f(a, b)` = the call node + 2 leaf args.
        assert_eq!(expr_cost(&call(vec![lit(1), lit(2)]), UNIT), 3);
        // A bare `Callee::Function` name is not itself an expression node.
        assert_eq!(expr_cost(&call(vec![]), UNIT), 1);
        // `recv.m(a)` = the call node + the receiver + 1 arg.
        assert_eq!(expr_cost(&method_call(lit(0), vec![lit(1)]), UNIT), 3);
        // …and the receiver is charged in full, not as a leaf.
        assert_eq!(
            expr_cost(&method_call(add(lit(0), lit(1)), vec![lit(2)]), UNIT),
            5
        );
    }

    #[test]
    fn a_ternary_maxes_its_branches_instead_of_summing_them() {
        // Only one branch runs. `cond ? ((a + b) + c) : d`
        let big = add(add(lit(1), lit(2)), lit(3)); // 5
        let small = lit(4); // 1
        let cost = expr_cost(&ternary(lit(0), big.clone(), small.clone()), UNIT);
        // own(1) + cond(1) + max(5, 1)
        assert_eq!(cost, 7);
        // Summing the branches would give 8; `min`ing them would give 3.
        assert_ne!(cost, 8);
        // The branch order must not matter.
        assert_eq!(expr_cost(&ternary(lit(0), small, big), UNIT), cost);
    }

    #[test]
    fn a_lambda_is_a_leaf_so_its_body_is_deferred() {
        use leek_hir::{LambdaBody, LambdaExpr};
        let lambda = Expr {
            kind: ExprKind::Lambda(LambdaExpr {
                params: vec![],
                body: LambdaBody::Block(Block {
                    // Deliberately expensive: none of it may be counted here.
                    stmts: vec![Stmt::Return(Some(add(add(lit(1), lit(2)), lit(3))))],
                    span: span(),
                }),
            }),
            ty: Type::Function,
            span: span(),
        };
        assert_eq!(expr_cost(&lambda, UNIT), 1);
    }

    fn var_decl(init: Option<Expr>) -> Stmt {
        Stmt::VarDecl(VarDecl {
            def: DefId(0),
            name: "x".into(),
            ty: None,
            init,
            is_global: false,
            span: span(),
        })
    }

    /// One pinned cost per `Stmt` variant, with an exhaustive `match` below
    /// so a new statement kind fails to compile here rather than silently
    /// going uncounted — the same guard `walk::walk_stmt_recurse` uses.
    #[test]
    fn every_statement_variant_has_a_pinned_cost() {
        let cond = add(lit(1), lit(2)); // 3
        let body = || Box::new(Stmt::Break(span()));

        let cases: Vec<(Stmt, u64)> = vec![
            (Stmt::Expr(add(lit(1), lit(2))), 1 + 3),
            (var_decl(Some(add(lit(1), lit(2)))), 1 + 3),
            // No initialiser: the statement still costs `per_stmt`.
            (var_decl(None), 1),
            (Stmt::Return(Some(add(lit(1), lit(2)))), 1 + 3),
            (Stmt::Return(None), 1),
            (
                // The branches are not counted here — each gets its own
                // block-entry charge during the walk.
                Stmt::If(IfStmt {
                    cond: cond.clone(),
                    then_branch: body(),
                    else_branch: Some(body()),
                    soft: false,
                    span: span(),
                }),
                1 + 3,
            ),
            (
                Stmt::While(WhileStmt {
                    cond: cond.clone(),
                    body: body(),
                    span: span(),
                }),
                1 + 3,
            ),
            (
                Stmt::DoWhile(DoWhileStmt {
                    body: body(),
                    cond: cond.clone(),
                    span: span(),
                }),
                1 + 3,
            ),
            (
                // init + cond + step, each in full; body excluded.
                Stmt::For(ForStmt {
                    init: Some(Box::new(Stmt::Expr(lit(0)))), // 1 + 1
                    cond: Some(cond.clone()),                 // 3
                    step: Some(lit(9)),                       // 1
                    body: body(),
                    span: span(),
                }),
                1 + 2 + 3 + 1,
            ),
            (
                Stmt::For(ForStmt {
                    init: None,
                    cond: None,
                    step: None,
                    body: body(),
                    span: span(),
                }),
                1,
            ),
            (
                Stmt::Foreach(ForeachStmt {
                    key: None,
                    value: ForeachBind {
                        target: lit(0),
                        name: "v".into(),
                        is_new: true,
                        is_by_ref: false,
                        span: span(),
                    },
                    iter: cond.clone(),
                    body: body(),
                    span: span(),
                }),
                1 + 3,
            ),
            (
                Stmt::Switch(SwitchStmt {
                    discriminant: cond.clone(),
                    // Arm bodies are charged per arm by the walk, not here.
                    arms: vec![SwitchArm {
                        case: Some(lit(1)),
                        body: vec![Stmt::Expr(add(lit(1), lit(2)))],
                    }],
                    span: span(),
                }),
                1 + 3,
            ),
            (Stmt::Break(span()), 1),
            (Stmt::Continue(span()), 1),
            (
                Stmt::Include(IncludeStmt {
                    path: "p".into(),
                    span: span(),
                }),
                1,
            ),
            (
                Stmt::Import(ImportStmt {
                    path: "p".into(),
                    span: span(),
                }),
                1,
            ),
            (
                // A nested block's statements are paid for by that block's
                // own entry charge, so only the block statement itself is
                // counted here.
                Stmt::Block(Block {
                    stmts: vec![Stmt::Expr(add(lit(1), lit(2)))],
                    span: span(),
                }),
                1,
            ),
            // An already-inserted `Charge` is itself a statement and costs
            // `per_stmt`. That is why the pass is not idempotent — see
            // `charging_twice_inflates_the_budget` in `lib.rs`.
            (Stmt::Charge(99), 1),
        ];

        for (stmt, expected) in &cases {
            assert_eq!(
                stmt_cost(stmt, UNIT),
                *expected,
                "unexpected cost for {stmt:?}"
            );
        }

        // Exhaustiveness guard: a new `Stmt` variant makes this `match` fail
        // to compile, forcing whoever adds it to give it a cost above.
        for (stmt, _) in &cases {
            match stmt {
                Stmt::Expr(_)
                | Stmt::VarDecl(_)
                | Stmt::Return(_)
                | Stmt::If(_)
                | Stmt::While(_)
                | Stmt::DoWhile(_)
                | Stmt::For(_)
                | Stmt::Foreach(_)
                | Stmt::Break(_)
                | Stmt::Continue(_)
                | Stmt::Block(_)
                | Stmt::Switch(_)
                | Stmt::Include(_)
                | Stmt::Import(_)
                | Stmt::Charge(_) => {}
            }
        }
    }

    #[test]
    fn the_two_knobs_are_independent() {
        // `a + b;` → one statement (`per_stmt`) + three expressions.
        let s = Stmt::Expr(add(lit(1), lit(2)));
        assert_eq!(stmt_cost(&s, WEIGHTED), 3 + 3 * 7);
        // Swapping the weights must change the answer, so neither knob can be
        // silently substituted for the other.
        let swapped = ChargeOpts {
            per_stmt: 7,
            per_expr: 3,
        };
        assert_eq!(stmt_cost(&s, swapped), 7 + 3 * 3);
    }

    #[test]
    fn stmts_cost_sums_the_sequence() {
        let stmts = vec![
            Stmt::Expr(add(lit(1), lit(2))), // 4
            Stmt::Break(span()),             // 1
            Stmt::Return(None),              // 1
        ];
        assert_eq!(stmts_cost(&stmts, UNIT), 6);
        assert_eq!(stmts_cost(&[], UNIT), 0);
    }
}
