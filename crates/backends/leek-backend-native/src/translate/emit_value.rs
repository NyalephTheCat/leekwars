//! Value plumbing: operand loading, scalar coercions, constants, boxed field/index reads, and the binary/unary operator bodies.

use super::{
    BinOp, Const, FloatCC, InstBuilder, IntCC, LocalId, NativeError, Operand, StackSlotData,
    StackSlotKind, Tx, UnOp, ValTy, Value, const_pow_exp, is_const_zero, types, unsupported,
};

impl Tx<'_, '_> {
    /// The runtime *value* of a local as a handle/scalar. For a cell local
    /// (lambda-shared storage) this peels the cell (`cell_get` → boxed
    /// `Ref`); any other local is its raw variable. Use this wherever a
    /// local's value is needed as a base / receiver / callee — NOT for the
    /// cell-write target or a raw capture, which want the cell handle.
    pub(super) fn local_value(&mut self, id: LocalId) -> Result<(Value, ValTy), NativeError> {
        let v = self.b.use_var(self.vars[id.0 as usize]);
        if self.cell_locals.contains(&id) {
            let f = self.imports.rt("leek_cell_get")?;
            let inst = self.b.ins().call(f, &[v]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        Ok((v, self.var_tys[id.0 as usize]))
    }

    /// Materialize a string literal's bytes *in-binary* (immediate byte stores
    /// to a stack slot) and box them at runtime via `leek_const_string`. Unlike
    /// baking a `box_string` handle as an absolute immediate, this is fully
    /// relocatable — the AOT binary builds the string in its own process.
    /// Materialise a compile-time string's bytes onto a stack slot, returning
    /// `(ptr, len)` as i64 values. Used both to box a string literal
    /// (`const_string`) and to pass a field name unboxed to `leek_field_get*`.
    pub(super) fn const_str_bytes(&mut self, s: &str) -> (Value, Value) {
        let bytes = s.as_bytes();
        let len = bytes.len();
        let ptr = if len == 0 {
            self.b.ins().iconst(types::I64, 0)
        } else {
            let slot = self.b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                u32::try_from(len).unwrap_or(u32::MAX),
                0,
            ));
            for (i, &byte) in bytes.iter().enumerate() {
                let bv = self.b.ins().iconst(types::I8, i64::from(byte));
                self.b
                    .ins()
                    .stack_store(bv, slot, i32::try_from(i).unwrap_or(0));
            }
            self.b.ins().stack_addr(types::I64, slot, 0)
        };
        let lenv = self
            .b
            .ins()
            .iconst(types::I64, i64::try_from(len).unwrap_or(i64::MAX));
        (ptr, lenv)
    }

    pub(super) fn const_string(&mut self, s: &str) -> Result<Value, NativeError> {
        let (ptr, lenv) = self.const_str_bytes(s);
        let f = self.imports.rt("leek_const_string")?;
        let inst = self.b.ins().call(f, &[ptr, lenv]);
        Ok(self.b.inst_results(inst)[0])
    }

    /// Read instance field / member `name` of `base`, returning a boxed handle,
    /// via `leek_field_get` — the field name is passed unboxed (`ptr`,`len`), so
    /// no `Value::String` key is allocated per read. Semantically identical to
    /// `leek_value_index(base, const_string(name))`.
    /// `slot` is the compile-time-resolved dense field slot when `base`'s class
    /// is known (see [`Self::field_slot_of`]) — then the read goes through
    /// `leek_field_get_slot`, skipping the runtime field-name hash. `None` keeps
    /// the name path (`leek_field_get`), which also handles `obj.method`
    /// bound-method reads (a method has no field slot).
    pub(super) fn field_get_boxed(
        &mut self,
        base_h: Value,
        name: &str,
        slot: Option<usize>,
    ) -> Result<Value, NativeError> {
        let (ptr, lenv) = self.const_str_bytes(name);
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        let inst = if let Some(slot) = slot {
            let slotv = self
                .b
                .ins()
                .iconst(types::I64, i64::try_from(slot).unwrap_or(i64::MAX));
            let f = self.imports.rt("leek_field_get_slot")?;
            self.b.ins().call(f, &[base_h, slotv, ptr, lenv, ver])
        } else {
            let f = self.imports.rt("leek_field_get")?;
            self.b.ins().call(f, &[base_h, ptr, lenv, ver])
        };
        Ok(self.b.inst_results(inst)[0])
    }

