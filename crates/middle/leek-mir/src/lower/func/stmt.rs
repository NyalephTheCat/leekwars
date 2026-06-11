//! Per-function CFG lowering.

use leek_diagnostics::convert;
use leek_hir::{
    Callee as HirCallee, DoWhileStmt, Expr, ExprKind, ForStmt, ForeachStmt, IfStmt, NameRef, Stmt,
    SwitchStmt, VarDecl, WhileStmt,
};
use leek_types::Type;

use crate::ir::{BinOp, BlockId, Const, LocalKind, Operand, Place, Rvalue, Statement, Terminator};

use super::util::{collect_lambda_captures, infer_simple_init_ty};
use super::{FnLowerer, LoopCtx};

/// Default value for a typed declaration with no initializer.
/// Container types start empty, scalars at their zero value. `Any`,
/// `Nullable`, class-instance, function, and interval types stay null
/// (`None`) — matching upstream's "typed slots are never null" rule
/// while leaving genuinely-optional slots null.
/// True when `e` is a call to a builtin free function. A builtin's
/// result is a freshly-produced value, so a v1 `var x = builtin(...)`
/// assignment must not clone it (see [`Rvalue::UseFresh`]). User
/// functions may return an existing reference, so they still clone.
fn init_is_fresh_builtin_call(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Call(c) if matches!(&c.callee, HirCallee::Function(NameRef::Builtin(_)))
    )
}

fn default_rvalue_for_type(ty: &Type) -> Option<Rvalue> {
    Some(match ty {
        Type::Map(_, _) => Rvalue::Map(Vec::new()),
        Type::Array(_) => Rvalue::Array(Vec::new()),
        Type::Set(_) => Rvalue::Set(Vec::new()),
        Type::Object => Rvalue::Object(Vec::new()),
        Type::Integer => Rvalue::Use(Operand::Const(Const::Int(0))),
        Type::Real => Rvalue::Use(Operand::Const(Const::Real(0.0f64.to_bits()))),
        Type::BigInteger => Rvalue::Use(Operand::Const(Const::BigInt("0".into()))),
        Type::Boolean => Rvalue::Use(Operand::Const(Const::Bool(false))),
        Type::String => Rvalue::Use(Operand::Const(Const::String(String::new()))),
        _ => return None,
    })
}

