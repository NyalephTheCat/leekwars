//! Per-function CFG lowering.

use std::collections::HashSet;

use leek_diagnostics::convert;
use leek_hir::{
    Callee as HirCallee, DoWhileStmt, Expr, ExprKind, ForStmt, ForeachBind, ForeachStmt, IfStmt,
    Literal, NameRef, Stmt, SwitchStmt, VarDecl, WhileStmt,
};
use leek_types::Type;

use crate::ir::{
    BinOp, BlockId, Const, LocalId, LocalKind, Operand, Place, Rvalue, Statement, Terminator,
};

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
        ExprKind::Call(c) if matches!(
            &c.callee,
            HirCallee::Function(NameRef::Builtin(_) | NameRef::Unresolved(_))
        )
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

    /// The value a declaration with no initialiser stores, or `None` for one
    /// that stays null.
    ///
    /// v1 is the whole exception: there a declaration is a box that starts
    /// null whatever its type, which is why `Map m m['a'] = 7` indexes into
    /// null and faults there and nowhere else. From v2 a typed slot is never
    /// null and takes its type's own value.
    fn declared_default(&self, ty: Option<&Type>) -> Option<Rvalue> {
        if self.hir.version <= 1 {
            return None;
        }
        ty.and_then(default_rvalue_for_type)
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
                if let Some(rv) = self.declared_default(v.ty.as_ref()) {
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
            // backend's `Place::Local` handler treats as
            // copy-not-clone (no `deep_clone_for_v1`).
            if let ExprKind::Unary(leek_hir::UnaryOp::Ref, inner) = &init.kind {
                let src = self.lower_expr_to_operand(inner);
                self.push_stmt(Statement::Assign(Place::Local(id), Rvalue::Use(src)));
                // Mark the local so the backend knows not to clone
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
            if let Some(rv) = self.declared_default(v.ty.as_ref()) {
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
        // `leek_hir::transform::mark_constant_conditions` decided this `if`
        // at compile time: emit the taken side alone, with no test, so it
        // costs *no* operation — not even the one a real test charges — and
        // the dead arm charges nothing for its body either. The condition is
        // never lowered, which is what makes `if (DEBUG && expensive())` free
        // rather than merely cheap.
        //
        // Reading the mark rather than re-deciding here is what keeps
        // `if (constant_call())` a real branch: the mark predates the call
        // substitution that made its condition a literal, as upstream's own
        // pass order does.
        if let Some(taken) = i.const_taken {
            let branch = if taken {
                Some(&i.then_branch)
            } else {
                i.else_branch.as_ref()
            };
            if let Some(branch) = branch {
                self.lower_stmt(branch);
            }
            return;
        }
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
        // Snapshot the iterable and walk the snapshot with a normal
        // index loop. This keeps the loop's shape uniform across
        // array / map / set / interval sources; the runtime
        // materialises the snapshot at MakeForeachIter time, and the
        // loop reads elements out of it directly — no per-element pair
        // to allocate or unpack (#111). A non-iterable — a string
        // (#268), an object or a class instance (#494) included —
        // snapshots to nothing, so the walk is empty.
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

        // A `var` binding declares a fresh user local so body references
        // resolve; a bare binding (`for (x in …)`) has no slot of its own —
        // each iteration stores through its l-value instead.
        let key_local = fe
            .key
            .as_ref()
            .and_then(|k| self.declare_foreach_binding(k));
        let value_local = self.declare_foreach_binding(&fe.value);

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

        // body: key = iter.key(pos); value = iter.value(pos);
        // <user body>; goto step. The snapshot reads are uncharged
        // (upstream's `next()` / `getKey()` / `getValue()` are free);
        // the explicit per-iteration tick below is the only charge.
        self.resume(body_bb);
        let pos = Operand::Local(pos_local);
        if let Some(k) = &fe.key {
            let read = Rvalue::ForeachKeyAt(iter_local, pos.clone());
            self.store_foreach_binding(k, key_local, read);
        }
        let read = Rvalue::ForeachValueAt(iter_local, pos);
        self.store_foreach_binding(&fe.value, value_local, read);
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

    /// Declare the fresh user local of a `var` foreach binding and map its
    /// `DefId` to it. `None` for a bare binding, which reuses existing storage.
    fn declare_foreach_binding(&mut self, bind: &ForeachBind) -> Option<LocalId> {
        let def = bind.local_def().filter(|_| bind.is_new)?;
        let id = self.declare_local(
            Some(bind.name.clone()),
            Type::Any,
            LocalKind::UserLocal,
            bind.span,
        );
        self.local_map.insert(def, id);
        Some(id)
    }

    /// Store a snapshot read (`read`, a [`Rvalue::ForeachKeyAt`] or
    /// [`Rvalue::ForeachValueAt`]) into a binding: its declared local, or —
    /// for a bare binding — the same [`Place`] an assignment to that name
    /// writes (reused local or capture cell, global, name-keyed global, class
    /// field).
    fn store_foreach_binding(
        &mut self,
        bind: &ForeachBind,
        declared: Option<LocalId>,
        read: Rvalue,
    ) {
        let place = match declared {
            Some(id) => Place::Local(id),
            None => self.lower_place(&bind.target),
        };
        self.push_stmt(Statement::Assign(place, read));
    }

    /// A fresh temp holding `rv`, in the block being built.
    fn materialize_here(&mut self, rv: Rvalue, span: leek_span::Span) -> LocalId {
        let t = self.fresh_temp(Type::Any, span);
        self.push_stmt(Statement::Assign(Place::Local(t), rv));
        t
    }

    /// The chain that decides which arm a `switch` takes: one loose
    /// comparison per label, branching to that label's body on a hit and to
    /// the next test on a miss. Leaves the builder on the block after the
    /// last test, for the caller to send at the default.
    ///
    /// `dispatched` says the selection is upstream's O(1) one, already
    /// charged in full: the comparisons then compute the same answer for
    /// nothing, rather than charging an operation each — and, for strings,
    /// a character-by-character comparison each.
    fn lower_switch_tests(
        &mut self,
        sw: &SwitchStmt,
        disc_local: LocalId,
        bodies: &[BlockId],
        dispatched: bool,
    ) {
        for (arm, &body_bb) in sw.arms.iter().zip(bodies) {
            let Some(case_expr) = &arm.case else { continue };
            let case = self.lower_expr_to_operand(case_expr);
            let cmp = self.fresh_temp(Type::Boolean, sw.span);
            // `eq()`, not `==`: a switch's loose comparison is the same in
            // every version, so `switch ('1') { case 1: … }` matches at v4 as
            // it does at v1.
            let test = Rvalue::Binary(BinOp::LooseEq, Operand::Local(disc_local), case);
            self.push_stmt(Statement::Assign(
                Place::Local(cmp),
                if dispatched {
                    Rvalue::Synthetic(Box::new(test))
                } else {
                    test
                },
            ));
            let next_bb = self.new_block();
            self.set_terminator(Terminator::Branch {
                cond: Operand::Local(cmp),
                then_block: body_bb,
                else_block: next_bb,
            });
            self.resume(next_bb);
        }
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

        // One body block per arm, in source order — which is also
        // fall-through order. `default` is an arm like any other: it can sit
        // in the middle, and a body that falls off its end continues into
        // whatever is written next, not into the default.
        let bodies: Vec<BlockId> = sw.arms.iter().map(|_| self.new_block()).collect();
        let default_body = sw
            .arms
            .iter()
            .position(|a| a.case.is_none())
            .map(|i| bodies[i]);
        let exit = self.new_block();

        self.loop_stack.push(LoopCtx {
            // `break` leaves the switch; `continue` belongs to the enclosing
            // loop and passes straight through — a switch is not a loop.
            // With no loop around it, `continue` has nowhere to go but out.
            continue_target: self
                .loop_stack
                .last()
                .map_or(exit, |outer| outer.continue_target),
            break_target: exit,
        });

        // Upstream emits a real Java `switch` — one O(1) dispatch, charged a
        // single operation however many cases there are — when every label is
        // a constant of one kind and the subject is that kind too, because
        // `eq()` then reduces to strict equality. The comparisons below are
        // that same selection, so only the charging changes.
        let default_target = default_body.unwrap_or(exit);
        match constant_dispatch(sw, &self.locals[disc_local.0 as usize].ty) {
            Dispatch::Chain => {
                self.lower_switch_tests(sw, disc_local, &bodies, false);
                self.goto(default_target);
            }
            Dispatch::Direct => {
                self.push_stmt(Statement::Charge(1));
                self.lower_switch_tests(sw, disc_local, &bodies, true);
                self.goto(default_target);
            }
            // A subject only known at run time gets upstream's *guarded*
            // form: when the value turns out to be of the labels' kind the
            // dispatch is the same O(1) one, and when it is not the loose
            // `eq()` chain is the only thing that can compare across kinds.
            // Both select the same arm; what differs is what they cost.
            Dispatch::Guarded(class) => {
                let cls = self.materialize_here(Rvalue::BuiltinRef(class.to_string()), sw.span);
                let ok = self.materialize_here(
                    Rvalue::Synthetic(Box::new(Rvalue::Binary(
                        BinOp::Instanceof,
                        Operand::Local(disc_local),
                        Operand::Local(cls),
                    ))),
                    sw.span,
                );
                let fast = self.new_block();
                let slow = self.new_block();
                self.set_terminator(Terminator::Branch {
                    cond: Operand::Local(ok),
                    then_block: fast,
                    else_block: slow,
                });
                self.resume(fast);
                self.push_stmt(Statement::Charge(1));
                self.lower_switch_tests(sw, disc_local, &bodies, true);
                self.goto(default_target);
                self.resume(slow);
                self.lower_switch_tests(sw, disc_local, &bodies, false);
                self.goto(default_target);
            }
        }

        // Second pass: emit each body, each falling through to the next arm
        // in source order and the last one to `exit`.
        //
        // Every body entered — by a match or by falling through from
        // the previous body — costs 1 op: upstream opens each Java
        // `case N: {` block with `ops(1)` (reference.tsv row
        // `var a = 0 var x = 1 switch (x) { case 1: a = 4 if (2 == 2) {
        // return 99 } case 2: ... }` = 7 ops, #78). Upstream merges
        // stacked labels (`case 1: case 2:`) into one test charged per
        // label; charging each empty body here yields the same total.
        for (i, arm) in sw.arms.iter().enumerate() {
            self.resume(bodies[i]);
            self.push_stmt(Statement::Charge(1));
            self.lower_block_stmts(&arm.body);
            self.goto(bodies.get(i + 1).copied().unwrap_or(exit));
        }

        self.loop_stack.pop();
        self.resume(exit);
    }
}

/// How a `switch` selects its arm — which is a question about what it costs,
/// not about what it answers: every form below compares the same way.
enum Dispatch {
    /// One loose comparison per label, charged an operation each.
    Chain,
    /// Upstream's O(1) Java `switch`, charged one operation however many
    /// cases there are: every label is a constant of one kind and the subject
    /// is declared that kind, so `eq()` reduces to strict equality.
    Direct,
    /// The same dispatch behind a run-time kind test, for a subject whose
    /// type is only known then — the named builtin class is the labels' kind.
    /// A value of another kind falls back to the chain, which is the only
    /// thing that can compare across kinds.
    Guarded(&'static str),
}

/// Which form a `switch` takes.
///
/// Upstream's conditions, and its reasons: every label must be a constant of
/// one kind — all `integer`s in Java's `int` range, or all strings — with no
/// duplicate (a Java `switch` would not compile with one, so upstream keeps
/// the chain, where the first case wins), at most one `default`, and at least
/// one label. A subject declared that kind dispatches directly; one of
/// *another* scalar kind cannot match a label strictly at all, so it keeps
/// the chain; anything else — a `var` — gets the guarded form.
fn constant_dispatch(sw: &SwitchStmt, subject: &Type) -> Dispatch {
    let labels: Vec<&Expr> = sw.arms.iter().filter_map(|a| a.case.as_ref()).collect();
    if labels.is_empty() || sw.arms.iter().filter(|a| a.case.is_none()).count() > 1 {
        return Dispatch::Chain;
    }
    let strings = matches!(labels[0].kind, ExprKind::Literal(Literal::String(_)));
    let mut seen: HashSet<String> = HashSet::new();
    for label in labels {
        let key = match (strings, constant_label(label)) {
            (true, Some(Label::Str(s))) => s,
            (false, Some(Label::Int(n))) => n.to_string(),
            _ => return Dispatch::Chain,
        };
        if !seen.insert(key) {
            return Dispatch::Chain;
        }
    }
    let want = if strings { Type::String } else { Type::Integer };
    if *subject == want {
        return Dispatch::Direct;
    }
    // A subject of another scalar kind can never match a constant label
    // strictly, so the guard would be dead and the chain is all there is.
    if matches!(
        subject,
        Type::Integer | Type::String | Type::Real | Type::Boolean | Type::Null
    ) {
        return Dispatch::Chain;
    }
    Dispatch::Guarded(if strings { "String" } else { "Integer" })
}

enum Label {
    Int(i32),
    Str(String),
}

/// A case label as the constant it is, when it is one. `case -1:` is a unary
/// minus on a literal rather than a literal, and an integer outside Java's
/// `int` range cannot be a Java `switch` label at all.
fn constant_label(e: &Expr) -> Option<Label> {
    match &e.kind {
        ExprKind::Literal(Literal::String(s)) => Some(Label::Str(s.clone())),
        ExprKind::Literal(Literal::Int(n)) => i32::try_from(*n).ok().map(Label::Int),
        ExprKind::Unary(leek_hir::UnaryOp::Neg, x) => match &x.kind {
            ExprKind::Literal(Literal::Int(n)) => i32::try_from(-*n).ok().map(Label::Int),
            _ => None,
        },
        _ => None,
    }
}
