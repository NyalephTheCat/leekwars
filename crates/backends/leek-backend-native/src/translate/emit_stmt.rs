//! Statement / place emission: per-statement dispatch, indexed/field/global stores, terminators, and the coalesced op-budget charge.

use super::{
    BlockId, Const, InstBuilder, IntCC, LocalId, LocalKind, NativeError, Operand, Place, Rvalue,
    Statement, Terminator, TrapCode, Tx, Type, ValTy, Value, map_value_valty, no_coalesce,
    receiver_class, resolve_static_field, types, unsupported,
};

impl Tx<'_, '_> {
    /// Accumulate `n` operations into the current block's pending charge, at
    /// the same MIR sites the interpreter's `charge_ops` fires. The charge is
    /// *coalesced*: nothing is emitted here — [`flush_charge`](Self::flush_charge)
    /// emits a single `leek_charge_ops(pending)` at the block boundary. Since a
    /// MIR block is straight-line, the summed-then-charged total equals the
    /// per-op total for any completing program, so the two backends still report
    /// identical op counts (`.ops(N)` corpus cases); a budget-hitting program
    /// errors under both, just possibly one block-charge later.
    // Returns `Result` for consistency with the fallible emit helpers it sits
    // beside (and is called with `?`); accumulation is infallible today.
    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn charge(&mut self, n: u64) -> Result<(), NativeError> {
        self.pending_charge = self.pending_charge.saturating_add(n);
        // A/B escape hatch: with `LEEK_NATIVE_NO_COALESCE=1` set, flush every
        // charge immediately, reproducing the old per-op charging so the two
        // modes can be benchmarked back-to-back on the same machine.
        if no_coalesce() {
            self.flush_charge()?;
        }
        Ok(())
    }

    /// Emit the coalesced `leek_charge_ops(pending)` call for the current block
    /// and reset the accumulator. Called at every terminator (block boundary)
    /// and, on a back-edge `Branch`, *before* the budget check so the check
    /// observes the block's ops. A zero pending charge emits nothing.
    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn flush_charge(&mut self) -> Result<(), NativeError> {
        let n = self.pending_charge;
        if n == 0 {
            return Ok(());
        }
        self.pending_charge = 0;
        // Text-dump (CLIF inspection) mode declares no imports — there's no op
        // counter to charge, so skip silently rather than fail the dump.
        let Ok(f) = self.imports.rt("leek_charge_ops") else {
            return Ok(());
        };
        let nv = self.b.ins().iconst(types::I64, n as i64);
        self.b.ins().call(f, &[nv]);
        Ok(())
    }

    /// Emit an op-budget back-edge check before a branch: if the budget is
    /// exhausted, jump to a trap block (which returns the function's default)
    /// instead of continuing — so an unbounded loop stops promptly. The runtime
    /// already recorded `TOO_MUCH_OPERATIONS`, which `run()` surfaces. Only
    /// emitted when a finite budget is in force (see
    /// [`crate::runtime::enforce_budget`]).
    ///
    /// The order matters for Cranelift: we emit the `brif` *first* (which fills
    /// the current block) and only then `switch_to_block` to fill the trap and
    /// continuation blocks. Filling the trap before terminating the current
    /// block — as a shared lazily-built trap would require — trips Cranelift's
    /// "you have to fill your block before switching" invariant, since the
    /// current loop block already holds the back-edge's charge instruction.
    pub(super) fn emit_budget_check(&mut self) -> Result<(), NativeError> {
        if !crate::runtime::enforce_budget() {
            return Ok(());
        }
        let f = self.imports.rt("leek_op_budget_exceeded")?;
        let inst = self.b.ins().call(f, &[]);
        let over = self.b.inst_results(inst)[0];
        let trap = self.b.create_block();
        let cont = self.b.create_block();
        // Terminate the current block — both switches below now leave a filled
        // block, satisfying Cranelift's invariant.
        self.b.ins().brif(over, trap, &[], cont, &[]);

        // Trap path: the budget is spent; return the function's default value
        // (the runtime has already recorded `TOO_MUCH_OPERATIONS`).
        self.b.switch_to_block(trap);
        let z = match self.ret_ty {
            ValTy::Ref => {
                let null = self.imports.rt("leek_box_null")?;
                let inst = self.b.ins().call(null, &[]);
                self.b.inst_results(inst)[0]
            }
            ValTy::Real => self.b.ins().f64const(0.0),
            _ => self.b.ins().iconst(types::I64, 0),
        };
        self.b.ins().return_(&[z]);

        // Continue emitting the branch into the budget-OK fall-through block.
        self.b.switch_to_block(cont);
        Ok(())
    }