impl FnLowerer<'_> {
    pub(crate) fn lower_block_stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.lower_stmt(s);
            if !self.is_open() {
                // Dead code after a terminator. Drop it — the
                // unreachable terminator on the (already-closed)
                // block stays. We don't open a fresh block for
                // dead statements; the next control-flow context
                // (e.g. else-branch, loop body) will open its own.
                return;
            }
        }
    }

    pub(crate) fn lower_stmt(&mut self, s: &Stmt) {
        // Stamp every MIR statement emitted while lowering this HIR
        // statement with its source span (for the native debug backend).
        self.cur_span = s.span();
        match s {
            Stmt::Expr(e) => {
                let _ = self.lower_expr_to_operand(e);
            }
            Stmt::VarDecl(v) => self.lower_var_decl(v),
            Stmt::Return(opt) => self.lower_return(opt.as_ref()),
            Stmt::If(i) => self.lower_if(i),
            Stmt::While(w) => self.lower_while(w),
            Stmt::DoWhile(dw) => self.lower_do_while(dw),
            Stmt::For(f) => self.lower_for(f),
            Stmt::Foreach(fe) => self.lower_foreach(fe),
            Stmt::Break(_) => {
                if let Some(ctx) = self.loop_stack.last().copied() {
                    // Upstream `LeekBreakInstruction` prepends `addCounter(1)`
                    // — a taken `break` costs 1 op.
                    self.push_stmt(Statement::Charge(1));
                    self.goto(ctx.break_target);
                } else {
                    self.errors.push(convert::lowering_unsupported(
                        s.span(),
                        "`break` outside any loop",
                    ));
                }
            }
            Stmt::Continue(_) => {
                if let Some(ctx) = self.loop_stack.last().copied() {
                    // Like `break`: upstream charges 1 op per taken `continue`.
                    self.push_stmt(Statement::Charge(1));
                    self.goto(ctx.continue_target);
                } else {
                    self.errors.push(convert::lowering_unsupported(
                        s.span(),
                        "`continue` outside any loop",
                    ));
                }
            }
            Stmt::Block(b) => self.lower_block_stmts(&b.stmts),
            Stmt::Switch(sw) => self.lower_switch(sw),
            Stmt::Include(_) | Stmt::Import(_) => {
                // Includes are a parser-stage construct (already merged by the
                // time we see HIR); library imports are compile-time metadata
                // only. Nothing to emit for either.
            }
            Stmt::Charge(n) => self.push_stmt(Statement::Charge(*n)),
        }
    }

    pub(crate) fn lower_var_decl(&mut self, v: &VarDecl) {
        if v.is_global {
            // A top-level `global x = init` declaration: the
            // global itself is registered in `ProgramCtx::lower`.
            // The initializer is lowered as an assignment into the
            // global's slot. A *typed* global with no initializer
            // gets its type's default (`global Map x` → `[:]`), so
            // `x = (x[1] = [:])` writes into a real container rather
            // than a null base.
            if let Some(init) = &v.init {
                let value = self.lower_expr_to_operand(init);
                self.push_stmt(Statement::Assign(
                    Place::Global(v.def, v.name.clone()),
                    Rvalue::Use(value),
                ));
                // Upstream charges 1 op for the assignment/store of a
                // `var`/`global x = e` declaration (`ops(e, 1)`).
                self.push_stmt(Statement::Charge(1));
            } else {
                // An uninitialized declaration still stores its default
                // (`ops(default, 1)` upstream) — the 1-op store applies
                // with or without an explicit initializer.
                self.push_stmt(Statement::Charge(1));
                if let Some(rv) = v.ty.as_ref().and_then(default_rvalue_for_type) {
                    self.push_stmt(Statement::Assign(Place::Global(v.def, v.name.clone()), rv));
                }
            }
            return;
        }
        // Allocate the local up front so the initializer can see it
        // (Leekscript permits `var x = x + 1` to refer to the outer
        // `x`, but HIR has already resolved that — the inner ref
        // points at the new binding only if it actually shadows).
        let ty = v.ty.clone().unwrap_or(Type::Any);
        let id = self.declare_local(Some(v.name.clone()), ty, LocalKind::UserLocal, v.span);
        self.local_map.insert(v.def, id);
        // When no explicit type was given, infer one from a simple
        // initialiser. Stored separately from `ty` so plain `=`
        // doesn't coerce (only compound assigns consult it).
        if v.ty.is_none()
            && let Some(init) = &v.init
        {
            self.locals[id.0 as usize].inferred_ty = infer_simple_init_ty(init);
        }
        if let Some(init) = &v.init {
            // Upstream charges 1 op for the store of a `var x = e`
            // declaration (`ops(e, 1)`). Charged once per declaration,
            // covering the alias / self-rec / plain sub-paths below.
            self.push_stmt(Statement::Charge(1));
            // `var b = @a` — opt out of v1's pass-by-value clone:
            // upstream's `@`-prefix in this position just means
            // "skip the LegacyArray snapshot", so `b` ends up
            // pointing at the same `Rc` as `a` for composite
            // values while scalars still copy by value.
            // `MakeAlias` is a raw-read assignment that the
            // interp's `Place::Local` handler treats as
            // copy-not-clone (no `deep_clone_for_v1`).
            if let ExprKind::Unary(leek_hir::UnaryOp::Ref, inner) = &init.kind {
                let src = self.lower_expr_to_operand(inner);
                self.push_stmt(Statement::Assign(Place::Local(id), Rvalue::Use(src)));
                // Mark the local so the interp knows not to clone
                // composite values on this assignment in v1.
                self.locals[id.0 as usize].is_by_ref = true;
                return;
            }
            // Detect self-recursive lambda init: `var f = function() { f(...) }`.
            // The lambda captures `v.def` as its own first-class
            // reference, but at MakeLambda time the slot is null
            // (we haven't done the assign yet). After the assign
            // lands, we patch the lambda's capture so subsequent
            // calls see the right binding.
            let self_capture_slot = match &init.kind {
                ExprKind::Lambda(lam) => collect_lambda_captures(lam)
                    .iter()
                    .position(|c| *c == v.def),
                _ => None,
            };
            let value = self.lower_expr_to_operand(init);
            // A builtin call returns a *fresh* value, so `var a =
            // arrayMap(...)` must not be v1-cloned on assignment — the
            // clone would deep-copy the result and lose any references
            // its elements share. User-call / variable / literal inits
            // still clone (pass-by-value).
            let rv = if init_is_fresh_builtin_call(init) {
                Rvalue::UseFresh(value)
            } else {
                Rvalue::Use(value)
            };
            self.push_stmt(Statement::Assign(Place::Local(id), rv));
            if let Some(slot) = self_capture_slot {
                self.push_stmt(Statement::Assign(
                    Place::LambdaCapture { lambda: id, slot },
                    Rvalue::Use(Operand::Local(id)),
                ));
            }
        } else {
            // An uninitialized declaration still stores its default
            // (`ops(default, 1)` upstream) — the 1-op store applies with
            // or without an explicit initializer.
            self.push_stmt(Statement::Charge(1));
            if let Some(rv) = v.ty.as_ref().and_then(default_rvalue_for_type) {
                // Typed local with no initializer defaults to its type's
                // value (container → empty, scalar → zero), matching the
                // upstream "typed slots are never null" rule.
                self.push_stmt(Statement::Assign(Place::Local(id), rv));
            }
        }
    }

    pub(crate) fn lower_return(&mut self, expr: Option<&Expr>) {
        let value = expr.map(|e| self.lower_expr_to_operand(e));
        self.set_terminator(Terminator::Return(value));
    }

    pub(crate) fn lower_if(&mut self, i: &IfStmt) {
        let cond = self.lower_expr_to_operand(&i.cond);
        let then_bb = self.new_block();
        let else_bb = self.new_block();
        let join_bb = self.new_block();
        // The `if` condition test costs 1 op (flow-control charge;
        // the native backend's branches themselves are free). A `soft`
        // if is the desugared `return? x` — upstream emits its null
        // test without an `ops()` tick, so it stays free here too.
        if !i.soft {
            self.push_stmt(Statement::Charge(1));
        }
        self.set_terminator(Terminator::Branch {
            cond,
            then_block: then_bb,
            else_block: else_bb,
        });

        self.resume(then_bb);
        self.lower_stmt(&i.then_branch);
        self.goto(join_bb);

        self.resume(else_bb);
        if let Some(else_branch) = &i.else_branch {
            self.lower_stmt(else_branch);
        }
        self.goto(join_bb);

        self.resume(join_bb);
    }

    pub(crate) fn lower_while(&mut self, w: &WhileStmt) {
        let header = self.new_block();
        let body_bb = self.new_block();
        let exit = self.new_block();
        self.goto(header);
        self.resume(header);
        let cond = self.lower_expr_to_operand(&w.cond);
        self.set_terminator(Terminator::Branch {
            cond,
            then_block: body_bb,
            else_block: exit,
        });

        self.resume(body_bb);
        // Loops tick 1 op per *body entry* (the Java oracle charges on
        // entering the body, N times for N iterations — NOT on each
        // header check, which would be N+1).
        self.push_stmt(Statement::Charge(1));
        self.loop_stack.push(LoopCtx {
            continue_target: header,
            break_target: exit,
        });
        self.lower_stmt(&w.body);
        self.loop_stack.pop();
        self.goto(header);

        self.resume(exit);
    }

    pub(crate) fn lower_do_while(&mut self, dw: &DoWhileStmt) {
        let body_bb = self.new_block();
        let cond_bb = self.new_block();
        let exit = self.new_block();
        self.goto(body_bb);
        self.resume(body_bb);
        // 1 op per body entry — see `lower_while`.
        self.push_stmt(Statement::Charge(1));
        self.loop_stack.push(LoopCtx {
            continue_target: cond_bb,
            break_target: exit,
        });
        self.lower_stmt(&dw.body);
        self.loop_stack.pop();
        self.goto(cond_bb);

        self.resume(cond_bb);
        let cond = self.lower_expr_to_operand(&dw.cond);
        self.set_terminator(Terminator::Branch {
            cond,
            then_block: body_bb,
            else_block: exit,
        });

        self.resume(exit);
    }

    pub(crate) fn lower_for(&mut self, f: &ForStmt) {
        if let Some(init) = &f.init {
            self.lower_stmt(init);
        }
        let header = self.new_block();
        let body_bb = self.new_block();
        let step_bb = self.new_block();
        let exit = self.new_block();
        self.goto(header);

        self.resume(header);
        if let Some(cond) = &f.cond {
            let c = self.lower_expr_to_operand(cond);
            self.set_terminator(Terminator::Branch {
                cond: c,
                then_block: body_bb,
                else_block: exit,
            });
        } else {
            self.set_terminator(Terminator::Goto(body_bb));
        }

        self.resume(body_bb);
        // 1 op per body entry — see `lower_while`. Charged even with no
        // condition (`for (;;)`): the iteration tick is what bounds the
        // loop against the op budget.
        self.push_stmt(Statement::Charge(1));
        self.loop_stack.push(LoopCtx {
            continue_target: step_bb,
            break_target: exit,
        });
        self.lower_stmt(&f.body);
        self.loop_stack.pop();
        self.goto(step_bb);

        self.resume(step_bb);
        if let Some(step) = &f.step {
            let _ = self.lower_expr_to_operand(step);
        }
        self.goto(header);

        self.resume(exit);
    }

    pub(crate) fn lower_foreach(&mut self, fe: &ForeachStmt) {
        // Snapshot the iterable into an Array<[key, value]> and
        // walk it with a normal index loop. This keeps the loop's
        // shape uniform across array / map / set / interval /
        // string / object sources; the interpreter materialises
        // the snapshot at MakeForeachIter time.
        let iter_val = self.lower_expr_to_operand(&fe.iter);
        let iter_local = self.fresh_temp(Type::Any, fe.span);
        self.push_stmt(Statement::Assign(
            Place::Local(iter_local),
            Rvalue::MakeForeachIter(iter_val),
        ));
        // Charge model (mirrors upstream's `ForeachBlock` /
        // `ForeachKeyBlock`, see the Java backend's `emit_foreach`):
        // the key:value form ticks 1 op before the iterability check,
        // then setup charges 1 per *declared* slot (key form) or a
        // flat 1 (value form, declared or reused). Captured slots pay
        // their 1 op via a runtime Box ctor upstream — totals are
        // capture-independent, so we fold it into the static charge.
        // Upstream skips the setup charge when the iterated value is
        // not iterable; we charge unconditionally (a foreach over a
        // non-iterable is the only shape that differs).
        let setup = if let Some(k) = &fe.key {
            self.push_stmt(Statement::Charge(1));
            u64::from(k.is_new) + u64::from(fe.value.is_new)
        } else {
            1
        };
        if setup > 0 {
            self.push_stmt(Statement::Charge(setup));
        }
        let pos_local = self.fresh_temp(Type::Integer, fe.span);
        self.push_stmt(Statement::Assign(
            Place::Local(pos_local),
            Rvalue::Use(Operand::Const(Const::Int(0))),
        ));
        let len_local = self.fresh_temp(Type::Integer, fe.span);
        self.push_stmt(Statement::Assign(
            Place::Local(len_local),
            Rvalue::ForeachLen(iter_local),
        ));

        // Declare key/value user locals so body references resolve.
        let key_local = fe.key.as_ref().map(|k| {
            let id = self.declare_local(
                Some(k.name.clone()),
                Type::Any,
                LocalKind::UserLocal,
                k.span,
            );
            self.local_map.insert(k.def, id);
            id
        });
        let value_local = self.declare_local(
            Some(fe.value.name.clone()),
            Type::Any,
            LocalKind::UserLocal,
            fe.value.span,
        );
        self.local_map.insert(fe.value.def, value_local);

        let header = self.new_block();
        let body_bb = self.new_block();
        let step_bb = self.new_block();
        let exit = self.new_block();
        self.goto(header);

        // header: cond = pos < len; if cond then body else exit.
        // The test is synthesized machinery — `Synthetic` so it never
        // ticks the budget (upstream's `hasNext()` is free).
        self.resume(header);
        let cond = self.fresh_temp(Type::Boolean, fe.span);
        self.push_stmt(Statement::Assign(
            Place::Local(cond),
            Rvalue::Synthetic(Box::new(Rvalue::Binary(
                BinOp::Lt,
                Operand::Local(pos_local),
                Operand::Local(len_local),
            ))),
        ));
        self.set_terminator(Terminator::Branch {
            cond: Operand::Local(cond),
            then_block: body_bb,
            else_block: exit,
        });

        // body: pair = iter[pos]; key = pair[0]; value = pair[1];
        // <user body>; goto step. The pair reads are synthesized
        // (upstream's `next()` / `getKey()` / `getValue()` are free);
        // the explicit per-iteration tick below is the only charge.
        self.resume(body_bb);
        let pair_local = self.fresh_temp(Type::Any, fe.span);
        self.push_stmt(Statement::Assign(
            Place::Local(pair_local),
            Rvalue::Synthetic(Box::new(Rvalue::Index(
                iter_local,
                Operand::Local(pos_local),
            ))),
        ));
        if let Some(key) = key_local {
            self.push_stmt(Statement::Assign(
                Place::Local(key),
                Rvalue::Synthetic(Box::new(Rvalue::Index(
                    pair_local,
                    Operand::Const(Const::Int(0)),
                ))),
            ));
        }
        self.push_stmt(Statement::Assign(
            Place::Local(value_local),
            Rvalue::Synthetic(Box::new(Rvalue::Index(
                pair_local,
                Operand::Const(Const::Int(1)),
            ))),
        ));
        // Per-iteration tick. Value form: 1 op, except v1's by-value
        // copy-on-set path which pays 2 (`@ref` skips the copy → 1).
        // Key:value form: v2+ charges nothing, v1 charges 1 per
        // non-`@ref` slot.
        let (v1, vn) = if let Some(k) = &fe.key {
            (u64::from(!k.is_by_ref) + u64::from(!fe.value.is_by_ref), 0)
        } else if fe.value.is_by_ref {
            (1, 1)
        } else {
            (2, 1)
        };
        if v1 == vn {
            if v1 > 0 {
                self.push_stmt(Statement::Charge(v1));
            }
        } else {
            self.push_stmt(Statement::ChargeVersioned { v1, vn });
        }
        self.loop_stack.push(LoopCtx {
            continue_target: step_bb,
            break_target: exit,
        });
        self.lower_stmt(&fe.body);
        self.loop_stack.pop();
        self.goto(step_bb);

        // step: pos += 1; goto header. Synthetic — the increment is
        // loop machinery, not a user `+`.
        self.resume(step_bb);
        let new_pos = self.fresh_temp(Type::Integer, fe.span);
        self.push_stmt(Statement::Assign(
            Place::Local(new_pos),
            Rvalue::Synthetic(Box::new(Rvalue::Binary(
                BinOp::Add,
                Operand::Local(pos_local),
                Operand::Const(Const::Int(1)),
            ))),
        ));
        self.push_stmt(Statement::Assign(
            Place::Local(pos_local),
            Rvalue::Use(Operand::Local(new_pos)),
        ));
        self.goto(header);

        self.resume(exit);
    }

    pub(crate) fn lower_switch(&mut self, sw: &SwitchStmt) {
        // Switch with fall-through. Each case has two blocks:
        //   test_bb: compare discriminant; on hit → body_bb; on
        //            miss → next test_bb (or default body / exit).
        //   body_bb: run the case body; on tail-fallthrough →
        //            NEXT case's body_bb (mirrors C/Java where
        //            an absent `break` falls through). `break`
        //            still jumps to `exit` via `loop_stack`.
        let disc = self.lower_expr_to_operand(&sw.discriminant);
        let disc_local = match disc {
            Operand::Local(id) => id,
            Operand::Const(c) => {
                let t = self.fresh_temp(Type::Any, sw.span);
                self.push_stmt(Statement::Assign(
                    Place::Local(t),
                    Rvalue::Use(Operand::Const(c)),
                ));
                t
            }
        };

        // Pre-allocate one body block per case + a body block for
        // the default arm (used both for direct default match and
        // for fall-through after the last case).
        let mut case_bodies: Vec<BlockId> = Vec::new();
        let mut default_body: Option<BlockId> = None;
        for arm in &sw.arms {
            if arm.case.is_some() {
                case_bodies.push(self.new_block());
            } else {
                default_body = Some(self.new_block());
            }
        }
        let exit = self.new_block();

        self.loop_stack.push(LoopCtx {
            continue_target: exit,
            break_target: exit,
        });

        // First pass: emit the test chain.
        let default_target = default_body.unwrap_or(exit);
        let mut case_iter = case_bodies.iter().copied();
        for arm in &sw.arms {
            let Some(case_expr) = &arm.case else { continue };
            let body_bb = case_iter.next().unwrap();
            let case = self.lower_expr_to_operand(case_expr);
            let cmp = self.fresh_temp(Type::Boolean, sw.span);
            self.push_stmt(Statement::Assign(
                Place::Local(cmp),
                Rvalue::Binary(BinOp::Eq, Operand::Local(disc_local), case),
            ));
            let next_bb = self.new_block();
            // Each case test costs 1 op (flow-control charge; the
            // native backend's branches themselves are free).
            self.push_stmt(Statement::Charge(1));
            self.set_terminator(Terminator::Branch {
                cond: Operand::Local(cmp),
                then_block: body_bb,
                else_block: next_bb,
            });
            self.resume(next_bb);
        }
        self.goto(default_target);

        // Second pass: emit each case body. Tail fall-through
        // goto chains to the next body in source order; the
        // default body (if any) is the chain's final stop before
        // `exit`.
        let body_after =
            |i: usize, case_bodies: &[BlockId], default: Option<BlockId>, exit: BlockId| {
                if let Some(next) = case_bodies.get(i + 1).copied() {
                    return next;
                }
                default.unwrap_or(exit)
            };
        let mut case_index = 0usize;
        for arm in &sw.arms {
            if arm.case.is_some() {
                let body_bb = case_bodies[case_index];
                self.resume(body_bb);
                self.lower_block_stmts(&arm.body);
                let next = body_after(case_index, &case_bodies, default_body, exit);
                self.goto(next);
                case_index += 1;
            } else if let Some(default_bb) = default_body {
                self.resume(default_bb);
                self.lower_block_stmts(&arm.body);
                self.goto(exit);
            }
        }

        self.loop_stack.pop();
        self.resume(exit);
    }
}
