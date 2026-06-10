//! Method dispatch: `new` instances, static/super/user/object-method resolution, and the visibility/override checks they share.

use super::{
    Const, DefId, DefaultArg, HashSet, InstBuilder, LocalId, NativeError, Operand, Tx, ValTy,
    Value, Visibility, builtin_ancestor, coerce_target_ty, const_default, fillable_default,
    receiver_class, resolve_static_field, resolve_static_method, types, unsupported,
};

impl Tx<'_, '_> {
    /// `new C(args)`: allocate the instance, run each field's initializer
    /// (coercing to the declared field type and storing it), then run the
    /// selected constructor. The instance handle is the result.
    pub(super) fn new_instance(
        &mut self,
        class: &str,
        args: &[Operand],
    ) -> Result<(Value, ValTy), NativeError> {
        if self.lang.version < 2 {
            return Err(unsupported("new (v1-3 value semantics)"));
        }
        // `new Array`/`new Map`/`new Set`/`new Object`/`new Integer(x)` etc.
        // — a builtin class, constructed via the shared
        // `construct_builtin_class`.
        if leek_runtime::builtin_class_name(class).is_some() {
            let name = self.const_string(class)?;
            let (ptr, n) = self.build_ref_array(args)?;
            let f = self.imports.rt("leek_construct_builtin")?;
            let nc = self.b.ins().iconst(types::I64, n as i64);
            let inst = self.b.ins().call(f, &[name, ptr, nc]);
            return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
        }
        // `self.program` is a shared `&MirProgram` (independent of `&mut
        // self`), so the resolved class data outlives the builder calls.
        let prog = self.program;
        let Some(c) = prog.class_by_name(class) else {
            return Err(unsupported("new: unknown class"));
        };
        // `class A extends Array {}` (or Map/Set/Object) — upstream (and the
        // interpreter's `construct_user_class`) collapses the user class to the
        // underlying builtin constructor, so `new A()` is a plain `[]`,
        // `push(new A(), 12)` works, and `instanceof A` is false in BOTH
        // backends. Mirror that exactly. A non-collection builtin ancestor still
        // skips (native doesn't model those).
        if let Some(builtin) = builtin_ancestor(prog, c) {
            if matches!(builtin.as_str(), "Array" | "Map" | "Set" | "Object") {
                let name = self.const_string(&builtin)?;
                let (ptr, n) = self.build_ref_array(args)?;
                let f = self.imports.rt("leek_construct_builtin")?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let inst = self.b.ins().call(f, &[name, ptr, nc]);
                return Ok((self.b.inst_results(inst)[0], ValTy::Ref));
            }
            return Err(unsupported("new: class extends a non-collection builtin"));
        }
        // A user `string()` method is a `Display`/`toString` override applied to
        // the *top-level* program result (mirroring the interpreter's
        // `invoke_instance_string_method`). The instance constructs normally;
        // `define_program` force-compiles + registers `string()` for every
        // constructed class so the post-run transform can invoke it. Nested
        // instances render as `Class {…}` in both backends, so construction is
        // always safe.
        let new = self.imports.rt("leek_instance_new")?;
        let set = self.imports.rt("leek_value_set_index")?;
        let class_def = self.b.ins().iconst(types::I64, i64::from(c.def_id.0));
        let name_box = self.const_string(&c.name)?;
        let inst = self.b.ins().call(new, &[class_def, name_box]);
        let this = self.b.inst_results(inst)[0];
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));

        // Initialize every declared field (in flattened slot order, matching
        // the interpreter's parent-first order), so even initializer-less
        // fields exist (as null) — `Display` and `keys()` see them all.
        for fs in &c.field_layout {
            let key = self.const_string(&fs.name)?;
            let boxed = match fs.init_fn {
                None => {
                    let null = self.imports.rt("leek_box_null")?;
                    let inst = self.b.ins().call(null, &[]);
                    self.b.inst_results(inst)[0]
                }
                Some(fi) => {
                    let Some((fref, sig)) = self.imports.field_init_fns.get(&fi) else {
                        return Err(unsupported("field-init not compiled"));
                    };
                    let (fref, sig) = (*fref, sig.clone());
                    if sig.params.len() != 1 {
                        return Err(unsupported("field-init arity"));
                    }
                    let call_inst = self.b.ins().call(fref, &[this]);
                    let mut v = self.b.inst_results(call_inst)[0];
                    let mut vt = sig.ret;
                    // Coerce a scalar init to the declared field type
                    // (`real x = 12` / `real? x = 12` → 12.0).
                    if let Some(ft) = coerce_target_ty(&fs.ty)
                        && vt != ValTy::Ref
                    {
                        v = self.coerce(v, vt, ft)?;
                        vt = ft;
                    }
                    self.coerce(v, vt, ValTy::Ref)?
                }
            };
            self.b.ins().call(set, &[this, key, boxed, ver]);
        }

        // Constructor (if the class or an ancestor declares one).
        if let Some(ctor_idx) = prog.select_constructor(c, args.len())
            && let Some(def) = prog.functions[ctor_idx].def_id
        {
            let Some((fref, sig)) = self.imports.user_fns.get(&def) else {
                return Err(unsupported("constructor not compiled"));
            };
            let (fref, sig) = (*fref, sig.clone());
            // params = `this` (Ref) + user params. Too many args (variadic)
            // isn't modeled.
            let user_params = sig.params.len() - 1;
            if args.len() > user_params {
                return Err(unsupported("constructor variadic args"));
            }
            let ctor_fn = &prog.functions[ctor_idx];
            if sig.has_defaults {
                // The constructor fills omitted defaulted params itself via the
                // hidden `argc` (= `this` + provided args).
                for i in args.len()..user_params {
                    if fillable_default(ctor_fn, ctor_fn.params[i + 1]).is_none() {
                        return Err(unsupported("constructor: omitted param without default"));
                    }
                }
                let mut cl_args = Vec::with_capacity(sig.params.len() + 1);
                cl_args.push(self.coerce(this, ValTy::Ref, sig.params[0])?);
                for (i, &pty) in sig.params[1..].iter().enumerate() {
                    if i < args.len() {
                        let (v, t) = self.operand(&args[i])?;
                        cl_args.push(self.coerce(v, t, pty)?);
                    } else {
                        cl_args.push(self.placeholder(pty));
                    }
                }
                cl_args.push(self.b.ins().iconst(types::I64, (args.len() + 1) as i64));
                self.b.ins().call(fref, &cl_args);
            } else {
                // An omitted trailing user param is filled from its
                // self-contained constant default (`constructor(x = 2)`); a
                // param with no default binds to null — matching the
                // interpreter — provided the slot is a boxed `Ref` (a declared
                // scalar param can't hold null, so that still skips).
                let mut cl_args = Vec::with_capacity(sig.params.len());
                cl_args.push(self.coerce(this, ValTy::Ref, sig.params[0])?);
                for (i, &pty) in sig.params[1..].iter().enumerate() {
                    let (v, t) = if i < args.len() {
                        self.operand(&args[i])?
                    } else if let Some(c) = const_default(ctor_fn, ctor_fn.params[i + 1]) {
                        self.operand(&Operand::Const(c))?
                    } else if pty == ValTy::Ref {
                        self.operand(&Operand::Const(Const::Null))?
                    } else {
                        return Err(unsupported(
                            "constructor: omitted scalar param without default",
                        ));
                    };
                    cl_args.push(self.coerce(v, t, pty)?);
                }
                self.b.ins().call(fref, &cl_args);
            }
        }

        Ok((this, ValTy::Ref))
    }

    /// `true` if the class-reference local `id` names a class that has a
    /// constructor thunk — so its `Value::ClassRef` can flow as a callable
    /// value (constructing via `dispatch_call_value`).
    pub(super) fn classref_has_thunk(&self, id: LocalId) -> bool {
        self.classref_locals
            .get(&id)
            .and_then(|cls| self.program.class_by_name(cls))
            .is_some_and(|c| self.ctor_thunk_classes.contains(&c.def_id.0))
    }

    /// `true` if a write to field `name` on `base` must silently no-op:
    /// the field is `final` AND the write comes from outside the receiver's
    /// class. Inside the class (its constructor/methods) `final` fields are
    /// writable — matching the interpreter's `caller_class_def() != class`.
    pub(super) fn is_final_field(&self, base: LocalId, name: &str) -> bool {
        let Some(cls) = receiver_class(
            self.mir_locals,
            self.new_classes,
            self.aliased_classes,
            base,
        ) else {
            return false;
        };
        let Some(c) = self.program.class_by_name(cls) else {
            return false;
        };
        c.field_slot(name)
            .is_some_and(|fs| fs.is_final && self.owning_class != Some(c.def_id))
    }

    /// The scalar kind a write to field `name` on `base` (a known class
    /// instance) coerces to (`real? x = 5` stores `5.0`), if any.
    pub(super) fn field_coerce_ty(&self, base: LocalId, name: &str) -> Option<ValTy> {
        receiver_class(
            self.mir_locals,
            self.new_classes,
            self.aliased_classes,
            base,
        )
        .and_then(|cls| self.program.class_by_name(cls))
        .and_then(|c| c.field_slot(name))
        .and_then(|fs| coerce_target_ty(&fs.ty))
    }

    /// The dense field slot for `name` when `base`'s class is statically known
    /// and declares (or inherits) `name` as a stored field. The slot
    /// (`MirClass::field_layout`) is assigned root-first, so an inherited field
    /// keeps the same slot in every subclass — making it valid for any runtime
    /// instance whose static type is this class (or a descendant). Returns
    /// `None` for an unknown class or a name that isn't a stored field (e.g. a
    /// method, which must keep the name path's bound-method fallback).
    pub(super) fn field_slot_of(&self, base: LocalId, name: &str) -> Option<usize> {
        // A/B hatch (read at codegen time, zero runtime cost): force the
        // field-name path to measure the slot-indexing win.
        if std::env::var_os("LEEK_NATIVE_NO_FIELDSLOT").is_some() {
            return None;
        }
        receiver_class(
            self.mir_locals,
            self.new_classes,
            self.aliased_classes,
            base,
        )
        .and_then(|cls| self.program.class_by_name(cls))
        .and_then(|c| c.field_slot(name))
        .map(|fs| fs.slot)
    }

    /// `true` if some strict descendant of `base` overrides the instance
    /// method `(name, arity)` — so a receiver of static type `base` whose
    /// runtime class might be that subclass needs virtual dispatch, which
    /// the static dispatch here can't provide.
    pub(super) fn overridden_below(
        &self,
        base: &leek_mir::ir::MirClass,
        name: &str,
        arity: usize,
    ) -> bool {
        self.program.classes.iter().any(|c| {
            c.def_id != base.def_id
                && self.class_descends_from(Some(c.def_id), base.def_id)
                && c.methods
                    .iter()
                    .any(|m| !m.is_static && m.name == name && m.user_arity == arity)
        })
    }

    /// Whether a member owned by `owner` with visibility `vis` is reachable
    /// from the function being compiled. Mirrors the interpreter's
    /// `member_visible`: public always; private only from the same class;
    /// protected from the class or a descendant.
    pub(super) fn method_visible(&self, owner: DefId, vis: Visibility) -> bool {
        match vis {
            Visibility::Public => true,
            Visibility::Private => self.owning_class == Some(owner),
            Visibility::Protected => self.class_descends_from(self.owning_class, owner),
        }
    }

    /// True if `caller` is `owner` or a subclass of it (cycle-safe), via the
    /// resolved `parent_def` chain.
    pub(super) fn class_descends_from(&self, caller: Option<DefId>, owner: DefId) -> bool {
        let Some(start) = caller else { return false };
        let mut cur = Some(start);
        let mut seen: HashSet<DefId> = HashSet::new();
        while let Some(d) = cur {
            if !seen.insert(d) {
                return false;
            }
            if d == owner {
                return true;
            }
            cur = self.program.class(d).and_then(|c| c.parent_def);
        }
        false
    }

    /// Try to statically dispatch `receiver.method(args)` to a user-class
    /// method. Returns `Ok(None)` when the receiver isn't a known
    /// `ClassInstance` or the class has no such method (so the caller can
    /// fall back to builtin-method sugar).
    /// `C.staticMethod(args)` — the receiver is a class reference. Resolve
    /// the static method and call it as a free function (no `this`). Mirrors
    /// the interpreter's `dispatch_method_call` on a `Value::ClassRef`.
    pub(super) fn try_static_method(
        &mut self,
        receiver: LocalId,
        method: &str,
        args: &[Operand],
    ) -> Result<Option<(Value, ValTy)>, NativeError> {
        let Some(cls) = self.classref_locals.get(&receiver).cloned() else {
            return Ok(None);
        };
        let Some(idx) = resolve_static_method(self.program, &cls, method, args.len()) else {
            // Not a static method — but it may be a static *field* holding a
            // callable (`static a = -> 12` then `A.a()`): read the field and
            // invoke the value indirectly (matching the interpreter, which
            // reads the static member then `call_value`s it). The field's
            // initializer (+ lambda) is made reachable by
            // `static_field_accesses` recognizing this call shape.
            if let Some((owner, fld)) = resolve_static_field(self.program, &cls, method) {
                if !self.method_visible(owner, fld.visibility) {
                    return Err(unsupported("static field (callable) not visible"));
                }
                let coerce = coerce_target_ty(&fld.ty);
                let callee = self.static_field_get(owner, method, coerce)?;
                let (ptr, n) = self.build_ref_array(args)?;
                let f = self.imports.rt("leek_call_value")?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let ver = self
                    .b
                    .ins()
                    .iconst(types::I64, i64::from(self.lang.version));
                let inst = self.b.ins().call(f, &[callee, ptr, nc, ver]);
                return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
            }
            // Otherwise an instance method used as a free function, etc. —
            // skip rather than miscompile.
            return Err(unsupported("static method not found on class reference"));
        };
        // An inaccessible static method yields null (matching the interp).
        if let Some(owner) = self.program.functions[idx].owning_class
            && let Some(c) = self.program.class(owner)
            && let Some(m) = c.methods.iter().find(|m| m.function_idx == idx)
            && !self.method_visible(owner, m.visibility)
        {
            let null = self.imports.rt("leek_box_null")?;
            let inst = self.b.ins().call(null, &[]);
            return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
        }
        let Some(def) = self.program.functions[idx].def_id else {
            return Ok(None);
        };
        if !self.imports.user_fns.contains_key(&def) {
            return Err(unsupported("static method target not compiled"));
        }
        Ok(Some(self.user_call(def, args)?))
    }

    /// `super.m(args)` — the receiver is a `MakeSuper` local. Resolve `m`
    /// against the *parent* class and dispatch statically (super is never
    /// virtual), passing the real `this` instance as the receiver.
    pub(super) fn try_super_method(
        &mut self,
        receiver: LocalId,
        method: &str,
        args: &[Operand],
    ) -> Result<Option<(Value, ValTy)>, NativeError> {
        if self.lang.version < 2 {
            return Ok(None);
        }
        let Some((this_local, parent)) = self.super_locals.get(&receiver).cloned() else {
            return Ok(None);
        };
        let prog = self.program;
        let Some(c) = prog.class_by_name(&parent) else {
            return Ok(None);
        };
        let Some(vt) = prog.resolve_method(c, method, Some(args.len())) else {
            return Err(unsupported("super method not found"));
        };
        // An inaccessible parent method through `super` is ambiguous between
        // the interp's null-dispatch and an actual call — skip rather than
        // risk a wrong result.
        if !self.method_visible(vt.owner, vt.visibility) {
            return Err(unsupported("super method not visible"));
        }
        let Some(def) = prog.functions[vt.function_idx].def_id else {
            return Ok(None);
        };
        let Some((fref, sig)) = self.imports.user_fns.get(&def) else {
            return Err(unsupported("super method target not compiled"));
        };
        let (fref, sig) = (*fref, sig.clone());
        if args.len() + 1 != sig.params.len() {
            return Ok(None);
        }
        let (recv, recv_ty) = self.local_value(this_local)?;
        let mut cl_args = Vec::with_capacity(args.len() + 1);
        cl_args.push(self.coerce(recv, recv_ty, sig.params[0])?);
        for (op, &pty) in args.iter().zip(&sig.params[1..]) {
            let (v, t) = self.operand(op)?;
            cl_args.push(self.coerce(v, t, pty)?);
        }
        let inst = self.b.ins().call(fref, &cl_args);
        Ok(Some((self.b.inst_results(inst)[0], sig.ret)))
    }

    pub(super) fn try_user_method(
        &mut self,
        receiver: LocalId,
        method: &str,
        args: &[Operand],
    ) -> Result<Option<(Value, ValTy)>, NativeError> {
        if self.lang.version < 2 {
            return Ok(None);
        }
        let prog = self.program;
        let Some(name) = receiver_class(
            self.mir_locals,
            self.new_classes,
            self.aliased_classes,
            receiver,
        ) else {
            // Unknown receiver class. If `method` is a (non-builtin) user method
            // of some class, dispatch dynamically on the receiver's RUNTIME class
            // via `leek_call_method` (instance method else builtin) — mirroring
            // the interpreter, and matching `dynamic_method_targets`' seeding.
            if !leek_runtime::is_known_builtin(method)
                && prog
                    .classes
                    .iter()
                    .any(|c| prog.resolve_method(c, method, Some(args.len())).is_some())
            {
                let (recv, recv_ty) = self.local_value(receiver)?;
                let recv = self.coerce(recv, recv_ty, ValTy::Ref)?;
                let key = self.const_string(method)?;
                let (ptr, n) = self.build_ref_array(args)?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let ver = self
                    .b
                    .ins()
                    .iconst(types::I64, i64::from(self.lang.version));
                let f = self.imports.rt("leek_call_method")?;
                let inst = self.b.ins().call(f, &[recv, key, ptr, nc, ver]);
                return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
            }
            return Ok(None);
        };
        let Some(c) = prog.class_by_name(name) else {
            return Ok(None);
        };
        let Some(vt) = prog.resolve_method(c, method, Some(args.len())) else {
            // No method by that name. If the class has a data *field* with
            // that name, `obj.field(args)` reads the field and invokes its
            // value (a stored function/lambda/bound method) — matching the
            // interpreter's `dispatch_method_call`, which falls back to a field
            // read + `call_value`.
            if c.field_slot(method).is_some() {
                let (callee, ct) = self.field(receiver, method)?;
                let callee = self.coerce(callee, ct, ValTy::Ref)?;
                let (ptr, n) = self.build_ref_array(args)?;
                let f = self.imports.rt("leek_call_value")?;
                let nc = self.b.ins().iconst(types::I64, n as i64);
                let ver = self
                    .b
                    .ins()
                    .iconst(types::I64, i64::from(self.lang.version));
                let inst = self.b.ins().call(f, &[callee, ptr, nc, ver]);
                return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
            }
            // No method and no field — the interpreter falls back to
            // `run_method` = `run_builtin(method, [receiver, …args])` (a builtin
            // method on the value; an unknown name or a math builtin on a
            // non-number yields null). The generic `call_builtin` shim
            // reproduces this EXACTLY (same `call_builtin`, null on Err), so
            // route through it rather than the type-coercing math path.
            let mut combined = Vec::with_capacity(args.len() + 1);
            combined.push(Operand::Local(receiver));
            combined.extend(args.iter().cloned());
            return Ok(Some(self.generic_builtin(method, &combined)?));
        };
        // Static dispatch is only sound when the receiver's runtime class is
        // its static class. A `new C()` temp is exact; a `this`/typed local
        // could be a subclass. If a subclass overrides this method, dispatch
        // VIRTUALLY: build a bound method keyed on the receiver's RUNTIME class
        // (via `leek_value_index` → `METHOD_RESOLVE`, seeded for the whole
        // hierarchy by `virtual_method_targets`) and invoke it. Gated to public
        // methods — an override's visibility depends on the runtime class,
        // which the static check can't model.
        let exact = self.new_classes.contains_key(&receiver);
        if !exact && self.overridden_below(c, method, args.len()) {
            if !matches!(vt.visibility, Visibility::Public) {
                return Err(unsupported("virtual dispatch (non-public method)"));
            }
            let (recv, recv_ty) = self.local_value(receiver)?;
            let recv = self.coerce(recv, recv_ty, ValTy::Ref)?;
            let key = self.const_string(method)?;
            let ver = self
                .b
                .ins()
                .iconst(types::I64, i64::from(self.lang.version));
            let idx = self.imports.rt("leek_value_index")?;
            let inst = self.b.ins().call(idx, &[recv, key, ver]);
            let bound = self.b.inst_results(inst)[0];
            let (ptr, n) = self.build_ref_array(args)?;
            let f = self.imports.rt("leek_call_value")?;
            let nc = self.b.ins().iconst(types::I64, n as i64);
            let inst = self.b.ins().call(f, &[bound, ptr, nc, ver]);
            return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
        }
        // An inaccessible method call yields null (matching the interpreter's
        // `dispatch_method_call`), rather than invoking the method.
        if !self.method_visible(vt.owner, vt.visibility) {
            let null = self.imports.rt("leek_box_null")?;
            let inst = self.b.ins().call(null, &[]);
            return Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)));
        }
        let Some(def) = prog.functions[vt.function_idx].def_id else {
            return Ok(None);
        };
        let Some((fref, sig)) = self.imports.user_fns.get(&def) else {
            return Err(unsupported("method target not compiled"));
        };
        let (fref, sig) = (*fref, sig.clone());
        // params = `this` (Ref) + user params. Too many args (variadic) →
        // fall back. Too few → pad omitted trailing params that carry a
        // self-contained default (`m(x = 2)`); otherwise fall back.
        let n_user_params = sig.params.len() - 1;
        if args.len() > n_user_params {
            return Ok(None);
        }
        // `has_defaults`: the method fills omitted defaulted params itself via
        // the hidden `argc` (= `this` + provided user args). Each omitted user
        // param must carry a default; pass placeholders + the count.
        if sig.has_defaults {
            let Some(callee) = prog.function(def) else {
                return Ok(None);
            };
            for i in args.len()..n_user_params {
                if fillable_default(callee, callee.params[i + 1]).is_none() {
                    return Ok(None);
                }
            }
            let (recv, recv_ty) = self.local_value(receiver)?;
            let mut cl_args = Vec::with_capacity(sig.params.len() + 1);
            cl_args.push(self.coerce(recv, recv_ty, sig.params[0])?);
            for idx in 0..n_user_params {
                let pty = sig.params[idx + 1];
                if idx < args.len() {
                    let (v, t) = self.operand(&args[idx])?;
                    cl_args.push(self.coerce(v, t, pty)?);
                } else {
                    cl_args.push(self.placeholder(pty));
                }
            }
            // argc counts `this` + the provided user args.
            cl_args.push(self.b.ins().iconst(types::I64, (args.len() + 1) as i64));
            let inst = self.b.ins().call(fref, &cl_args);
            return Ok(Some((self.b.inst_results(inst)[0], sig.ret)));
        }
        let mut defaults: Vec<DefaultArg> = Vec::new();
        if args.len() < n_user_params {
            let Some(callee) = prog.function(def) else {
                return Ok(None);
            };
            for i in args.len()..n_user_params {
                match self.param_default(callee, callee.params[i + 1]) {
                    Some(d) => defaults.push(d),
                    None => return Ok(None),
                }
            }
        }
        let (recv, recv_ty) = self.local_value(receiver)?;
        let mut cl_args = Vec::with_capacity(sig.params.len());
        cl_args.push(self.coerce(recv, recv_ty, sig.params[0])?);
        for idx in 0..n_user_params {
            let pty = sig.params[idx + 1];
            let (v, t) = if idx < args.len() {
                self.operand(&args[idx])?
            } else {
                self.default_arg_value(&defaults[idx - args.len()])?
            };
            cl_args.push(self.coerce(v, t, pty)?);
        }
        let inst = self.b.ins().call(fref, &cl_args);
        Ok(Some((self.b.inst_results(inst)[0], sig.ret)))
    }

    /// `o.field(args)` where `o` is an object literal — read the field and
    /// invoke its value (`leek_call_value`: a callable runs, a non-callable /
    /// absent field yields null, matching the interpreter's `Object` arm of
    /// `dispatch_method_call`). Returns `Ok(None)` when `o` isn't a known
    /// object local (so the caller falls back to builtin-method sugar).
    pub(super) fn try_object_method(
        &mut self,
        receiver: LocalId,
        field: &str,
        args: &[Operand],
    ) -> Result<Option<(Value, ValTy)>, NativeError> {
        if !self.object_locals.contains(&receiver) {
            return Ok(None);
        }
        // Only a field the object literal actually DEFINES is an
        // object-field-call. A *missing* field name is a builtin method on the
        // object (`{}.keys()`, `{a:1}.values()`) — fall through to builtin
        // sugar (the interpreter's `Object` arm does the same: field-then-
        // `run_method`).
        let Some(field_op) = self
            .object_field_srcs
            .get(&receiver)
            .and_then(|m| m.get(field))
        else {
            return Ok(None);
        };
        // A field holding a *user* class reference CONSTRUCTS the class when
        // invoked. That works when the class has a constructor thunk (the field
        // holds a `Value::ClassRef`, and `dispatch_call_value` constructs via
        // the thunk). Without one (un-constructible class / v1) the runtime
        // couldn't build it, so skip. (Builtin classes always construct via the
        // `BuiltinClass` arm, so those are fine.)
        if let Operand::Local(l) = field_op
            && self.classref_locals.contains_key(l)
            && !self.classref_has_thunk(*l)
        {
            return Err(unsupported("object field holds a class reference"));
        }
        let (callee, ct) = self.field(receiver, field)?;
        let callee = self.coerce(callee, ct, ValTy::Ref)?;
        let (ptr, n) = self.build_ref_array(args)?;
        let f = self.imports.rt("leek_call_value")?;
        let nc = self.b.ins().iconst(types::I64, n as i64);
        let ver = self
            .b
            .ins()
            .iconst(types::I64, i64::from(self.lang.version));
        let inst = self.b.ins().call(f, &[callee, ptr, nc, ver]);
        Ok(Some((self.b.inst_results(inst)[0], ValTy::Ref)))
    }
}