    /// True when an rvalue reads a `@`-by-ref source local directly
    /// (`t = @a` lowers to `Use`/`UseFresh` of a local whose `is_by_ref` is
    /// set). Assigning such a value into a cell must *alias* (no v1 clone) so
    /// the by-ref alias chain is preserved — `function(@a){ t = @a }` makes
    /// `t` alias the caller's argument.
    pub(super) fn rvalue_reads_byref_source(&self, rv: &Rvalue) -> bool {
        match rv {
            Rvalue::Use(Operand::Local(l)) | Rvalue::UseFresh(Operand::Local(l)) => {
                self.mir_locals[l.0 as usize].is_by_ref
            }
            _ => false,
        }
    }

    /// If `id` is a `big_integer`-declared local (its var holds a boxed
    /// `Ref`), coerce the value being stored into it to a `Value::BigInt`
    /// via `leek_to_bigint` (reals truncate exactly, ints/bools convert,
    /// null passes through) — mirroring upstream's declared-type write
    /// coercion. Every other local passes the value through untouched.
    pub(super) fn coerce_bigint_local(
        &mut self,
        id: LocalId,
        v: Value,
    ) -> Result<Value, NativeError> {
        let is_bigint = match &self.mir_locals[id.0 as usize].ty {
            Type::BigInteger => true,
            Type::Nullable(t) => matches!(t.as_ref(), Type::BigInteger),
            _ => false,
        };
        if !is_bigint || self.var_tys[id.0 as usize] != ValTy::Ref {
            return Ok(v);
        }
        let f = self.imports.rt("leek_to_bigint")?;
        let inst = self.b.ins().call(f, &[v]);
        Ok(self.b.inst_results(inst)[0])
    }