    /// Read instance field `name` of `base` directly into an unboxed scalar
    /// (`target` is `Int`/`Real`), via `leek_field_get_int`/`_real`. The value is
    /// `read_member(..).to_long()/.to_real()` — byte-identical to what the boxed
    /// read coerced to the scalar slot would produce, with neither key nor result
    /// boxed. The caller guarantees `base` is a known class instance.
    pub(super) fn field_unboxed(
        &mut self,
        base: LocalId,
        name: &str,
        target: ValTy,
    ) -> Result<Value, NativeError> {
        let (bv, bt) = self.local_value(base)?;
        let base_h = self.coerce(bv, bt, ValTy::Ref)?;
        let (ptr, lenv) = self.const_str_bytes(name);
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        // A known-class field resolves to a dense slot → the slot shim skips the
        // field-name hash (see [`Self::field_slot_of`]).
        if let Some(slot) = self.field_slot_of(base, name) {
            let slotv = self
                .b
                .ins()
                .iconst(types::I64, i64::try_from(slot).unwrap_or(i64::MAX));
            let sym = if target == ValTy::Real {
                "leek_field_get_slot_real"
            } else {
                "leek_field_get_slot_int"
            };
            let f = self.imports.rt(sym)?;
            let inst = self.b.ins().call(f, &[base_h, slotv, ptr, lenv, ver]);
            return Ok(self.b.inst_results(inst)[0]);
        }
        let sym = if target == ValTy::Real {
            "leek_field_get_real"
        } else {
            "leek_field_get_int"
        };
        let f = self.imports.rt(sym)?;
        let inst = self.b.ins().call(f, &[base_h, ptr, lenv, ver]);
        Ok(self.b.inst_results(inst)[0])
    }

    pub(super) fn operand(&mut self, op: &Operand) -> Result<(Value, ValTy), NativeError> {
        match op {
            Operand::Local(id) => {
                let ty = self.var_tys[id.0 as usize];
                let v = self.b.use_var(self.vars[id.0 as usize]);
                // A cell local's var holds the shared cell handle; a normal
                // read peels the current value out of it.
                if self.cell_locals.contains(id) {
                    let f = self.imports.rt("leek_cell_get")?;
                    let inst = self.b.ins().call(f, &[v]);
                    return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
                }
                Ok((v, ty))
            }
            Operand::Const(Const::Int(n)) => Ok((self.b.ins().iconst(types::I64, *n), ValTy::Int)),
            Operand::Const(Const::Bool(x)) => {
                Ok((self.b.ins().iconst(types::I64, *x as i64), ValTy::Bool))
            }
            Operand::Const(Const::Real(bits)) => {
                Ok((self.b.ins().f64const(f64::from_bits(*bits)), ValTy::Real))
            }
            // A string literal: materialize its bytes in-binary and box them at
            // runtime (relocatable — see `const_string`).
            Operand::Const(Const::String(s)) => Ok((self.const_string(s)?, ValTy::Ref)),
            // A big_integer literal: same scheme, but the decimal digits parse
            // into an arbitrary-precision value (`leek_const_bigint`).
            Operand::Const(Const::BigInt(s)) => {
                let (ptr, lenv) = self.const_str_bytes(s);
                let f = self.imports.rt("leek_const_bigint")?;
                let inst = self.b.ins().call(f, &[ptr, lenv]);
                Ok((self.b.inst_results(inst)[0], ValTy::Ref))
            }
            // A null literal: build a fresh `Null` handle at runtime rather than
            // baking a pointer to the compiler process's null singleton.
            Operand::Const(Const::Null) => {
                let f = self.imports.rt("leek_box_null")?;
                let inst = self.b.ins().call(f, &[]);
                Ok((self.b.inst_results(inst)[0], ValTy::Ref))
            }
        }
    }

