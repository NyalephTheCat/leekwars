//! Per-function CFG lowering.

use leek_diagnostics::convert;
use leek_hir::{
    BinaryOp as HBinOp, Call as HirCall, Callee as HirCallee, DefId, Expr, ExprKind, IntervalExpr,
    NameRef, PostfixOp, SliceExpr, UnaryOp as HUnOp,
};
use leek_span::Span;
use leek_types::Type;

use crate::ir::{
    BinOp, CallExpr, Callee, CastKind, Const, IntervalRvalue, LocalId, LocalKind, Operand, Place,
    Rvalue, SetElem, SliceBounds, Statement, Terminator, UnOp,
};

use super::util::{
    collect_lambda_captures_full, expr_forces_real, hir_binop_to_mir, lit_to_const,
    placeholder_function,
};
use super::{FnLowerer, PendingLambda};

impl FnLowerer<'_> {
    /// Lower an expression, returning the operand its value is
    /// available in. Side-effecting evaluation pushes statements
    /// into the current block.
    pub(crate) fn lower_expr_to_operand(&mut self, e: &Expr) -> Operand {
        match &e.kind {
            ExprKind::Literal(lit) => Operand::Const(lit_to_const(lit)),
            ExprKind::Name(n) => self.lower_name(n, &e.ty, e.span),
            ExprKind::Binary(op, l, r) => self.lower_binary(*op, l, r, &e.ty, e.span),
            ExprKind::Unary(op, x) => self.lower_unary(*op, x, &e.ty, e.span),
            ExprKind::Postfix(op, x) => self.lower_postfix(*op, x, &e.ty, e.span),
            ExprKind::Call(c) => self.lower_call(c, &e.ty),
            ExprKind::Field(base, name, optional) => {
                let base_local = self.lower_expr_to_local(base);
                let t = self.fresh_temp(e.ty.clone(), e.span);
                // `obj?.field` (#2272): null receiver short-circuits this
                // link to null instead of reading the field. `?.class` is
                // excluded like upstream (`getFieldNullSafe` is bypassed
                // for the reflective `class` access).
                if *optional && name != "class" {
                    let read_bb = self.new_block();
                    let null_bb = self.new_block();
                    let join = self.new_block();
                    let is_null = self.null_check(base_local, e.span);
                    self.set_terminator(Terminator::Branch {
                        cond: is_null,
                        then_block: null_bb,
                        else_block: read_bb,
                    });
                    self.resume(null_bb);
                    self.push_stmt(Statement::Assign(
                        Place::Local(t),
                        Rvalue::Use(Operand::Const(Const::Null)),
                    ));
                    self.goto(join);
                    self.resume(read_bb);
                    self.push_stmt(Statement::Assign(
                        Place::Local(t),
                        Rvalue::Field(base_local, name.clone()),
                    ));
                    self.goto(join);
                    self.resume(join);
                } else {
                    self.push_stmt(Statement::Assign(
                        Place::Local(t),
                        Rvalue::Field(base_local, name.clone()),
                    ));
                }
                Operand::Local(t)
            }
            ExprKind::Index(base, idx) => {
                let base_local = self.lower_expr_to_local(base);
                let idx_op = self.lower_expr_to_operand(idx);
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::Index(base_local, idx_op),
                ));
                Operand::Local(t)
            }
            ExprKind::Slice(s) => {
                let base_local = self.lower_expr_to_local(&s.base);
                let bounds = self.lower_slice_bounds(s);
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::Slice(base_local, bounds),
                ));
                Operand::Local(t)
            }
            ExprKind::Array(items) => {
                let ops = items
                    .iter()
                    .map(|x| self.lower_expr_to_operand(x))
                    .collect();
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(Place::Local(t), Rvalue::Array(ops)));
                Operand::Local(t)
            }
            ExprKind::Map(pairs) => {
                let kv = pairs
                    .iter()
                    .map(|(k, v)| (self.lower_expr_to_operand(k), self.lower_expr_to_operand(v)))
                    .collect();
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(Place::Local(t), Rvalue::Map(kv)));
                Operand::Local(t)
            }
            ExprKind::Set(items) => {
                let ops = items
                    .iter()
                    .map(|x| match &x.end {
                        Some(end) => SetElem::Range(
                            self.lower_expr_to_operand(&x.start),
                            self.lower_expr_to_operand(end),
                        ),
                        None => SetElem::One(self.lower_expr_to_operand(&x.start)),
                    })
                    .collect();
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(Place::Local(t), Rvalue::Set(ops)));
                Operand::Local(t)
            }
            ExprKind::Object(pairs) => {
                let kv = pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), self.lower_expr_to_operand(v)))
                    .collect();
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(Place::Local(t), Rvalue::Object(kv)));
                Operand::Local(t)
            }
            ExprKind::Ternary(c, then_e, else_e) => {
                self.lower_ternary(c, then_e, else_e, &e.ty, e.span)
            }
            ExprKind::Interval(iv) => self.lower_interval(iv, &e.ty, e.span),
            ExprKind::Cast(inner, _ty) => {
                let v = self.lower_expr_to_operand(inner);
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::Cast(CastKind::User, v),
                ));
                Operand::Local(t)
            }
            ExprKind::New(n) => {
                let args = n
                    .args
                    .iter()
                    .map(|a| self.lower_expr_to_operand(a))
                    .collect();
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::New {
                        class: n.class.clone(),
                        args,
                    },
                ));
                Operand::Local(t)
            }
            ExprKind::Lambda(lam) => self.lower_lambda(lam, &e.ty, e.span),
        }
    }

    /// Reserve a function slot, register a [`PendingLambda`], and
    /// emit an [`Rvalue::MakeLambda`]. Captures are pre-discovered
    /// from the lambda's HIR so the MakeLambda can carry them as
    /// flat operands resolved against the enclosing frame.
    pub(crate) fn lower_lambda(
        &mut self,
        lam: &leek_hir::LambdaExpr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        let (captures, needs_this) = collect_lambda_captures_full(lam);
        // Materialise each captured DefId as an operand. If a
        // capture isn't in the parent's local_map, we surface a
        // LowerError and pass null so the lambda body can still
        // run (it'll likely produce wrong results, but won't
        // crash the lowerer).
        let mut cap_operands: Vec<Operand> = Vec::with_capacity(captures.len() + 1);
        // When inside a method body and the lambda references
        // `this`/`super`/`Class_` (directly or via a field name),
        // capture the outer `this` implicitly as the first slot.
        // The pending-lambda step then rebuilds a `MethodCtx`
        // pointing at that slot so the lambda body's
        // `NameRef::This` resolves correctly.
        let inherit_method_ctx = self.method_ctx.clone();
        let implicit_this = needs_this
            .then(|| inherit_method_ctx.as_ref().and_then(|m| m.this_local))
            .flatten();
        if let Some(this_local) = implicit_this {
            cap_operands.push(Operand::Local(this_local));
        }
        for cap in &captures {
            if let Some(id) = self.local_map.get(cap).copied() {
                // Mark the outer-scope local as shared so the
                // interpreter wraps it in a `Value::Cell` at
                // frame init. Closures then share the cell with
                // the outer scope and mutations propagate.
                self.locals[id.0 as usize].is_shared = true;
                cap_operands.push(Operand::Local(id));
            } else {
                self.errors.push(convert::lowering_unsupported(
                    span,
                    format!("lambda captures unbound local {cap}; passing null instead"),
                ));
                cap_operands.push(Operand::Const(Const::Null));
            }
        }
        // Reserve a function slot. The placeholder will be
        // overwritten by `ProgramCtx::lower_pending_lambda` later.
        let function_idx = self.program_functions.len();
        self.program_functions.push(placeholder_function(span));
        self.pending_lambdas.push(PendingLambda {
            function_idx,
            lambda: lam.clone(),
            captures,
            method_ctx: if implicit_this.is_some() {
                inherit_method_ctx
            } else {
                None
            },
            needs_this: implicit_this.is_some(),
            span,
        });
        let t = self.fresh_temp(ty.clone(), span);
        self.push_stmt(Statement::Assign(
            Place::Local(t),
            Rvalue::MakeLambda {
                function_idx,
                captures: cap_operands,
            },
        ));
        Operand::Local(t)
    }

    /// Convenience: lower an expression, then ensure its value is
    /// in a local (materializing a temp if the operand is a
    /// constant). Used at sites where the consumer requires a
    /// `LocalId` — e.g. as a `Place::Field` base.
    pub(crate) fn lower_expr_to_local(&mut self, e: &Expr) -> LocalId {
        match self.lower_expr_to_operand(e) {
            Operand::Local(id) => id,
            Operand::Const(c) => {
                let t = self.fresh_temp(e.ty.clone(), e.span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::Use(Operand::Const(c)),
                ));
                t
            }
        }
    }

    pub(crate) fn lower_name(&mut self, n: &NameRef, ty: &Type, span: Span) -> Operand {
        match n {
            NameRef::Local(def) => {
                if let Some(id) = self.local_map.get(def).copied() {
                    Operand::Local(id)
                } else {
                    self.errors.push(convert::lowering_unsupported(
                        span,
                        format!("MIR lowering saw an unbound local {def}"),
                    ));
                    let t = self.fresh_temp(ty.clone(), span);
                    self.push_stmt(Statement::Assign(
                        Place::Local(t),
                        Rvalue::Unsupported("unbound-local"),
                    ));
                    Operand::Local(t)
                }
            }
            NameRef::Global(def) => {
                let name = self.globals.get(def).cloned().unwrap_or_else(|| "?".into());
                let t = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::GlobalRef(*def, name),
                ));
                Operand::Local(t)
            }
            NameRef::Function(def) => {
                let t = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::FunctionRef(*def),
                ));
                Operand::Local(t)
            }
            NameRef::Class(def) => {
                let class_name = match self.hir.defs.get(def.0 as usize) {
                    Some(leek_hir::Def::Class(c)) => c.name.clone(),
                    _ => "?".into(),
                };
                let t = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::ClassRef(*def, class_name),
                ));
                Operand::Local(t)
            }
            NameRef::Builtin(name) => {
                let t = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::BuiltinRef(name.clone()),
                ));
                Operand::Local(t)
            }
            NameRef::This => match self.method_ctx.as_ref().and_then(|m| m.this_local) {
                Some(id) => Operand::Local(id),
                None => Operand::Const(Const::Null),
            },
            NameRef::Super => {
                let Some(ctx) = self.method_ctx.clone() else {
                    return self.materialize(
                        Rvalue::Unsupported("super-outside-method"),
                        ty.clone(),
                        span,
                    );
                };
                let Some(this_local) = ctx.this_local else {
                    return self.materialize(
                        Rvalue::Unsupported("super-in-static"),
                        ty.clone(),
                        span,
                    );
                };
                let Some(parent) = ctx.parent_class else {
                    // `super` in a class with no parent is meaningless; use null.
                    return Operand::Const(Const::Null);
                };
                self.materialize(
                    Rvalue::MakeSuper {
                        this: this_local,
                        parent_class: parent,
                    },
                    ty.clone(),
                    span,
                )
            }
            NameRef::Class_ => match self.method_ctx.as_ref() {
                Some(ctx) => self.materialize(
                    Rvalue::ClassRef(ctx.class_def_id, ctx.class_name.clone()),
                    ty.clone(),
                    span,
                ),
                None => Operand::Const(Const::Null),
            },
            NameRef::Unresolved(_) => {
                let t = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::Use(Operand::Const(Const::Null)),
                ));
                Operand::Local(t)
            }
        }
    }

    pub(crate) fn materialize(&mut self, rv: Rvalue, ty: Type, span: Span) -> Operand {
        let t = self.fresh_temp(ty, span);
        self.push_stmt(Statement::Assign(Place::Local(t), rv));
        Operand::Local(t)
    }

    pub(crate) fn lower_binary(
        &mut self,
        op: HBinOp,
        l: &Expr,
        r: &Expr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        if op.is_assignment() {
            return self.lower_assignment(op, l, r, ty, span);
        }
        match op {
            HBinOp::And => self.lower_short_circuit_and(l, r, ty, span),
            HBinOp::Or => self.lower_short_circuit_or(l, r, ty, span),
            HBinOp::NullCoalesce => self.lower_null_coalesce(l, r, ty, span),
            _ => {
                let lop = self.lower_expr_to_operand(l);
                let rop = self.lower_expr_to_operand(r);
                let mop = hir_binop_to_mir(op).expect("non-short-circuit, non-assignment binop");
                let t = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::Binary(mop, lop, rop),
                ));
                Operand::Local(t)
            }
        }
    }

    pub(crate) fn lower_short_circuit_and(
        &mut self,
        l: &Expr,
        r: &Expr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        // Re-associate `(A and B) and C` → `A and (B and C)` for
        // the same reason as `or` above (see `lower_short_circuit_or`).
        if let leek_hir::ExprKind::Binary(HBinOp::And, ll, lr) = &l.kind {
            let lr_expr = (**lr).clone();
            let r_expr = r.clone();
            let lr_span = lr_expr.span;
            let new_r_kind =
                leek_hir::ExprKind::Binary(HBinOp::And, Box::new(lr_expr), Box::new(r_expr));
            let new_r = leek_hir::Expr {
                kind: new_r_kind,
                ty: ty.clone(),
                span: lr_span,
            };
            return self.lower_short_circuit_and(ll, &new_r, ty, span);
        }
        let result = self.fresh_temp(ty.clone(), span);
        let l_val = self.lower_expr_to_operand(l);
        let rhs_bb = self.new_block();
        let false_bb = self.new_block();
        let join = self.new_block();
        self.set_terminator(Terminator::Branch {
            cond: l_val,
            then_block: rhs_bb,
            else_block: false_bb,
        });

        self.resume(rhs_bb);
        let r_val = self.lower_expr_to_operand(r);
        self.push_stmt(Statement::Assign(Place::Local(result), Rvalue::Use(r_val)));
        self.goto(join);

        self.resume(false_bb);
        self.push_stmt(Statement::Assign(
            Place::Local(result),
            Rvalue::Use(Operand::Const(Const::Bool(false))),
        ));
        self.goto(join);

        self.resume(join);
        Operand::Local(result)
    }

    pub(crate) fn lower_short_circuit_or(
        &mut self,
        l: &Expr,
        r: &Expr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        // Re-associate `(A or B) or C` to `A or (B or C)` at MIR
        // lowering so chained `or` evaluates as a single short-
        // circuit cascade — matches upstream's per-op cost model
        // (only the outermost branch charges in a chain). Without
        // this, a left-associative tree fires one Branch per `or`,
        // over-counting by N-1 ops on N-deep chains.
        if let leek_hir::ExprKind::Binary(HBinOp::Or, ll, lr) = &l.kind {
            let lr_expr = (**lr).clone();
            let r_expr = r.clone();
            let lr_span = lr_expr.span;
            let new_r_kind =
                leek_hir::ExprKind::Binary(HBinOp::Or, Box::new(lr_expr), Box::new(r_expr));
            let new_r = leek_hir::Expr {
                kind: new_r_kind,
                ty: ty.clone(),
                span: lr_span,
            };
            return self.lower_short_circuit_or(ll, &new_r, ty, span);
        }
        let result = self.fresh_temp(ty.clone(), span);
        let l_val = self.lower_expr_to_operand(l);
        let true_bb = self.new_block();
        let rhs_bb = self.new_block();
        let join = self.new_block();
        self.set_terminator(Terminator::Branch {
            cond: l_val,
            then_block: true_bb,
            else_block: rhs_bb,
        });

        self.resume(true_bb);
        self.push_stmt(Statement::Assign(
            Place::Local(result),
            Rvalue::Use(Operand::Const(Const::Bool(true))),
        ));
        self.goto(join);

        self.resume(rhs_bb);
        let r_val = self.lower_expr_to_operand(r);
        self.push_stmt(Statement::Assign(Place::Local(result), Rvalue::Use(r_val)));
        self.goto(join);

        self.resume(join);
        Operand::Local(result)
    }

    pub(crate) fn lower_null_coalesce(
        &mut self,
        l: &Expr,
        r: &Expr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        let result = self.fresh_temp(ty.clone(), span);
        let l_val = self.lower_expr_to_operand(l);
        // Initialize result with the lhs so the non-null arm is a
        // no-op. We then branch on lhs === null.
        self.push_stmt(Statement::Assign(
            Place::Local(result),
            Rvalue::Use(l_val.clone()),
        ));
        let is_null = self.fresh_temp(Type::Boolean, span);
        self.push_stmt(Statement::Assign(
            Place::Local(is_null),
            Rvalue::Binary(BinOp::IdentityEq, l_val, Operand::Const(Const::Null)),
        ));
        let rhs_bb = self.new_block();
        let join = self.new_block();
        self.set_terminator(Terminator::Branch {
            cond: Operand::Local(is_null),
            then_block: rhs_bb,
            else_block: join,
        });

        self.resume(rhs_bb);
        let r_val = self.lower_expr_to_operand(r);
        self.push_stmt(Statement::Assign(Place::Local(result), Rvalue::Use(r_val)));
        self.goto(join);

        self.resume(join);
        Operand::Local(result)
    }

    pub(crate) fn lower_assignment(
        &mut self,
        op: HBinOp,
        lhs: &Expr,
        rhs: &Expr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        // For compound assigns we first read the old value, apply
        // the base op, then write back. Plain `=` skips the read.
        let place = self.lower_place(lhs);
        // For nested index assignments (`a[i][j] = v`), collect
        // the chain of (outer_base, outer_idx, intermediate_local)
        // so we can emit a write-back after the inner mutation.
        // This propagates v1-v3 LegacyArray array→map promotion
        // up through every level of indirection (set_index in
        // the interp returns the morphed value when promotion
        // happens, and the explicit write-back makes sure each
        // outer slot ends up holding the new container).
        let writeback_chain = self.collect_index_writeback_chain(lhs, &place);
        let new_value = if let Some(base_op) = op.compound_base() {
            if base_op == HBinOp::NullCoalesce {
                // `a ??= b` desugars into `a = a ?? b`. The short-
                // circuit lowering is reused.
                let old = self.read_place(&place, ty, span);
                self.lower_null_coalesce_with_initial(old, rhs, ty, span)
            } else {
                let old = self.read_place(&place, ty, span);
                let rhs_v = self.lower_expr_to_operand(rhs);
                // `^=` is the only compound op whose desugar
                // semantics differ from the standalone operator
                // (v1 means POW-assign, v2+ means XOR-assign).
                // Lower to the dedicated MIR op so the interp
                // can dispatch on version at runtime.
                let mop = if matches!(base_op, HBinOp::BitXor) {
                    BinOp::CompoundXor
                } else {
                    hir_binop_to_mir(base_op).expect("compound base op")
                };
                let t = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::Binary(mop, old, rhs_v),
                ));
                Operand::Local(t)
            }
        } else {
            self.lower_expr_to_operand(rhs)
        };
        // Read back from the place after the assign so that
        // - failed writes (assignment to a null base,
        //   out-of-bounds array store in v4) yield `null` instead
        //   of the candidate RHS;
        // - typed locals get the coerced value (`integer a = 100;
        //   return a /= 5` returns `20`, not `20.0`).
        self.push_stmt(Statement::Assign(place.clone(), Rvalue::Use(new_value)));
        // After the main write, propagate any v1-v3 LegacyArray
        // promotion up the index chain — `tabmulti[i][j] = v` may
        // morph `tabmulti[i]` from an Array into a sparse Map, and
        // without explicit write-back the outer slot keeps a stale
        // empty array. For non-promotion writes the chain is a
        // no-op (every intermediate `Rc` is unchanged).
        for (outer_base, outer_idx, intermediate) in writeback_chain {
            self.push_stmt(Statement::Assign(
                Place::Index(outer_base, outer_idx),
                Rvalue::Use(Operand::Local(intermediate)),
            ));
        }
        self.read_place(&place, ty, span)
    }

    /// Walk the LHS expression's index chain — from innermost to
    /// outermost — returning `(outer_base, outer_idx,
    /// intermediate_local)` triples for every nested level. The
    /// `intermediate_local` is the same LocalId that `lower_place`
    /// already allocated when reading each inner value; we just
    /// re-discover the chain shape so we can emit the explicit
    /// write-back statements.
    pub(crate) fn collect_index_writeback_chain(
        &mut self,
        lhs: &Expr,
        place: &Place,
    ) -> Vec<(LocalId, Operand, LocalId)> {
        let mut chain: Vec<(LocalId, Operand, LocalId)> = Vec::new();
        let Place::Index(inner_local, _) = place else {
            return chain;
        };
        let ExprKind::Index(inner_lhs, _) = &lhs.kind else {
            return chain;
        };
        let mut cur_lhs: &Expr = inner_lhs;
        let mut cur_local: LocalId = *inner_local;
        while let ExprKind::Index(outer_base, outer_idx) = &cur_lhs.kind {
            let outer_local = self.lower_expr_to_local(outer_base);
            let outer_idx_op = self.lower_expr_to_operand(outer_idx);
            chain.push((outer_local, outer_idx_op, cur_local));
            cur_lhs = outer_base;
            cur_local = outer_local;
        }
        chain
    }

    /// `a ??= b` reuses the same shape as `a ?? b` but with the
    /// already-read lhs.
    pub(crate) fn lower_null_coalesce_with_initial(
        &mut self,
        lhs: Operand,
        rhs: &Expr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        let result = self.fresh_temp(ty.clone(), span);
        self.push_stmt(Statement::Assign(
            Place::Local(result),
            Rvalue::Use(lhs.clone()),
        ));
        let is_null = self.fresh_temp(Type::Boolean, span);
        self.push_stmt(Statement::Assign(
            Place::Local(is_null),
            Rvalue::Binary(BinOp::IdentityEq, lhs, Operand::Const(Const::Null)),
        ));
        let rhs_bb = self.new_block();
        let join = self.new_block();
        self.set_terminator(Terminator::Branch {
            cond: Operand::Local(is_null),
            then_block: rhs_bb,
            else_block: join,
        });
        self.resume(rhs_bb);
        let r_val = self.lower_expr_to_operand(rhs);
        self.push_stmt(Statement::Assign(Place::Local(result), Rvalue::Use(r_val)));
        self.goto(join);
        self.resume(join);
        Operand::Local(result)
    }

    pub(crate) fn read_place(&mut self, place: &Place, ty: &Type, span: Span) -> Operand {
        let rv = match place.clone() {
            Place::Local(id) => return Operand::Local(id),
            Place::Global(def, name) => Rvalue::GlobalRef(def, name),
            Place::Field(base, name) => Rvalue::Field(base, name),
            Place::Index(base, idx) => Rvalue::Index(base, idx),
            Place::Slice(base, bounds) => Rvalue::Slice(base, bounds),
            Place::LambdaCapture { .. } => {
                // LambdaCapture is write-only — never appears as a
                // read-modify-write target. Return null defensively
                // if someone wires it in by mistake.
                Rvalue::Use(Operand::Const(Const::Null))
            }
        };
        let t = self.fresh_temp(ty.clone(), span);
        self.push_stmt(Statement::Assign(Place::Local(t), rv));
        Operand::Local(t)
    }

    /// Lower an l-value expression into a [`Place`]. Side-effecting
    /// sub-expressions (the base of a field access, the index of an
    /// index access) get pushed into the current block as temp
    /// assignments first.
    pub(crate) fn lower_place(&mut self, e: &Expr) -> Place {
        match &e.kind {
            ExprKind::Name(NameRef::Local(def)) => {
                if let Some(id) = self.local_map.get(def).copied() {
                    Place::Local(id)
                } else {
                    self.errors.push(convert::lowering_unsupported(
                        e.span,
                        format!("assignment to unbound local {def}"),
                    ));
                    let t = self.fresh_temp(e.ty.clone(), e.span);
                    Place::Local(t)
                }
            }
            ExprKind::Name(NameRef::Global(def)) => {
                let name = self.globals.get(def).cloned().unwrap_or_else(|| "?".into());
                Place::Global(*def, name)
            }
            // Assignments to a builtin / function / class name
            // (`abs = 2`) shadow the stdlib binding with a
            // name-keyed global. The interpreter's name-keyed
            // global store does the right thing on read too —
            // `BuiltinRef` / `FunctionRef` / `ClassRef` check
            // it first before falling back to the canonical
            // stdlib value.
            ExprKind::Name(NameRef::Builtin(name)) => Place::Global(DefId(0), name.clone()),
            ExprKind::Name(NameRef::Function(def)) => {
                let name = self
                    .hir
                    .defs
                    .get(def.0 as usize)
                    .map_or_else(|| "?".into(), |d| d.name().to_string());
                Place::Global(*def, name)
            }
            ExprKind::Name(NameRef::Class(def)) => {
                let name = self
                    .hir
                    .defs
                    .get(def.0 as usize)
                    .map_or_else(|| "?".into(), |d| d.name().to_string());
                Place::Global(*def, name)
            }
            // Optional access is never an l-value (upstream sets
            // `isLeftValue = false` on the `?.` form) — the flag is
            // ignored here and the write behaves like a plain access.
            ExprKind::Field(base, name, _) => {
                let base_local = self.lower_expr_to_local(base);
                Place::Field(base_local, name.clone())
            }
            ExprKind::Index(base, idx) => {
                let base_local = self.lower_expr_to_local(base);
                let idx_op = self.lower_expr_to_operand(idx);
                Place::Index(base_local, idx_op)
            }
            ExprKind::Slice(s) => {
                let base_local = self.lower_expr_to_local(&s.base);
                let bounds = self.lower_slice_bounds(s);
                Place::Slice(base_local, bounds)
            }
            _ => {
                self.errors.push(convert::lowering_unsupported(
                    e.span,
                    "assignment target is not an l-value MIR knows how to model",
                ));
                let t = self.fresh_temp(e.ty.clone(), e.span);
                Place::Local(t)
            }
        }
    }

    pub(crate) fn lower_slice_bounds(&mut self, s: &SliceExpr) -> SliceBounds {
        SliceBounds {
            start: s.start.as_ref().map(|e| self.lower_expr_to_operand(e)),
            end: s.end.as_ref().map(|e| self.lower_expr_to_operand(e)),
            step: s.step.as_ref().map(|e| self.lower_expr_to_operand(e)),
        }
    }

    pub(crate) fn lower_unary(&mut self, op: HUnOp, x: &Expr, ty: &Type, span: Span) -> Operand {
        match op {
            HUnOp::PreInc | HUnOp::PreDec => {
                let base_op = if matches!(op, HUnOp::PreInc) {
                    HBinOp::Add
                } else {
                    HBinOp::Sub
                };
                let place = self.lower_place(x);
                let old = self.read_place(&place, ty, span);
                let mop = hir_binop_to_mir(base_op).unwrap();
                let new_val = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(new_val),
                    Rvalue::Binary(mop, old, Operand::Const(Const::Int(1))),
                ));
                self.push_stmt(Statement::Assign(
                    place,
                    Rvalue::Use(Operand::Local(new_val)),
                ));
                Operand::Local(new_val)
            }
            // `@x` on a local mirrors `is_by_ref` at the
            // expression level — mark the local as shared so its
            // slot becomes a `Value::Cell`, then return the
            // operand unchanged (the consumer's
            // `read_operand_raw` path preserves the cell). The
            // `is_by_ref` flag also tags the local so a
            // `return @x` terminator preserves the cell across
            // the call boundary (without it the return path
            // would unbox and we'd lose the alias).
            HUnOp::Ref => {
                if let ExprKind::Name(NameRef::Local(def)) = &x.kind
                    && let Some(id) = self.local_map.get(def).copied()
                {
                    // `@x` shares the slot via a `Value::Cell` so the
                    // reference aliases it. `is_by_ref` means
                    // "declared `@param`" at the call site, where it
                    // skips v1's by-value clone — so a body-level
                    // `@a` on a *real* call param (`function(a){ t=@a }`)
                    // must NOT set it, or v1 args stop being copied.
                    // Lambda capture slots are also `Param`-kind but
                    // the call site skips them, so they (and ordinary
                    // locals) still get `is_by_ref` — `return @x`
                    // relies on it to preserve the alias cell.
                    self.locals[id.0 as usize].is_shared = true;
                    let capture_count = self.captures.len();
                    let is_real_param = self.locals[id.0 as usize].kind == LocalKind::Param
                        && self.params.iter().skip(capture_count).any(|&p| p == id);
                    if !is_real_param {
                        self.locals[id.0 as usize].is_by_ref = true;
                    }
                    return Operand::Local(id);
                }
                // Non-local `@expr` — fall back to identity (no
                // shared reference available).
                self.lower_expr_to_operand(x)
            }
            _ => {
                let v = self.lower_expr_to_operand(x);
                let mop = match op {
                    HUnOp::Neg => UnOp::Neg,
                    HUnOp::Pos => UnOp::Pos,
                    HUnOp::Not => UnOp::Not,
                    HUnOp::BitNot => UnOp::BitNot,
                    HUnOp::Ref => UnOp::Ref,
                    HUnOp::PreInc | HUnOp::PreDec => unreachable!(),
                };
                let t = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(Place::Local(t), Rvalue::Unary(mop, v)));
                Operand::Local(t)
            }
        }
    }

    pub(crate) fn lower_postfix(
        &mut self,
        op: PostfixOp,
        x: &Expr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        match op {
            PostfixOp::PostInc | PostfixOp::PostDec => {
                let base_op = if matches!(op, PostfixOp::PostInc) {
                    HBinOp::Add
                } else {
                    HBinOp::Sub
                };
                let place = self.lower_place(x);
                let old = self.read_place(&place, ty, span);
                // Save the old value so the expression evaluates
                // to it, matching `x++`'s semantics.
                let saved = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(saved),
                    Rvalue::Use(old.clone()),
                ));
                let mop = hir_binop_to_mir(base_op).unwrap();
                let new_val = self.fresh_temp(ty.clone(), span);
                self.push_stmt(Statement::Assign(
                    Place::Local(new_val),
                    Rvalue::Binary(mop, old, Operand::Const(Const::Int(1))),
                ));
                self.push_stmt(Statement::Assign(
                    place,
                    Rvalue::Use(Operand::Local(new_val)),
                ));
                Operand::Local(saved)
            }
            PostfixOp::NonNull => {
                // `x!` is a non-null assertion. Treat as a use; the
                // type system / runtime catches the actual check.
                self.lower_expr_to_operand(x)
            }
        }
    }

    pub(crate) fn lower_call(&mut self, call: &HirCall, ty: &Type) -> Operand {
        // Set for `obj?.m(args)` (#2272): the receiver local to
        // null-check after the arguments are evaluated (upstream's
        // `callObjectAccessNullSafe` receives the args eagerly, so a
        // null receiver still evaluates them — only dispatch is
        // skipped).
        let mut optional_recv = None;
        let callee = match &call.callee {
            HirCallee::Function(name) => match name {
                NameRef::Function(def) => Callee::Function(*def),
                NameRef::Builtin(n) => Callee::Builtin(n.clone()),
                NameRef::Local(def) => match self.local_map.get(def).copied() {
                    Some(id) => Callee::Indirect(id),
                    None => Callee::Builtin("?".into()),
                },
                NameRef::Class(def) => {
                    // `MyClass(args)` == `new MyClass(args)`.
                    let class_name = match self.hir.defs.get(def.0 as usize) {
                        Some(leek_hir::Def::Class(c)) => c.name.clone(),
                        _ => "?".into(),
                    };
                    let args = call
                        .args
                        .iter()
                        .map(|a| self.lower_expr_to_operand(a))
                        .collect();
                    let t = self.fresh_temp(ty.clone(), call.span);
                    self.push_stmt(Statement::Assign(
                        Place::Local(t),
                        Rvalue::New {
                            class: class_name,
                            args,
                        },
                    ));
                    return Operand::Local(t);
                }
                NameRef::Super => {
                    // `super(args)` from a subclass constructor.
                    // Look up the parent class + this from the
                    // current method context and emit a dedicated
                    // SuperConstructor callee.
                    if let Some(ctx) = self.method_ctx.clone()
                        && let (Some(this_local), Some(parent)) =
                            (ctx.this_local, ctx.parent_class.clone())
                    {
                        let args = call
                            .args
                            .iter()
                            .map(|a| self.lower_expr_to_operand(a))
                            .collect();
                        let t = self.fresh_temp(ty.clone(), call.span);
                        self.push_stmt(Statement::Call {
                            dest: Some(Place::Local(t)),
                            call: CallExpr {
                                callee: Callee::SuperConstructor {
                                    this: this_local,
                                    parent_class: parent,
                                },
                                args,
                                span: call.span,
                            },
                        });
                        return Operand::Local(t);
                    }
                    return Operand::Const(Const::Null);
                }
                NameRef::This | NameRef::Class_ => {
                    // `this(...)` / `class(...)` not modelled yet.
                    self.errors.push(convert::lowering_unsupported(
                        call.span,
                        "this/class constructor-chain call not yet lowered",
                    ));
                    let t = self.fresh_temp(ty.clone(), call.span);
                    self.push_stmt(Statement::Assign(
                        Place::Local(t),
                        Rvalue::Unsupported("this-call"),
                    ));
                    return Operand::Local(t);
                }
                NameRef::Unresolved(_) => Callee::Builtin("?".into()),
                NameRef::Global(_) => {
                    // Calling a global value: read it, then call it
                    // indirectly.
                    let cv = self.lower_name(name, ty, call.span);
                    let local = match cv {
                        Operand::Local(id) => id,
                        Operand::Const(c) => {
                            let t = self.fresh_temp(ty.clone(), call.span);
                            self.push_stmt(Statement::Assign(
                                Place::Local(t),
                                Rvalue::Use(Operand::Const(c)),
                            ));
                            t
                        }
                    };
                    Callee::Indirect(local)
                }
            },
            HirCallee::Method {
                receiver,
                method,
                optional,
            } => {
                let recv = self.lower_expr_to_local(receiver);
                if *optional {
                    optional_recv = Some(recv);
                }
                Callee::Method {
                    receiver: recv,
                    method: method.clone(),
                }
            }
            HirCallee::Expr(e) => {
                let local = self.lower_expr_to_local(e);
                Callee::Indirect(local)
            }
        };
        let args: Vec<Operand> = call
            .args
            .iter()
            .map(|a| self.lower_expr_to_operand(a))
            .collect();
        // Builtins that may morph their first arg's container
        // (`removeElement`, `assocReverse`, `assocSort`, … on a
        // v1-v3 `LegacyArray` — an array gets promoted to a sparse
        // map). The interp signals promotion through a thread-local
        // side-channel; the post-call statement here applies it to
        // the caller's slot when needed. Without this, mutations
        // like `assocReverse(a)` lose the promoted-map shape.
        let writeback_arg0 = matches!(
            &callee,
            Callee::Builtin(name)
                if matches!(
                    name.as_str(),
                    "removeElement"
                        | "arrayRemoveElement"
                        | "assocReverse"
                        | "assocSort"
                        | "keySort"
                )
        )
        .then(|| match args.first() {
            Some(Operand::Local(id)) => Some(*id),
            _ => None,
        })
        .flatten();
        let t = self.fresh_temp(ty.clone(), call.span);
        let emit_call = |this: &mut Self| {
            this.push_stmt(Statement::Call {
                dest: Some(Place::Local(t)),
                call: CallExpr {
                    callee,
                    args,
                    span: call.span,
                },
            });
            if let Some(arg0) = writeback_arg0 {
                this.push_stmt(Statement::ApplyPromotion(arg0));
            }
        };
        if let Some(recv) = optional_recv {
            // `obj?.m(args)`: null receiver yields null, no dispatch.
            let call_bb = self.new_block();
            let null_bb = self.new_block();
            let join = self.new_block();
            let is_null = self.null_check(recv, call.span);
            self.set_terminator(Terminator::Branch {
                cond: is_null,
                then_block: null_bb,
                else_block: call_bb,
            });
            self.resume(null_bb);
            self.push_stmt(Statement::Assign(
                Place::Local(t),
                Rvalue::Use(Operand::Const(Const::Null)),
            ));
            self.goto(join);
            self.resume(call_bb);
            emit_call(self);
            self.goto(join);
            self.resume(join);
        } else {
            emit_call(self);
        }
        Operand::Local(t)
    }

    /// `local === null` as a fresh boolean temp — the receiver guard
    /// for an optional-chaining link (`?.`). Identity comparison so no
    /// value coercion is involved, matching upstream's Java-level
    /// `value == null` reference check.
    pub(crate) fn null_check(&mut self, local: LocalId, span: Span) -> Operand {
        let t = self.fresh_temp(Type::Boolean, span);
        self.push_stmt(Statement::Assign(
            Place::Local(t),
            Rvalue::Binary(
                BinOp::IdentityEq,
                Operand::Local(local),
                Operand::Const(Const::Null),
            ),
        ));
        Operand::Local(t)
    }

    pub(crate) fn lower_ternary(
        &mut self,
        cond: &Expr,
        then_e: &Expr,
        else_e: &Expr,
        ty: &Type,
        span: Span,
    ) -> Operand {
        let result = self.fresh_temp(ty.clone(), span);
        let c = self.lower_expr_to_operand(cond);
        let then_bb = self.new_block();
        let else_bb = self.new_block();
        let join = self.new_block();
        self.set_terminator(Terminator::Branch {
            cond: c,
            then_block: then_bb,
            else_block: else_bb,
        });

        self.resume(then_bb);
        let tv = self.lower_expr_to_operand(then_e);
        self.push_stmt(Statement::Assign(Place::Local(result), Rvalue::Use(tv)));
        self.goto(join);

        self.resume(else_bb);
        let ev = self.lower_expr_to_operand(else_e);
        self.push_stmt(Statement::Assign(Place::Local(result), Rvalue::Use(ev)));
        self.goto(join);

        self.resume(join);
        Operand::Local(result)
    }

    pub(crate) fn lower_interval(&mut self, iv: &IntervalExpr, ty: &Type, span: Span) -> Operand {
        // `Infinity` (the builtin name) reaches the interval as a
        // `NameRef::Builtin("Infinity")`; `∞` is a `Real` literal.
        // The two evaluate to the same `Real(±inf)` but format
        // differently for the other bound (`Infinity` widens it to
        // real). Note the source shape before lowering loses that
        // distinction.
        let start_forces_real = iv.start.as_ref().is_some_and(|e| expr_forces_real(e));
        let end_forces_real = iv.end.as_ref().is_some_and(|e| expr_forces_real(e));
        let start = iv.start.as_ref().map(|e| self.lower_expr_to_operand(e));
        let end = iv.end.as_ref().map(|e| self.lower_expr_to_operand(e));
        let step = iv.step.as_ref().map(|e| self.lower_expr_to_operand(e));
        let t = self.fresh_temp(ty.clone(), span);
        self.push_stmt(Statement::Assign(
            Place::Local(t),
            Rvalue::Interval(IntervalRvalue {
                start,
                end,
                step,
                start_inclusive: iv.start_inclusive,
                end_inclusive: iv.end_inclusive,
                start_forces_real,
                end_forces_real,
            }),
        ));
        Operand::Local(t)
    }
}