    pub(super) fn stmt(&mut self, s: &Statement) -> Result<(), NativeError> {
        match s {
            Statement::Assign(Place::Local(id), rv) => {
                // A cell local's var holds the (stable) shared cell handle;
                // a write stores the new value *into* the cell so closures
                // sharing the cell see the reassignment — the var itself is
                // never rebound.
                if self.cell_locals.contains(id) {
                    let (v, ty) = self.rvalue(rv)?;
                    let mut v = self.coerce(v, ty, ValTy::Ref)?;
                    v = self.coerce_bigint_local(*id, v)?;
                    // v1 value semantics also apply to a captured cell local: a
                    // composite reassigned *into* the cell is copied (deep-clone)
                    // so it doesn't alias its source — UNLESS the value is freshly
                    // produced (`UseFresh`), the cell is itself a `@`-by-ref alias,
                    // or the RHS reads a `@`-by-ref source local (the alias chain
                    // continues, `t = @a`). Mirrors the plain-local path below.
                    if self.lang.version <= 1
                        && !matches!(rv, Rvalue::UseFresh(_))
                        && self.mir_locals[id.0 as usize].kind == LocalKind::UserLocal
                        && !self.mir_locals[id.0 as usize].is_by_ref
                        && !self.rvalue_reads_byref_source(rv)
                    {
                        let f = self.imports.rt("leek_clone_v1")?;
                        let inst = self.b.ins().call(f, &[v]);
                        v = self.b.inst_results(inst)[0];
                    }
                    let cell = self.b.use_var(self.vars[id.0 as usize]);
                    let f = self.imports.rt("leek_cell_set")?;
                    self.b.ins().call(f, &[cell, v]);
                    return Ok(());
                }
                let target = self.var_tys[id.0 as usize];
                // A direct array read into a scalar-typed slot reads straight
                // into an unboxed `i64`/`f64` (`integer x = arr[i]`), skipping
                // the box-then-`to_long`/`to_real`-unbox round-trip. Gated on a
                // statically-integer index and a non-class-ref base (the only
                // base kind `index` intercepts before the general read path).
                let (v, ty) = match rv {
                    Rvalue::Index(base, idx)
                        if matches!(target, ValTy::Int | ValTy::Real)
                            && self.operand_int_kind(idx)
                            && !self.classref_locals.contains_key(base) =>
                    {
                        (self.index_unboxed(*base, idx, target)?, target)
                    }
                    // A direct field read into a scalar-typed slot
                    // (`integer x = obj.f`) reads straight into an unboxed
                    // scalar. Gated to the generic member path `field` would take
                    // (a `Ref` base that isn't a class-ref; not the `super`/
                    // `class` meta-properties), so the special cases still route
                    // through `field`.
                    Rvalue::Field(base, name)
                        if matches!(target, ValTy::Int | ValTy::Real)
                            && name != "super"
                            && name != "class"
                            && self.var_tys[base.0 as usize] == ValTy::Ref
                            && !self.classref_locals.contains_key(base) =>
                    {
                        (self.field_unboxed(*base, name, target)?, target)
                    }
                    _ => self.rvalue(rv)?,
                };
                // Coerce to the local's declared cranelift kind (e.g.
                // `real x = 42` stores 42 as 42.0); a `big_integer` local
                // additionally coerces the boxed value to a bigint.
                let mut v = self.coerce(v, ty, target)?;
                v = self.coerce_bigint_local(*id, v)?;
                // v1 value semantics: assigning a composite to a *user*
                // local copies it (deep-clone), unless the value is freshly
                // produced (`UseFresh` — e.g. a builtin result) or the slot
                // is a `@`-by-ref alias.
                if self.lang.version <= 1
                    && target == ValTy::Ref
                    && !matches!(rv, Rvalue::UseFresh(_))
                    && self.mir_locals[id.0 as usize].kind == LocalKind::UserLocal
                    && !self.mir_locals[id.0 as usize].is_by_ref
                {
                    let f = self.imports.rt("leek_clone_v1")?;
                    let inst = self.b.ins().call(f, &[v]);
                    v = self.b.inst_results(inst)[0];
                }
                self.b.def_var(self.vars[id.0 as usize], v);
                Ok(())
            }
            Statement::Assign(Place::Index(base, idx), rv) => self.set_index(*base, idx, rv),
            Statement::Assign(Place::Field(base, name), rv) => self.set_field(*base, name, rv),
            Statement::Assign(Place::Global(_, name), rv) => self.set_global(name, rv),
            // Self-recursive `var f = function(){ f(...) }`: the lowering patches
            // the lambda's own capture slot after the init assign. In the cell
            // model this is a no-op — the binding `f` is `is_shared` (captured
            // by its own lambda), so it's a `Value::Cell` the lambda captured by
            // raw handle; the preceding `cell_set` of the lambda value into that
            // shared cell already makes recursive calls see the right binding.
            Statement::Assign(Place::LambdaCapture { lambda, .. }, _)
                if self.cell_locals.contains(lambda) =>
            {
                Ok(())
            }
            Statement::Assign(p, _) => Err(unsupported(format!("assign to {p:?}"))),
            // Static op charge inserted by the `leek-charge` HIR pass (statement
            // costs — assignments, returns, … — that aren't charged dynamically
            // by the binary/branch/builtin sites). The interpreter executes
            // these; native must too, or its op count comes up short.
            Statement::Charge(n) => self.charge(*n),
            // Version-split charge (foreach per-iteration tick): the
            // version is static here, so it folds to a plain charge.
            Statement::ChargeVersioned { v1, vn } => {
                self.charge(if self.lang.version <= 1 { *v1 } else { *vn })
            }
            // v1-v3 LegacyArray promotion: a mutating builtin (`push` in v1;
            // `assocSort`/`keySort`/`assocReverse`/`removeElement` in v1-v3)
            // may morph a dense array into a sparse map and stash the new
            // value; write it back to the local. A no-op in v4 (nothing is
            // stashed) and whenever the runtime stash is empty.
            Statement::ApplyPromotion(local) => {
                if self.lang.version <= 3 && self.var_tys[local.0 as usize] == ValTy::Ref {
                    let (cur, _) = self.local_value(*local)?;
                    let f = self.imports.rt("leek_apply_promotion")?;
                    let inst = self.b.ins().call(f, &[cur]);
                    let v = self.b.inst_results(inst)[0];
                    if self.cell_locals.contains(local) {
                        let cell = self.b.use_var(self.vars[local.0 as usize]);
                        let set = self.imports.rt("leek_cell_set")?;
                        self.b.ins().call(set, &[cell, v]);
                    } else {
                        self.b.def_var(self.vars[local.0 as usize], v);
                    }
                }
                Ok(())
            }
            Statement::Call { dest, call } => self.call(dest.as_ref(), call),
        }
    }