    /// Convert `v` (kind `from`) to kind `to`, matching Leekscript's
    /// `coerce_to_type`: int/bool widen to real, and real narrows to int
    /// by truncation toward zero (`(long) r` / Rust `r as i64`, which
    /// saturates — `fcvt_to_sint_sat`). Real → bool isn't a real
    /// coercion in Leekscript (it would keep the real), so it bails.
    pub(super) fn coerce(
        &mut self,
        v: Value,
        from: ValTy,
        to: ValTy,
    ) -> Result<Value, NativeError> {
        match (from, to) {
            (a, b) if a == b => Ok(v),
            // Box a scalar into a handle.
            (_, ValTy::Ref) => {
                let sym = match from {
                    ValTy::Int => "leek_box_int",
                    ValTy::Bool => "leek_box_bool",
                    ValTy::Real => "leek_box_real",
                    ValTy::Ref => unreachable!(),
                };
                let fref = self.imports.rt(sym)?;
                let inst = self.b.ins().call(fref, &[v]);
                Ok(self.b.inst_results(inst)[0])
            }
            // Unbox a handle into a scalar.
            (ValTy::Ref, _) => {
                let sym = match to {
                    ValTy::Int => "leek_unbox_int",
                    ValTy::Bool => "leek_unbox_bool",
                    ValTy::Real => "leek_unbox_real",
                    ValTy::Ref => unreachable!(),
                };
                let fref = self.imports.rt(sym)?;
                let inst = self.b.ins().call(fref, &[v]);
                Ok(self.b.inst_results(inst)[0])
            }
            (ValTy::Int | ValTy::Bool, ValTy::Real) => {
                Ok(self.b.ins().fcvt_from_sint(types::F64, v))
            }
            // int↔bool share the i64 repr.
            (ValTy::Int, ValTy::Bool) | (ValTy::Bool, ValTy::Int) => Ok(v),
            (ValTy::Real, ValTy::Int) => Ok(self.b.ins().fcvt_to_sint_sat(types::I64, v)),
            (ValTy::Real, ValTy::Bool) => Err(unsupported("real → bool coercion")),
            _ => Ok(v),
        }
    }

    /// True if an operand's static kind is an unboxed `integer` — an `Int`
    /// local or an integer literal. Used to gate the unboxed-index array reads
    /// (`leek_index_int` / `leek_array_get_*`), which take the index as a raw
    /// `i64`.
    pub(super) fn operand_int_kind(&self, op: &Operand) -> bool {
        match op {
            Operand::Local(id) => self.var_tys[id.0 as usize] == ValTy::Int,
            Operand::Const(Const::Int(_)) => true,
            _ => false,
        }
    }

    /// Read `base[idx]` directly into an unboxed scalar (`target` is `Int` or
    /// `Real`), for an indexing whose result flows into a scalar-typed slot.
    /// `read_index_versioned(..).to_long()/.to_real()` is exactly what the
    /// destination's `coerce(Ref → Int/Real)` (i.e. `leek_unbox_int/real`)
    /// would compute on the boxed read, so the value is byte-identical — but
    /// neither the index nor the result is ever boxed. The caller guarantees
    /// `idx` is a statically-integer operand and `base` is not a class-ref.
    pub(super) fn index_unboxed(
        &mut self,
        base: LocalId,
        idx: &Operand,
        target: ValTy,
    ) -> Result<Value, NativeError> {
        let arr = {
            let (v, base_ty) = self.local_value(base)?;
            self.coerce(v, base_ty, ValTy::Ref)?
        };
        let (i, _it) = self.operand(idx)?;
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        let sym = if target == ValTy::Real {
            "leek_array_get_real"
        } else {
            "leek_array_get_int"
        };
        let f = self.imports.rt(sym)?;
        let inst = self.b.ins().call(f, &[arr, i, ver]);
        Ok(self.b.inst_results(inst)[0])
    }

    /// True if an operand's static kind is a boxed `Ref` (a dynamic value) —
    /// a `Ref`/cell local, or a null/string constant.
    pub(super) fn operand_is_ref(&self, op: &Operand) -> bool {
        match op {
            Operand::Local(id) => self.var_tys[id.0 as usize] == ValTy::Ref,
            Operand::Const(Const::Null | Const::String(_) | Const::BigInt(_)) => true,
            _ => false,
        }
    }

