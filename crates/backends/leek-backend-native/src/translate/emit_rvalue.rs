//! Rvalue emission: the per-rvalue dispatch plus literals, fields, intervals, slices, and indexing.

use super::{
    Const, DefId, InstBuilder, LocalId, NativeError, Operand, Rvalue, StackSlotData, StackSlotKind,
    Tx, ValTy, Value, class_reflect, coerce_target_ty, program_writes_global,
    resolve_instance_method_value, resolve_static_field, resolve_static_method_value, rvalue_name,
    types, unsupported,
};

impl Tx<'_, '_> {
    pub(super) fn rvalue(&mut self, rv: &Rvalue) -> Result<(Value, ValTy), NativeError> {
        match rv {
            Rvalue::Use(op) | Rvalue::UseFresh(op) => self.operand(op),
            Rvalue::Binary(op, l, r) => self.binary(*op, l, r),
            Rvalue::Unary(op, x) => self.unary(*op, x),
            Rvalue::Array(elems) => self.array_literal(elems),
            Rvalue::Index(base, idx) => self.index(*base, idx),
            Rvalue::Slice(base, bounds) => self.slice(*base, bounds),
            Rvalue::MakeForeachIter(op) => self.foreach_iter(op),
            Rvalue::Map(pairs) => self.map_literal(pairs),
            Rvalue::Set(items) => self.set_literal(items),
            Rvalue::Interval(iv) => self.interval(iv),
            Rvalue::Object(fields) => self.object_literal(fields),
            Rvalue::Field(base, name) => self.field(*base, name),
            Rvalue::GlobalRef(_, name) => self.global_get(name),
            Rvalue::New { class, args } => self.new_instance(class, args),
            // `x -> …` / `function(){}`: build a closure value capturing the
            // current capture values (value-capture snapshot).
            Rvalue::MakeLambda {
                function_idx,
                captures,
            } => {
                let (ptr, n) = self.build_ref_array_opt(captures, true)?;
                let f = self.imports.rt("leek_make_lambda")?;
                let fidx = self.b.ins().iconst(types::I64, *function_idx as i64);
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let inst = self.b.ins().call(f, &[fidx, ptr, nc]);
                Ok((self.b.inst_results(inst)[0], ValTy::Ref))
            }
            // A bare builtin name used as a value. A *constant* (`PI`,
            // `SORT_ASC`, …) boxes its value directly; a builtin *function*
            // (`arrayMap(arr, abs)`) boxes a `Function::Builtin` handle the
            // runtime's HOF dispatch (`dispatch_call_value`) invokes by
            // name. Unknown names skip.
            Rvalue::BuiltinRef(name) => {
                if let Some(v) = leek_runtime::lookup_constant(name) {
                    let ptr = crate::runtime::box_value(v) as i64;
                    Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
                } else if let Some(cls) = leek_runtime::builtin_class_name(name) {
                    // A builtin class used as a value (`var c = Array`,
                    // `x instanceof Map`, `Integer.MAX_VALUE`). Box a
                    // `BuiltinClass` handle — the shared `value_instanceof`,
                    // `read_field` (statics) and `construct_builtin_class`
                    // all dispatch on it.
                    let v = leek_runtime::Value::BuiltinClass(cls);
                    let ptr = crate::runtime::box_value(v) as i64;
                    Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
                } else if leek_runtime::is_known_builtin(name) {
                    // A user global with this name shadows the builtin
                    // (`abs = 2; return abs`, or `var _c = count; count = …`):
                    // the reference resolves dynamically — the global's value if
                    // one has been assigned, else the builtin handle. Defer to
                    // the `leek_ref_or_builtin` shim.
                    if program_writes_global(self.program, name) {
                        let name_h = self.const_string(name)?;
                        let f = self.imports.rt("leek_ref_or_builtin")?;
                        let inst = self.b.ins().call(f, &[name_h]);
                        return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
                    }
                    let v = leek_runtime::Value::Function(leek_runtime::Function::Builtin(
                        name.clone(),
                    ));
                    let ptr = crate::runtime::box_value(v) as i64;
                    Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
                } else {
                    // An unresolved bare name (not a constant/class/builtin) —
                    // a global declared elsewhere (`global x`) or an undefined
                    // reference. The interpreter reads the name-keyed global
                    // store, falling back to null; `leek_global_get` does
                    // exactly that.
                    let name_h = self.const_string(name)?;
                    let f = self.imports.rt("leek_global_get")?;
                    let inst = self.b.ins().call(f, &[name_h]);
                    Ok((self.b.inst_results(inst)[0], ValTy::Ref))
                }
            }
            // A class used as a value (`var c = C`, or the `C.staticMethod()`
            // receiver). The class identity is known at compile time, so box
            // it once into a leaked handle (like a string literal).
            Rvalue::ClassRef(def_id, name) => {
                let v = leek_runtime::Value::ClassRef(*def_id, std::rc::Rc::new(name.clone()));
                let ptr = crate::runtime::box_value(v) as i64;
                Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
            }
            // A named function used as a value (`var f = foo`). Boxed as a
            // `Function::User` handle; invoked via the indirect-call path
            // (`USER_FN_IDX` → uniform body), so the function is uniform-
            // compiled + registered in `define_program`.
            Rvalue::FunctionRef(def_id) => {
                let v = leek_runtime::Value::Function(leek_runtime::Function::User(*def_id));
                let ptr = crate::runtime::box_value(v) as i64;
                Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref))
            }
            // `super` evaluates to the same instance as `this`; the parent-class
            // dispatch happens at the method-call site (via `super_locals`).
            // Field access `super.field` then reads `this`'s field storage.
            Rvalue::MakeSuper { this, .. } => {
                let (v, t) = self.local_value(*this)?;
                let boxed = self.coerce(v, t, ValTy::Ref)?;
                Ok((boxed, ValTy::Ref))
            }
            // `expr as T` (and implicit numeric/bool/string coercions). Box the
            // operand and run the shared `apply_cast` (matching the interp);
            // the boxed result is coerced to the destination kind by the
            // consuming local's declared type.
            Rvalue::Cast(kind, x) => {
                let (v, t) = self.operand(x)?;
                let boxed = self.coerce(v, t, ValTy::Ref)?;
                let code: i64 = match kind {
                    leek_mir::ir::CastKind::IntToReal => 0,
                    leek_mir::ir::CastKind::RealToInt => 1,
                    leek_mir::ir::CastKind::ToBool => 2,
                    leek_mir::ir::CastKind::ToString => 3,
                    leek_mir::ir::CastKind::User => 4,
                };
                let f = self.imports.rt("leek_apply_cast")?;
                let codev = self.b.ins().iconst(types::I64, code);
                let inst = self.b.ins().call(f, &[codev, boxed]);
                Ok((self.b.inst_results(inst)[0], ValTy::Ref))
            }
            other => Err(unsupported(format!("rvalue {}", rvalue_name(other)))),
        }
    }

    /// Build an object literal `{f: v, …}`: allocate, then set each field
    /// (reusing the generic index-set, which routes objects to `set_field`).
    pub(super) fn object_literal(
        &mut self,
        fields: &[(String, Operand)],
    ) -> Result<(Value, ValTy), NativeError> {
        let new = self.imports.rt("leek_object_new")?;
        let set = self.imports.rt("leek_value_set_index")?;
        let inst = self.b.ins().call(new, &[]);
        let obj = self.b.inst_results(inst)[0];
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        for (name, op) in fields {
            let key = self.const_string(name)?;
            let (v, t) = self.operand(op)?;
            let val = self.coerce(v, t, ValTy::Ref)?;
            self.b.ins().call(set, &[obj, key, val, ver]);
        }
        Ok((obj, ValTy::Ref))
    }

    /// `base.field` read — a string-keyed index (`read_index` handles
    /// objects/instances via `read_field`).
    /// Emit a boxed-null handle.
    pub(super) fn boxed_null(&mut self) -> Result<Value, NativeError> {
        let null = self.imports.rt("leek_box_null")?;
        let inst = self.b.ins().call(null, &[]);
        Ok(self.b.inst_results(inst)[0])
    }

    /// Emit a `leek_static_get(owner_def, name)` — lazily-initialised
    /// per-class static-field storage. `decl_ty` is the field's declared
    /// type; a scalar one coerces the stored value (so `real? a = 12` reads
    /// back `12.0`).
    pub(super) fn static_field_get(
        &mut self,
        owner: DefId,
        name: &str,
        coerce: Option<ValTy>,
    ) -> Result<Value, NativeError> {
        let get = self.imports.rt("leek_static_get")?;
        let cd = self.b.ins().iconst(types::I64, owner.0 as i64);
        let key = self.const_string(name)?;
        let inst = self.b.ins().call(get, &[cd, key]);
        let mut v = self.b.inst_results(inst)[0];
        if let Some(kind) = coerce {
            let coerce = self.imports.rt("leek_coerce_scalar")?;
            let k = self.b.ins().iconst(
                types::I64,
                match kind {
                    ValTy::Int => 0,
                    ValTy::Real => 1,
                    _ => 2,
                },
            );
            let inst = self.b.ins().call(coerce, &[v, k]);
            v = self.b.inst_results(inst)[0];
        }
        Ok(v)
    }

    /// Emit a `leek_static_set(owner_def, name, val)`.
    pub(super) fn static_field_set(
        &mut self,
        owner: DefId,
        name: &str,
        val: Value,
    ) -> Result<(), NativeError> {
        let set = self.imports.rt("leek_static_set")?;
        let cd = self.b.ins().iconst(types::I64, owner.0 as i64);
        let key = self.const_string(name)?;
        self.b.ins().call(set, &[cd, key, val]);
        Ok(())
    }

    pub(super) fn field(
        &mut self,
        base: LocalId,
        name: &str,
    ) -> Result<(Value, ValTy), NativeError> {
        // `C.super` on a class reference is C's parent class. When the base is
        // a known class-ref with an explicit user parent (`parent_def`), box it
        // at compile time. Otherwise (`x.class.super`, or a class with no
        // explicit parent → implicit builtin `Value` base) resolve at runtime
        // via `leek_class_super` (backed by the `CLASS_PARENT` table).
        if name == "super" {
            if let Some(cls) = self.classref_locals.get(&base).cloned()
                && let Some(c) = self.program.class_by_name(&cls)
                && let Some(pdef) = c.parent_def
                && let Some(pc) = self.program.class(pdef)
            {
                let v = leek_runtime::Value::ClassRef(pdef, std::rc::Rc::new(pc.name.clone()));
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            let (v, t) = self.local_value(base)?;
            let boxed = self.coerce(v, t, ValTy::Ref)?;
            let f = self.imports.rt("leek_class_super")?;
            let inst = self.b.ins().call(f, &[boxed]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        // `.class` is a meta-property (the runtime class) available on ANY
        // value, including scalars — box the base and ask the shared
        // `class_of`.
        if name == "class" {
            let (v, t) = self.local_value(base)?;
            let boxed = self.coerce(v, t, ValTy::Ref)?;
            let f = self.imports.rt("leek_class_of")?;
            let inst = self.b.ins().call(f, &[boxed]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        // `C.member` — a class reference's member.
        if let Some(cls) = self.classref_locals.get(&base).cloned() {
            // `C.name` is the class name (a compile-time-known string).
            if name == "name" {
                let v = leek_runtime::Value::String(std::rc::Rc::new(cls));
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            // A static field reads from per-class storage (lazily initialised).
            if let Some((owner, fld)) = resolve_static_field(self.program, &cls, name) {
                if !self.method_visible(owner, fld.visibility) {
                    return Ok((self.boxed_null()?, ValTy::Ref));
                }
                let coerce = coerce_target_ty(&fld.ty);
                let v = self.static_field_get(owner, name, coerce)?;
                return Ok((v, ValTy::Ref));
            }
            // Reflective members (`C.fields`, `C.methods`, …) are known at
            // compile time — materialise the array as a leaked handle.
            if let Some(v) = class_reflect(self.program, &cls, name) {
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            // A static method read as a *value* (`var f = C.staticMethod`):
            // box a `Function::User` handle (the method is uniform-compiled +
            // registered in `USER_FN_IDX` via `static_method_value_info`), so
            // it can be invoked indirectly through `leek_call_value`.
            if let Some(idx) = resolve_static_method_value(self.program, &cls, name)
                && let Some(def) = self.program.functions[idx].def_id
            {
                let v = leek_runtime::Value::Function(leek_runtime::Function::User(def));
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            // An *instance* method read via the class ref (`var f = A.m`) is an
            // unbound method value — invoked as `f(receiver, …)`, the receiver
            // becomes the method's first (`this`) parameter. Box the same
            // `Function::User` handle (the method is uniform-compiled +
            // registered via `static_method_value_refs`).
            if let Some(idx) = resolve_instance_method_value(self.program, &cls, name)
                && let Some(def) = self.program.functions[idx].def_id
            {
                let v = leek_runtime::Value::Function(leek_runtime::Function::User(def));
                let ptr = crate::runtime::box_value(v) as i64;
                return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
            }
            // Other class members aren't representable here — skip.
            return Err(unsupported("class reference member access"));
        }
        if self.var_tys[base.0 as usize] != ValTy::Ref {
            return Err(unsupported("field of non-object"));
        }
        // `obj.m` where `m` is a method (not a stored field) is a bound-method
        // value. It falls through to `leek_value_index`, which builds the
        // `BoundMethod` from `METHOD_RESOLVE` — seeded for `obj.m` field reads
        // by `index_method_targets` (so the method is reachable + uniform-
        // compiled). The receiver is captured, so calling it prepends `obj`.
        let slot = self.field_slot_of(base, name);
        let (base_h, _) = self.local_value(base)?;
        let v = self.field_get_boxed(base_h, name, slot)?;
        Ok((v, ValTy::Ref))
    }

    /// Read a file-level global by name.
    pub(super) fn global_get(&mut self, name: &str) -> Result<(Value, ValTy), NativeError> {
        let get = self.imports.rt("leek_global_get")?;
        let key = self.const_string(name)?;
        let inst = self.b.ins().call(get, &[key]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Build an interval `[start..end]`. Endpoints are boxed (a null
    /// handle marks an unbounded end); inclusivity / forces-real bits are
    /// packed into `flags`. `step` is ignored (as in the interpreter).
    pub(super) fn interval(
        &mut self,
        iv: &leek_mir::ir::IntervalRvalue,
    ) -> Result<(Value, ValTy), NativeError> {
        // Interval-literal construction costs 2 ops (interp `exec.rs`).
        self.charge(2)?;
        let f = self.imports.rt("leek_interval")?;
        let bound = |this: &mut Self, op: &Option<Operand>| -> Result<Value, NativeError> {
            match op {
                Some(o) => {
                    let (v, t) = this.operand(o)?;
                    this.coerce(v, t, ValTy::Ref)
                }
                None => Ok(this.b.ins().iconst(types::I64, 0)), // null handle
            }
        };
        let start = bound(self, &iv.start)?;
        let end = bound(self, &iv.end)?;
        let flags = (iv.start_inclusive as i64)
            | ((iv.end_inclusive as i64) << 1)
            | ((iv.start_forces_real as i64) << 2)
            | ((iv.end_forces_real as i64) << 3);
        let flags = self.b.ins().iconst(types::I64, flags);
        let inst = self.b.ins().call(f, &[start, end, flags]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Build a map literal: allocate, then box-and-insert each key/value.
    pub(super) fn map_literal(
        &mut self,
        pairs: &[(Operand, Operand)],
    ) -> Result<(Value, ValTy), NativeError> {
        let new = self.imports.rt("leek_map_new")?;
        let put = self.imports.rt("leek_map_put")?;
        let inst = self.b.ins().call(new, &[]);
        let map = self.b.inst_results(inst)[0];
        for (k, v) in pairs {
            let (kv, kt) = self.operand(k)?;
            let key = self.coerce(kv, kt, ValTy::Ref)?;
            let (vv, vt) = self.operand(v)?;
            let val = self.coerce(vv, vt, ValTy::Ref)?;
            self.b.ins().call(put, &[map, key, val]);
        }
        Ok((map, ValTy::Ref))
    }

    /// Build a set literal: allocate, then box-and-add each element.
    pub(super) fn set_literal(&mut self, items: &[Operand]) -> Result<(Value, ValTy), NativeError> {
        // Set-literal construction costs 2 ops per element (interp `exec.rs`).
        self.charge(2 * items.len() as u64)?;
        let new = self.imports.rt("leek_set_new")?;
        let add = self.imports.rt("leek_set_add")?;
        let inst = self.b.ins().call(new, &[]);
        let set = self.b.inst_results(inst)[0];
        for o in items {
            let (v, t) = self.operand(o)?;
            let elem = self.coerce(v, t, ValTy::Ref)?;
            self.b.ins().call(add, &[set, elem]);
        }
        Ok((set, ValTy::Ref))
    }

    /// Build a `foreach` iterator handle (`[key, value]` pairs) from an
    /// iterable. The loop body then indexes it: `iter[i]` is a pair,
    /// `iter[i][1]` the value — both lower through `index`.
    pub(super) fn foreach_iter(&mut self, op: &Operand) -> Result<(Value, ValTy), NativeError> {
        let f = self.imports.rt("leek_foreach_iter")?;
        let (v, t) = self.operand(op)?;
        let h = self.coerce(v, t, ValTy::Ref)?;
        let inst = self.b.ins().call(f, &[h]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Build an array literal: allocate, then box-and-push each element.
    /// Composite support is v4-only (v1–v3 arrays are value-typed, which
    /// this aliasing handle model doesn't reproduce).
    pub(super) fn array_literal(
        &mut self,
        elems: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        let new = self.imports.rt("leek_array_new")?;
        let push = self.imports.rt("leek_array_push")?;
        let inst = self.b.ins().call(new, &[]);
        let arr = self.b.inst_results(inst)[0];
        for op in elems {
            let (v, t) = self.operand(op)?;
            let boxed = self.coerce(v, t, ValTy::Ref)?;
            self.b.ins().call(push, &[arr, boxed]);
        }
        Ok((arr, ValTy::Ref))
    }

    /// `base[idx]` read for any indexable handle (array / string / map /
    /// …). The result is a handle to the element (out-of-range → `null`).
    /// `base[start:end:step]` — slice an array / string / interval. Each
    /// bound is boxed (a null handle marks an absent bound).
    pub(super) fn slice(
        &mut self,
        base: LocalId,
        bounds: &leek_mir::ir::SliceBounds,
    ) -> Result<(Value, ValTy), NativeError> {
        if self.var_tys[base.0 as usize] != ValTy::Ref {
            return Err(unsupported("slice of non-composite"));
        }
        let f = self.imports.rt("leek_slice")?;
        let (base_h, _) = self.local_value(base)?;
        let bound = |this: &mut Self, op: &Option<Operand>| -> Result<Value, NativeError> {
            match op {
                Some(o) => {
                    let (v, t) = this.operand(o)?;
                    this.coerce(v, t, ValTy::Ref)
                }
                None => Ok(this.boxed_null()?),
            }
        };
        let start = bound(self, &bounds.start)?;
        let end = bound(self, &bounds.end)?;
        let step = bound(self, &bounds.step)?;
        let inst = self.b.ins().call(f, &[base_h, start, end, step]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    pub(super) fn index(
        &mut self,
        base: LocalId,
        idx: &Operand,
    ) -> Result<(Value, ValTy), NativeError> {
        // `C['member']` indexes a class reference — resolved like `C.member`
        // (the interpreter treats index and field member access identically).
        if let Some(cls) = self.classref_locals.get(&base).cloned() {
            if let Operand::Const(Const::String(name)) = idx {
                // `C['name']` — the class name.
                if name.as_str() == "name" {
                    let v = leek_runtime::Value::String(std::rc::Rc::new(cls.clone()));
                    let ptr = crate::runtime::box_value(v) as i64;
                    return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
                }
                // A static field.
                if let Some((owner, fld)) = resolve_static_field(self.program, &cls, name) {
                    if !self.method_visible(owner, fld.visibility) {
                        return Ok((self.boxed_null()?, ValTy::Ref));
                    }
                    let coerce = coerce_target_ty(&fld.ty);
                    let v = self.static_field_get(owner, name, coerce)?;
                    return Ok((v, ValTy::Ref));
                }
                // A reflective member (`C['fields']`, …).
                if let Some(v) = class_reflect(self.program, &cls, name) {
                    let ptr = crate::runtime::box_value(v) as i64;
                    return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
                }
                // A static or instance method read as a value (`A['m']`) — box
                // a `Function::User` handle (uniform-compiled + registered via
                // `static_method_value_refs`, which scans string-`Index` reads).
                if let Some(idx) = resolve_static_method_value(self.program, &cls, name)
                    .or_else(|| resolve_instance_method_value(self.program, &cls, name))
                    && let Some(def) = self.program.functions[idx].def_id
                {
                    let v = leek_runtime::Value::Function(leek_runtime::Function::User(def));
                    let ptr = crate::runtime::box_value(v) as i64;
                    return Ok((self.b.ins().iconst(types::I64, ptr), ValTy::Ref));
                }
            }
            return Err(unsupported("class reference index (non-static-field)"));
        }
        // Indexing a non-composite (`(5)[0]`) yields null — box the scalar
        // and let the shared `read_index` return null, matching the interp.
        let arr = {
            let (v, base_ty) = self.local_value(base)?;
            self.coerce(v, base_ty, ValTy::Ref)?
        };
        let (i, it) = self.operand(idx)?;
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        // A statically-integer index needs no heap box: `leek_index_int` takes
        // it as a raw `i64`. The result is still a handle, identical to the
        // boxed-index `leek_value_index` (whose class-ref / instance-method
        // special cases only apply to a string key, so an integer index falls
        // through to the same `read_index_versioned`).
        if it == ValTy::Int {
            let get = self.imports.rt("leek_index_int")?;
            let inst = self.b.ins().call(get, &[arr, i, ver]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        let get = self.imports.rt("leek_value_index")?;
        // The index is itself a boxed value (so map string keys work too).
        let idx_h = self.coerce(i, it, ValTy::Ref)?;
        let inst = self.b.ins().call(get, &[arr, idx_h, ver]);
        Ok((self.b.inst_results(inst)[0], ValTy::Ref))
    }

    /// Box each operand to a `Ref` handle into a fresh stack array, returning
    /// `(ptr, count)`. Used for lambda captures and indirect-call args; the
    /// callee shim copies the handles out, so the array's frame lifetime is
    /// sufficient.
    pub(super) fn build_ref_array(
        &mut self,
        ops: &[Operand],
    ) -> Result<(Value, usize), NativeError> {
        self.build_ref_array_opt(ops, false)
    }

    /// Build a stack array of boxed `Ref` handles from `ops`. When `raw`,
    /// a cell local contributes its *cell handle* directly (not the peeled
    /// value) — used for `MakeLambda` captures so the closure shares the
    /// same `Value::Cell` `Rc` and observes the enclosing scope's writes.
    pub(super) fn build_ref_array_opt(
        &mut self,
        ops: &[Operand],
        raw: bool,
    ) -> Result<(Value, usize), NativeError> {
        let n = ops.len();
        if n == 0 {
            return Ok((self.b.ins().iconst(types::I64, 0), 0));
        }
        let slot = self.b.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            (n * 8) as u32,
            3,
        ));
        for (i, op) in ops.iter().enumerate() {
            let h = match op {
                Operand::Local(id) if raw && self.cell_locals.contains(id) => {
                    self.b.use_var(self.vars[id.0 as usize])
                }
                _ => {
                    let (v, t) = self.operand(op)?;
                    self.coerce(v, t, ValTy::Ref)?
                }
            };
            self.b.ins().stack_store(h, slot, (i * 8) as i32);
        }
        let ptr = self.b.ins().stack_addr(types::I64, slot, 0);
        Ok((ptr, n))
    }
}