    /// `base[idx] = value` (v4 semantics: in-range writes land, otherwise
    /// no-op). `base` is an array handle.
    pub(super) fn set_index(
        &mut self,
        base: LocalId,
        idx: &Operand,
        rv: &Rvalue,
    ) -> Result<(), NativeError> {
        // `C['staticField'] = v` — write to per-class static storage.
        if let Some(cls) = self.classref_locals.get(&base).cloned() {
            if let Operand::Const(Const::String(name)) = idx
                && let Some((owner, field)) = resolve_static_field(self.program, &cls, name)
            {
                if field.is_final || !self.method_visible(owner, field.visibility) {
                    return Ok(());
                }
                let (v, vt) = self.rvalue(rv)?;
                let v = self.coerce(v, vt, ValTy::Ref)?;
                return self.static_field_set(owner, name, v);
            }
            return Err(unsupported("class reference index assignment"));
        }
        if self.var_tys[base.0 as usize] != ValTy::Ref {
            return Err(unsupported("index assign to non-composite"));
        }
        // Honor `final` fields when the base is a known class instance: a
        // constant field name no-ops if final (and the write is external);
        // a dynamic key that could hit a final field can't be checked
        // statically, so bail.
        if let Some(cls) = receiver_class(
            self.mir_locals,
            self.new_classes,
            self.aliased_classes,
            base,
        ) && let Some(c) = self.program.class_by_name(cls)
        {
            match idx {
                Operand::Const(Const::String(s)) => {
                    if self.is_final_field(base, s) {
                        return Ok(());
                    }
                }
                _ if self.owning_class != Some(c.def_id)
                    && c.field_layout.iter().any(|fs| fs.is_final) =>
                {
                    return Err(unsupported(
                        "dynamic index-write on instance with final field",
                    ));
                }
                _ => {}
            }
        }
        let (arr, _) = self.local_value(base)?;
        let (i, it) = self.operand(idx)?;
        let (mut v, mut vt) = self.rvalue(rv)?;
        // A typed numeric array coerces the written element to its element
        // kind (`Array<real>`'s `a[0] = 5` stores `5.0`); a constant-keyed
        // write to a typed instance field coerces to the field type.
        if vt != ValTy::Ref {
            if let Some(et) = self.elem_tys[base.0 as usize] {
                v = self.coerce(v, vt, et)?;
                vt = et;
            } else if let Some(mvt) = map_value_valty(&self.mir_locals[base.0 as usize].ty) {
                // A typed map coerces the written value to its value type
                // (`Map<integer, real>`'s `m[k] = 5` stores `5.0`).
                v = self.coerce(v, vt, mvt)?;
                vt = mvt;
            } else if let Operand::Const(Const::String(s)) = idx
                && let Some(ft) = self.field_coerce_ty(base, s)
            {
                v = self.coerce(v, vt, ft)?;
                vt = ft;
            }
        }
        let elem = self.coerce(v, vt, ValTy::Ref)?;
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        // A statically-integer index needs no heap box (`a[i] = v`); the boxed
        // path stays for map keys of any kind.
        if it == ValTy::Int {
            let set = self.imports.rt("leek_set_index_int")?;
            self.b.ins().call(set, &[arr, i, elem, ver]);
        } else {
            let set = self.imports.rt("leek_value_set_index")?;
            let idx_h = self.coerce(i, it, ValTy::Ref)?;
            self.b.ins().call(set, &[arr, idx_h, elem, ver]);
        }
        Ok(())
    }