    /// A dummy value of kind `ty` for an omitted defaulted call argument — the
    /// callee overwrites it (running the param's `default_init`) before the
    /// body runs, so the value is never observed. A `Ref` uses a valid boxed
    /// null handle (not a 0 pointer) in case of a stray read.
    pub(super) fn placeholder(&mut self, ty: ValTy) -> Value {
        match ty {
            ValTy::Real => self.b.ins().f64const(0.0),
            ValTy::Ref => {
                // `leek_box_null` is always declared (a composite shim).
                let f = self
                    .imports
                    .rt("leek_box_null")
                    .expect("leek_box_null shim declared");
                let inst = self.b.ins().call(f, &[]);
                self.b.inst_results(inst)[0]
            }
            _ => self.b.ins().iconst(types::I64, 0),
        }
    }

    /// `value != 0` as an i64 condition for `brif`.
    pub(super) fn truthy(&mut self, v: Value, ty: ValTy) -> Value {
        match ty {
            ValTy::Real => {
                let zero = self.b.ins().f64const(0.0);
                self.b.ins().fcmp(FloatCC::NotEqual, v, zero)
            }
            _ => v,
        }
    }

    /// Truthiness of `v` as a 1-bit value (`value != 0`).
    pub(super) fn emit_i1(&mut self, v: Value, ty: ValTy) -> Value {
        if ty == ValTy::Real {
            let zero = self.b.ins().f64const(0.0);
            self.b.ins().fcmp(FloatCC::NotEqual, v, zero)
        } else {
            let zero = self.b.ins().iconst(types::I64, 0);
            self.b.ins().icmp(IntCC::NotEqual, v, zero)
        }
    }

    pub(super) fn binary(
        &mut self,
        op: BinOp,
        l: &Operand,
        r: &Operand,
    ) -> Result<(Value, ValTy), NativeError> {
        // Charge the binary op's cost (interp charges this for every `Binary`
        // rvalue, before evaluating — so it applies even to the v1 div-by-zero
        // early return below). String-concat's per-char surcharge is handled in
        // the boxed `leek_value_binop` path, which measures the result at
        // runtime; no `.ops` corpus case exercises string concat.
        self.charge(op.op_cost())?;
        self.binary_uncharged(op, l, r)
    }

    /// [`Self::binary`] without the op charge — for compiler-synthesized
    /// ops (the foreach machinery's `pos < len` test and `pos + 1` step),
    /// whose upstream equivalents never tick the budget.
    pub(super) fn binary_uncharged(
        &mut self,
        op: BinOp,
        l: &Operand,
        r: &Operand,
    ) -> Result<(Value, ValTy), NativeError> {
        // v1 real division by a statically-zero divisor yields `null`, not
        // `±∞` — produce a boxed-null `Ref` (so `8 / 0 === null` is true and
        // `0 / 0 === NaN` is false), rather than emitting an infinity v1
        // never produces.
        if self.lang.version == 1 && matches!(op, BinOp::Div) && is_const_zero(r) {
            // Use the compile-time leaked null handle (an `iconst`), not the
            // `leek_box_null` shim — so this works even in an otherwise
            // composite-free function.
            let f = self.imports.rt("leek_box_null")?;
            let inst = self.b.ins().call(f, &[]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        let (a, lt) = self.operand(l)?;
        let (b, rt) = self.operand(r)?;

        // Arithmetic / comparison involving a dynamically-typed (boxed)
        // operand — e.g. an array element — dispatches at runtime through
        // the shared `apply_binary`, matching the interpreter exactly.
        if lt == ValTy::Ref || rt == ValTy::Ref {
            let code = self
                .b
                .ins()
                .iconst(types::I64, crate::runtime::binop_code(op));
            let ver = self
                .b
                .ins()
                .iconst(types::I64, i64::from(self.lang.version));
            // Fast path: when exactly one operand is dynamic (`Ref`) and the
            // other is a statically-`integer`/`real` value (literal OR a typed
            // local), pass that scalar by value to a `_ci{r,l}` / `_cr{r,l}`
            // shim instead of heap-boxing it on every evaluation (`arr[i] - 1`,
            // `boxed + x`, …). The shim rebuilds the identical `Value::Int` /
            // `Value::Real` on the stack, so the result is unchanged — only the
            // scalar's per-op allocation is removed. `Bool` keeps the boxed
            // path (its `Value::Bool` dispatch differs from an int rebuild).
            let res = if lt == ValTy::Ref && rt == ValTy::Int {
                let a = self.coerce(a, lt, ValTy::Ref)?;
                let cir = self.imports.rt("leek_value_binop_cir")?;
                let inst = self.b.ins().call(cir, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            } else if lt == ValTy::Ref && rt == ValTy::Real {
                let a = self.coerce(a, lt, ValTy::Ref)?;
                let crr = self.imports.rt("leek_value_binop_crr")?;
                let inst = self.b.ins().call(crr, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            } else if rt == ValTy::Ref && lt == ValTy::Int {
                let b = self.coerce(b, rt, ValTy::Ref)?;
                let cil = self.imports.rt("leek_value_binop_cil")?;
                let inst = self.b.ins().call(cil, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            } else if rt == ValTy::Ref && lt == ValTy::Real {
                let b = self.coerce(b, rt, ValTy::Ref)?;
                let crl = self.imports.rt("leek_value_binop_crl")?;
                let inst = self.b.ins().call(crl, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            } else {
                let binop = self.imports.rt("leek_value_binop")?;
                let a = self.coerce(a, lt, ValTy::Ref)?;
                let b = self.coerce(b, rt, ValTy::Ref)?;
                let inst = self.b.ins().call(binop, &[code, a, b, ver]);
                self.b.inst_results(inst)[0]
            };
            // String concat's dynamic surcharge (number→string conversion +
            // per-char cost) is metered inside the `leek_value_binop*` shims
            // themselves (`apply_binop_charged`), where both operand values
            // are visible — nothing extra to emit here.
            return Ok((res, ValTy::Ref));
        }

        // `xor` is logical xor of truthiness in every version — works on
        // any scalar mix.
        if matches!(op, BinOp::Xor) {
            let ba = self.emit_i1(a, lt);
            let bb = self.emit_i1(b, rt);
            let x = self.b.ins().bxor(ba, bb);
            return Ok((self.b.ins().uextend(types::I64, x), ValTy::Bool));
        }

        // Equality across an int and a bool (the only non-equal,
        // non-real scalar pairing). Two behaviors:
        //   * `===` / `!==` (and `==` / `!=` in v4) are type-sensitive:
        //     differing types ⇒ structurally false / true.
        //   * `==` / `!=` in v1–v3 coerce by *truthiness*
        //     (`true == 12` is true, `false == 0` is true).
        if matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::IdentityEq | BinOp::IdentityNe
        ) && lt != rt
            && lt != ValTy::Real
            && rt != ValTy::Real
        {
            let identity = matches!(op, BinOp::IdentityEq | BinOp::IdentityNe);
            let want_eq = matches!(op, BinOp::Eq | BinOp::IdentityEq);
            if identity || self.lang.version >= 4 {
                let c = self.b.ins().iconst(types::I64, i64::from(!want_eq));
                return Ok((c, ValTy::Bool));
            }
            // v1–v3 truthiness comparison.
            let ba = self.emit_i1(a, lt);
            let bb = self.emit_i1(b, rt);
            let cc = if want_eq {
                IntCC::Equal
            } else {
                IntCC::NotEqual
            };
            let bit = self.b.ins().icmp(cc, ba, bb);
            return Ok((self.b.ins().uextend(types::I64, bit), ValTy::Bool));
        }

        // `^=` compound assignment is POWER-assign in v1 (`x ^= n` ≡
        // `x = x ** n`) and XOR-assign in v2+ (`x = x ^ n`). The v1 form
        // shares the `**` path below; the v2+ form is integer bitwise xor
        // (handled in the integer match arm). Standalone `^` is `BitXor`.
        let pow_like = matches!(op, BinOp::Pow)
            || (matches!(op, BinOp::CompoundXor) && self.lang.version <= 1);

        // `**` power. Real if either side is real; otherwise integer
        // power, but only when the exponent is a constant in `[0, 64)`
        // (the interp's int-pow range — outside it the result kind
        // becomes data-dependent, so we skip).
        if pow_like {
            if lt == ValTy::Real || rt == ValTy::Real {
                let fref = self
                    .imports
                    .pow_real
                    .ok_or_else(|| unsupported("pow import not declared"))?;
                let a = self.coerce(a, lt, ValTy::Real)?;
                let b = self.coerce(b, rt, ValTy::Real)?;
                let inst = self.b.ins().call(fref, &[a, b]);
                return Ok((self.b.inst_results(inst)[0], ValTy::Real));
            }
            match const_pow_exp(r) {
                Some(e) if (0..64).contains(&e) => {
                    let fref = self
                        .imports
                        .pow_int
                        .ok_or_else(|| unsupported("ipow import not declared"))?;
                    let inst = self.b.ins().call(fref, &[a, b]);
                    return Ok((self.b.inst_results(inst)[0], ValTy::Int));
                }
                _ => return Err(unsupported("integer ** with non-constant/large exponent")),
            }
        }

        // Bitwise / shift ops are integer-only: a real operand truncates to
        // an integer (matching the interpreter), so they never take the real
        // path even when an operand is typed `real`.
        let bitwise = matches!(
            op,
            BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
                | BinOp::CompoundXor
                | BinOp::ShiftL
                | BinOp::ShiftR
                | BinOp::UShiftR
        );
        let real = !bitwise && (lt == ValTy::Real || rt == ValTy::Real || matches!(op, BinOp::Div));

        if real {
            let a = self.coerce(a, lt, ValTy::Real)?;
            let b = self.coerce(b, rt, ValTy::Real)?;
            let ins = self.b.ins();
            let (v, ty) = match op {
                BinOp::Add => (ins.fadd(a, b), ValTy::Real),
                BinOp::Sub => (ins.fsub(a, b), ValTy::Real),
                BinOp::Mul => (ins.fmul(a, b), ValTy::Real),
                BinOp::Div => (ins.fdiv(a, b), ValTy::Real),
                // `===` / `!==` on numbers compares numerically, like
                // `==` / `!=` (`1 === 1.0` is true).
                BinOp::Eq | BinOp::IdentityEq => return Ok(self.fcmp(FloatCC::Equal, a, b)),
                BinOp::Ne | BinOp::IdentityNe => return Ok(self.fcmp(FloatCC::NotEqual, a, b)),
                BinOp::Lt => return Ok(self.fcmp(FloatCC::LessThan, a, b)),
                BinOp::Le => return Ok(self.fcmp(FloatCC::LessThanOrEqual, a, b)),
                BinOp::Gt => return Ok(self.fcmp(FloatCC::GreaterThan, a, b)),
                BinOp::Ge => return Ok(self.fcmp(FloatCC::GreaterThanOrEqual, a, b)),
                other => return Err(unsupported(format!("real binary op {other:?}"))),
            };
            return Ok((v, ty));
        }

        // Integer path: coerce both operands to `Int` (a no-op when already
        // integer/bool; truncates a `real` operand for the bitwise ops routed
        // here).
        let a = self.coerce(a, lt, ValTy::Int)?;
        let b = self.coerce(b, rt, ValTy::Int)?;
        let ins = self.b.ins();
        let (v, ty) = match op {
            BinOp::Add => (ins.iadd(a, b), ValTy::Int),
            BinOp::Sub => (ins.isub(a, b), ValTy::Int),
            BinOp::Mul => (ins.imul(a, b), ValTy::Int),
            BinOp::IntDiv => (ins.sdiv(a, b), ValTy::Int),
            BinOp::Mod => (ins.srem(a, b), ValTy::Int),
            BinOp::BitAnd => (ins.band(a, b), ValTy::Int),
            BinOp::BitOr => (ins.bor(a, b), ValTy::Int),
            BinOp::BitXor => (ins.bxor(a, b), ValTy::Int),
            // v2+ `^=` xor-assign (v1 power-assign took the `**` path above).
            BinOp::CompoundXor => (ins.bxor(a, b), ValTy::Int),
            BinOp::ShiftL => (ins.ishl(a, b), ValTy::Int),
            BinOp::ShiftR => (ins.sshr(a, b), ValTy::Int),
            BinOp::UShiftR => (ins.ushr(a, b), ValTy::Int),
            BinOp::Eq | BinOp::IdentityEq => return Ok(self.icmp(IntCC::Equal, a, b)),
            BinOp::Ne | BinOp::IdentityNe => return Ok(self.icmp(IntCC::NotEqual, a, b)),
            BinOp::Lt => return Ok(self.icmp(IntCC::SignedLessThan, a, b)),
            BinOp::Le => return Ok(self.icmp(IntCC::SignedLessThanOrEqual, a, b)),
            BinOp::Gt => return Ok(self.icmp(IntCC::SignedGreaterThan, a, b)),
            BinOp::Ge => return Ok(self.icmp(IntCC::SignedGreaterThanOrEqual, a, b)),
            other => return Err(unsupported(format!("binary op {other:?}"))),
        };
        Ok((v, ty))
    }

    pub(super) fn icmp(&mut self, cc: IntCC, a: Value, b: Value) -> (Value, ValTy) {
        let bit = self.b.ins().icmp(cc, a, b);
        (self.b.ins().uextend(types::I64, bit), ValTy::Bool)
    }

    pub(super) fn fcmp(&mut self, cc: FloatCC, a: Value, b: Value) -> (Value, ValTy) {
        let bit = self.b.ins().fcmp(cc, a, b);
        (self.b.ins().uextend(types::I64, bit), ValTy::Bool)
    }

    pub(super) fn unary(&mut self, op: UnOp, x: &Operand) -> Result<(Value, ValTy), NativeError> {
        // Upstream's emitter charges 1 op per source-level unary operator,
        // except `+x` and `@x` which are free (`unary_op_cost`). Charged
        // before evaluation, mirroring `binary`.
        if matches!(op, UnOp::Neg | UnOp::Not | UnOp::BitNot) {
            self.charge(1)?;
        }
        let (v, ty) = self.operand(x)?;
        if ty == ValTy::Ref {
            match op {
                // `!dynamic` uses the shared truthiness shim.
                UnOp::Not => {
                    let f = self.imports.rt("leek_truthy")?;
                    let inst = self.b.ins().call(f, &[v]);
                    let t = self.b.inst_results(inst)[0];
                    let zero = self.b.ins().iconst(types::I64, 0);
                    let bit = self.b.ins().icmp(IntCC::Equal, t, zero);
                    return Ok((self.b.ins().uextend(types::I64, bit), ValTy::Bool));
                }
                // `-dynamic` / `~dynamic` go through the shared unary shim,
                // returning a new boxed value.
                UnOp::Neg | UnOp::BitNot => {
                    let code = i64::from(op != UnOp::Neg);
                    let f = self.imports.rt("leek_value_unary")?;
                    let code_v = self.b.ins().iconst(types::I64, code);
                    let inst = self.b.ins().call(f, &[code_v, v]);
                    return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
                }
                _ => return Err(unsupported("unary operator on dynamic (boxed) value")),
            }
        }
        match op {
            UnOp::Neg if ty == ValTy::Real => Ok((self.b.ins().fneg(v), ValTy::Real)),
            UnOp::Neg => Ok((self.b.ins().ineg(v), ValTy::Int)),
            UnOp::BitNot if ty == ValTy::Real => Err(unsupported("bitnot on real")),
            UnOp::BitNot => Ok((self.b.ins().bnot(v), ValTy::Int)),
            UnOp::Pos | UnOp::Ref => Ok((v, ty)),
            UnOp::Not => {
                let bit = if ty == ValTy::Real {
                    let zero = self.b.ins().f64const(0.0);
                    self.b.ins().fcmp(FloatCC::Equal, v, zero)
                } else {
                    let zero = self.b.ins().iconst(types::I64, 0);
                    self.b.ins().icmp(IntCC::Equal, v, zero)
                };
                Ok((self.b.ins().uextend(types::I64, bit), ValTy::Bool))
            }
        }
    }
}