    /// `base.field = value` — a string-keyed index-set (`set_index` routes
    /// objects/instances to `set_field`).
    pub(super) fn set_field(
        &mut self,
        base: LocalId,
        name: &str,
        rv: &Rvalue,
    ) -> Result<(), NativeError> {
        // `C.staticField = …` — write to per-class static storage.
        if let Some(cls) = self.classref_locals.get(&base).cloned() {
            let Some((owner, field)) = resolve_static_field(self.program, &cls, name) else {
                return Err(unsupported("class reference member assignment"));
            };
            // A `final` static field, or one inaccessible from here, ignores
            // the write (matching the interpreter).
            if field.is_final || !self.method_visible(owner, field.visibility) {
                return Ok(());
            }
            let (v, vt) = self.rvalue(rv)?;
            let v = self.coerce(v, vt, ValTy::Ref)?;
            return self.static_field_set(owner, name, v);
        }
        if self.var_tys[base.0 as usize] != ValTy::Ref {
            return Err(unsupported("field assign to non-object"));
        }
        // A `final` field ignores writes (its initializer already set it).
        if self.is_final_field(base, name) {
            return Ok(());
        }
        let slot = self.field_slot_of(base, name);
        let (base_h, _) = self.local_value(base)?;
        let (ptr, lenv) = self.const_str_bytes(name);
        let (mut v, mut vt) = self.rvalue(rv)?;
        // Coerce a scalar write to the declared field type (`real? x = 5`
        // stores `5.0`), matching the interpreter's `coerce_to_type`.
        if vt != ValTy::Ref
            && let Some(ft) = self.field_coerce_ty(base, name)
        {
            v = self.coerce(v, vt, ft)?;
            vt = ft;
        }
        let val = self.coerce(v, vt, ValTy::Ref)?;
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        // The field name is passed unboxed (`ptr`,`len`). A known-class field
        // resolves to a dense slot → `leek_field_set_slot` writes it via a
        // direct `Vec` index, skipping the field-name hash; otherwise
        // `leek_field_set` writes via `set_field(&str, …)`. Neither allocates a
        // `Value::String` key.
        if let Some(slot) = slot {
            let set = self.imports.rt("leek_field_set_slot")?;
            let slotv = self
                .b
                .ins()
                .iconst(types::I64, i64::try_from(slot).unwrap_or(i64::MAX));
            self.b
                .ins()
                .call(set, &[base_h, slotv, ptr, lenv, val, ver]);
        } else {
            let set = self.imports.rt("leek_field_set")?;
            self.b.ins().call(set, &[base_h, ptr, lenv, val, ver]);
        }
        Ok(())
    }

    /// `global name = value`. A typed global (`global real x`) coerces the
    /// written value to its declared kind.
    pub(super) fn set_global(&mut self, name: &str, rv: &Rvalue) -> Result<(), NativeError> {
        let set = self.imports.rt("leek_global_set")?;
        let key = self.const_string(name)?;
        let (mut v, mut vt) = self.rvalue(rv)?;
        if let Some(&gt) = self.global_tys.get(name)
            && vt != ValTy::Ref
        {
            v = self.coerce(v, vt, gt)?;
            vt = gt;
        }
        let mut val = self.coerce(v, vt, ValTy::Ref)?;
        // A `big_integer`-declared global coerces every store, like a local.
        let global_is_bigint = self.program.globals.iter().any(|g| {
            g.name == name
                && match &g.ty {
                    Type::BigInteger => true,
                    Type::Nullable(t) => matches!(t.as_ref(), Type::BigInteger),
                    _ => false,
                }
        });
        if global_is_bigint {
            let f = self.imports.rt("leek_to_bigint")?;
            let inst = self.b.ins().call(f, &[val]);
            val = self.b.inst_results(inst)[0];
        }
        self.b.ins().call(set, &[key, val]);
        Ok(())
    }

    pub(super) fn terminator(
        &mut self,
        block_id: BlockId,
        t: &Terminator,
    ) -> Result<(), NativeError> {
        // A `default_init` block used as an entry-time param filler: its
        // `Return(op)` stores `op` into the param var (peeling/boxing for a
        // cell) and jumps to the continuation rather than returning.
        if let Some(&(param, cont)) = self.default_fill.get(&block_id) {
            let Terminator::Return(Some(op)) = t else {
                return Err(unsupported("default-init block: non-return terminator"));
            };
            let (v, vt) = self.operand(op)?;
            let target = self.var_tys[param.0 as usize];
            let v = self.coerce(v, vt, target)?;
            if self.cell_locals.contains(&param) {
                let cell = self.b.use_var(self.vars[param.0 as usize]);
                let set = self.imports.rt("leek_cell_set")?;
                self.b.ins().call(set, &[cell, v]);
            } else {
                self.b.def_var(self.vars[param.0 as usize], v);
            }
            self.flush_charge()?;
            self.b.ins().jump(cont, &[]);
            return Ok(());
        }
        match t {
            Terminator::Goto(b) => {
                self.flush_charge()?;
                self.b.ins().jump(self.blocks[b], &[]);
            }
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // Conditional-branch op charges live in MIR lowering (an
                // explicit `Statement::Charge(1)` at if/ternary/and/or/`??`/
                // `?.`/switch sites; loops tick once per *body entry* instead,
                // matching the Java oracle), so the branch itself is free here.
                //
                // Flush the block's coalesced charge before the budget check so
                // the check observes the full count.
                self.flush_charge()?;
                // Back-edge budget check: a branch is the only way to re-enter a
                // block, so checking here bounds every loop. Stops an unbounded
                // loop once the op budget is spent (when a finite one is set).
                self.emit_budget_check()?;
                let (c, ty) = self.operand(cond)?;
                // brif tests an integer for non-zero; a real condition
                // becomes `c != 0.0`; a dynamic value goes through the
                // shared `is_truthy` runtime shim.
                let c = if ty == ValTy::Ref {
                    let f = self.imports.rt("leek_truthy")?;
                    let inst = self.b.ins().call(f, &[c]);
                    self.b.inst_results(inst)[0]
                } else {
                    self.truthy(c, ty)
                };
                self.b.ins().brif(
                    c,
                    self.blocks[then_block],
                    &[],
                    self.blocks[else_block],
                    &[],
                );
            }
            Terminator::Return(Some(op)) => {
                // `return @x` (v1): hand back the *raw* shared cell so the
                // caller aliases the same storage (matches the interpreter).
                // The `@` marks the returned cell local `is_by_ref`; a plain
                // `return x` of a cell local peels as usual. Guarded to v1 + a
                // boxed (`Ref`) return type — a scalar `@x` return has no
                // aliasing to preserve and would mis-coerce a cell handle to an
                // int, so it peels instead.
                let (v, ty) = match op {
                    Operand::Local(id)
                        if self.lang.version <= 1
                            && self.ret_ty == ValTy::Ref
                            && self.cell_locals.contains(id)
                            && self.mir_locals[id.0 as usize].is_by_ref =>
                    {
                        (self.b.use_var(self.vars[id.0 as usize]), ValTy::Ref)
                    }
                    _ => self.operand(op)?,
                };
                let v = self.coerce(v, ty, self.ret_ty)?;
                self.flush_charge()?;
                self.b.ins().return_(&[v]);
            }
            // A void function returns null. With a `Ref` result that's a
            // boxed null; for the (rejected-for-main) scalar case it's a
            // dead dummy zero.
            Terminator::Return(None) => {
                self.flush_charge()?;
                let z = match self.ret_ty {
                    ValTy::Ref => {
                        let null = self.imports.rt("leek_box_null")?;
                        let inst = self.b.ins().call(null, &[]);
                        self.b.inst_results(inst)[0]
                    }
                    ValTy::Real => self.b.ins().f64const(0.0),
                    _ => self.b.ins().iconst(types::I64, 0),
                };
                self.b.ins().return_(&[z]);
            }
            Terminator::Switch {
                discriminant,
                arms,
                default,
            } => {
                self.flush_charge()?;
                let (disc, dty) = self.operand(discriminant)?;
                if dty == ValTy::Real {
                    return Err(unsupported("switch on real"));
                }
                for (k, target) in arms {
                    let key = match k {
                        Const::Int(n) => *n,
                        Const::Bool(x) => *x as i64,
                        other => return Err(unsupported(format!("switch on {other:?}"))),
                    };
                    let kv = self.b.ins().iconst(types::I64, key);
                    let eq = self.b.ins().icmp(IntCC::Equal, disc, kv);
                    let next = self.b.create_block();
                    self.b.ins().brif(eq, self.blocks[target], &[], next, &[]);
                    self.b.switch_to_block(next);
                }
                self.b.ins().jump(self.blocks[default], &[]);
            }
            Terminator::Unreachable => {
                self.flush_charge()?;
                self.b.ins().trap(TrapCode::user(1).unwrap());
            }
        }
        Ok(())
    }
}
