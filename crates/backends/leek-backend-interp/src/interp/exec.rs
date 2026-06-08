//! MIR interpreter — exec.

use std::cell::RefCell;
use std::rc::Rc;

use leek_mir::{
    BinOp,
    IntervalRvalue, Operand, Place, Rvalue, SliceBounds, Statement,
    Terminator,
};
use leek_types::Type;

use crate::value::{Function as FnValue, IntervalValue, MapData, ObjectData, SetData, SuperValue, Value};

use super::value::{const_to_value, coerce_to_type, set_field, set_index, binary_op_cost, apply_binary, apply_unary, apply_cast, slice_value, builtin_class_name, construct_builtin_class, make_foreach_iter};
use super::{Interpreter, Outcome, StepResult};

impl Interpreter<'_> {
    pub(crate) fn exec_stmt(
        &mut self,
        locals: &mut Vec<Value>,
        stmt: &Statement,
    ) -> Result<(), Outcome> {
        match stmt {
            Statement::Assign(place, rv) => {
                let value = self.eval_rvalue(locals, rv)?;
                // A freshly-produced value (builtin result) must not be
                // v1-cloned on assignment — see [`Rvalue::UseFresh`].
                let skip_v1_clone = matches!(rv, Rvalue::UseFresh(_));
                self.write_place(locals, place, value, skip_v1_clone)?;
                Ok(())
            }
            Statement::Call { dest, call } => {
                let v = self.exec_call(locals, call)?;
                if let Some(place) = dest {
                    self.write_place(locals, place, v, false)?;
                }
                Ok(())
            }
            Statement::ApplyPromotion(local) => {
                if let Some(v) = leek_runtime::take_pending_promotion() {
                    if let Some(slot) = locals.get_mut(local.0 as usize) {
                        if let Value::Cell(cell) = slot {
                            *cell.borrow_mut() = v;
                        } else {
                            *slot = v;
                        }
                    }
                }
                Ok(())
            }
            Statement::Charge(n) => {
                // Charge `n` ops. (There is no per-statement `tick()`
                // debit — the interpreter charges ops only at explicit
                // sites — so this adds the full `n`.)
                self.op_count = self.op_count.saturating_add(*n);
                if let Some(limit) = self.op_limit {
                    if self.op_count > limit {
                        return Err(Outcome::Error("TOO_MUCH_OPERATIONS".into()));
                    }
                }
                Ok(())
            }
        }
    }

    // ---- Terminators ----

    pub(crate) fn exec_terminator(
        &mut self,
        locals: &[Value],
        term: &Terminator,
    ) -> Result<StepResult, Outcome> {
        match term {
            Terminator::Goto(b) => Ok(StepResult::Goto(*b)),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                // Upstream charges 1 op per branch (matches the
                // `if/while/and/or` flow-control cost).
                if let Some(o) = self.charge_ops(1) {
                    return Err(o);
                }
                let cv = self.read_operand_cow(locals, cond);
                let take_then = cv.is_truthy();
                Ok(StepResult::Goto(if take_then {
                    *then_block
                } else {
                    *else_block
                }))
            }
            Terminator::Return(opt) => {
                // When the returned operand is a Local that's marked
                // `is_by_ref` (i.e. the body did `return @x`),
                // preserve the underlying `Value::Cell` so the
                // caller can choose to alias rather than clone.
                // Plain returns peel the cell as usual.
                let v = match opt {
                    Some(Operand::Local(id)) => {
                        let is_ref = self
                            .function_stack
                            .last()
                            .and_then(|&idx| self.function(idx).locals.get(id.0 as usize))
                            .is_some_and(|d| d.is_by_ref);
                        if is_ref {
                            self.read_operand_raw(locals, &Operand::Local(*id))
                        } else {
                            self.read_operand(locals, &Operand::Local(*id))
                        }
                    }
                    Some(op) => self.read_operand(locals, op),
                    None => Value::Null,
                };
                Ok(StepResult::Return(v))
            }
            Terminator::Switch {
                discriminant,
                arms,
                default,
            } => {
                let disc = self.read_operand_cow(locals, discriminant);
                for (k, target) in arms {
                    let kv = const_to_value(k);
                    if disc.loose_eq(&kv) {
                        return Ok(StepResult::Goto(*target));
                    }
                }
                Ok(StepResult::Goto(*default))
            }
            Terminator::Unreachable => Err(Outcome::Error("unreachable MIR terminator".into())),
        }
    }

    // ---- Operands and places ----

    // `read_operand`/`read_operand_raw` take `&self` for consistency with the
    // other `Interp` exec methods even though they only read `locals`.
    #[allow(clippy::unused_self)]
    pub(crate) fn read_operand(&self, locals: &[Value], op: &Operand) -> Value {
        match op {
            Operand::Local(id) => {
                // Locals marked `is_shared` are stored as
                // `Value::Cell(rc)` so closures see writes. Most
                // call sites want the unwrapped value, so peel the
                // cell here. Capture construction (`MakeLambda`)
                // takes the raw slot via `read_operand_raw` so the
                // Rc identity is preserved end-to-end.
                let raw = locals.get(id.0 as usize).cloned().unwrap_or(Value::Null);
                raw.unbox()
            }
            Operand::Const(c) => const_to_value(c),
        }
    }

    /// Like [`Self::read_operand`] but borrows the value in place when possible,
    /// returning a [`Cow`] that is `Borrowed` for the common case (a plain
    /// local that isn't a captured `Value::Cell`) and `Owned` only when a value
    /// must be materialised — a `Const`, an out-of-range slot, or a `Cell` that
    /// has to be peeled. The hot arithmetic paths read operands only to pass
    /// them by reference into the shared `eval` helpers, so borrowing avoids a
    /// clone *and* its matching drop per operand — the interpreter's single
    /// largest per-op cost (`Value` clone/drop churn, per profiling).
    #[allow(clippy::unused_self)]
    pub(crate) fn read_operand_cow<'l>(
        &self,
        locals: &'l [Value],
        op: &Operand,
    ) -> std::borrow::Cow<'l, Value> {
        use std::borrow::Cow;
        match op {
            Operand::Local(id) => match locals.get(id.0 as usize) {
                // A captured local is stored behind a shared `Cell`; peel it
                // (this clones, as before) so callers see the "real" value.
                Some(v @ Value::Cell(_)) => Cow::Owned(v.unbox()),
                Some(v) => Cow::Borrowed(v),
                None => Cow::Owned(Value::Null),
            },
            Operand::Const(c) => Cow::Owned(const_to_value(c)),
        }
    }

    /// Like [`Self::read_operand`] but does NOT peel a captured
    /// local's `Value::Cell`. Used by `MakeLambda` so the closure
    /// receives the same `Rc` the outer scope stores in its slot,
    /// keeping writes synchronised in both directions.
    #[allow(clippy::unused_self)]
    pub(crate) fn read_operand_raw(&self, locals: &[Value], op: &Operand) -> Value {
        match op {
            Operand::Local(id) => locals.get(id.0 as usize).cloned().unwrap_or(Value::Null),
            Operand::Const(c) => const_to_value(c),
        }
    }

    // `locals` is the function's frame stack, threaded as `&mut Vec` uniformly
    // across the exec methods (some of which grow it), so it isn't a slice here.
    #[allow(clippy::ptr_arg)]
    pub(crate) fn write_place(
        &mut self,
        locals: &mut Vec<Value>,
        p: &Place,
        value: Value,
        skip_v1_clone: bool,
    ) -> Result<(), Outcome> {
        match p {
            Place::Local(id) => {
                let i = id.0 as usize;
                if i < locals.len() {
                    // Explicit `T x = init` (declared type) coerces
                    // on every write. Inferred types from
                    // `var x = ...` coerce too in strict mode;
                    // in non-strict they only affect compound
                    // assigns (MIR emits an explicit Cast there).
                    let decl = self
                        .function_stack
                        .last()
                        .and_then(|&idx| self.function(idx).locals.get(i));
                    let target_ty = decl.and_then(|d| match (&d.ty, &d.inferred_ty) {
                        (Type::Any, Some(it)) if self.strict => Some(it.clone()),
                        (t, _) if !matches!(t, Type::Any) => Some(t.clone()),
                        _ => None,
                    });
                    let coerced = match target_ty {
                        Some(ty) => coerce_to_type(&value, &ty),
                        None => value,
                    };
                    // v1 LegacyArray pass-by-value: assignments to
                    // user-visible locals snapshot composite values
                    // so `var b = a; push(b, ...)` doesn't mutate
                    // `a`. Temps stay shared (they hold expression
                    // intermediates and would balloon clones).
                    // `@a` (marked `is_by_ref`) opts out of the
                    // clone — the local then shares the same `Rc`
                    // as the source for composites.
                    let is_user_local = decl
                        .is_some_and(|d| matches!(d.kind, leek_mir::LocalKind::UserLocal));
                    let skip_clone = skip_v1_clone
                        || decl.is_some_and(|d| d.is_by_ref)
                        // Incoming `Value::Cell` signals a reference
                        // alias (e.g. `var a = f()` where `f` does
                        // `return @x`). The caller wants to share
                        // storage with the source, so the v1 deep
                        // clone would be wrong here.
                        || matches!(coerced, Value::Cell(_));
                    let snapped = if self.version <= 1 && is_user_local && !skip_clone {
                        leek_runtime::deep_clone_for_v1(&coerced)
                    } else {
                        coerced
                    };
                    // Captured locals live behind a shared
                    // `Value::Cell` so the closure (which holds a
                    // clone of the same `Rc`) sees this write — but
                    // only for locals declared `is_shared`. Plain
                    // locals always replace the slot. A plain local
                    // can momentarily hold a `Value::Cell` (the
                    // `Use` rvalue preserves Cells from byref
                    // sources), and writing snapped into that Cell's
                    // interior would create an `rc → rc` cycle when
                    // the snapped value is the same Cell, causing
                    // `unbox` to spin forever.
                    let is_shared = decl.is_some_and(|d| d.is_shared);
                    if is_shared && let Value::Cell(cell) = &locals[i] {
                        // Peel snapped before writing into the shared
                        // storage so we never store `Cell` inside a
                        // `Cell` (and so cell-aliasing-itself is
                        // impossible).
                        *cell.borrow_mut() = snapped.unbox();
                    } else {
                        locals[i] = snapped;
                    }
                }
            }
            Place::Global(_, name) => {
                // Coerce the RHS to the global's declared type
                // (e.g. `global real x = 56` stores `56.0` not `56`).
                // The type comes from the precomputed `global_types` map (O(1),
                // only non-`Any` entries present), so an untyped global skips
                // coercion entirely (`coerce_to_type` on `Any` is identity).
                // Skip the lookup hash altogether when no global is typed (the
                // common case) — `global_types` is then empty.
                let coerced = if self.global_types.is_empty() {
                    value
                } else {
                    match self.global_types.get(name) {
                        Some(ty) => coerce_to_type(&value, ty),
                        None => value,
                    }
                };
                // Update an existing global in place — avoids cloning the name
                // `String` (a heap allocation) on every write in a loop; only
                // the first-ever write of a global allocates the key.
                if let Some(slot) = self.globals.get_mut(name) {
                    *slot = coerced;
                } else {
                    self.globals.insert(name.clone(), coerced);
                }
            }
            Place::Field(base, name) => {
                let base_v = locals
                    .get(base.0 as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .unbox();
                // `ClassName.field = ...` writes to the static
                // slot. Cache it the same place
                // `read_static_field` looks (keyed by class DefId
                // + name) so subsequent reads see the new value.
                if let Value::ClassRef(def_id, _) = &base_v {
                    if self.is_static_field_final(*def_id, name) {
                        return Ok(());
                    }
                    // Coerce to the declared static-field type if
                    // one exists, so `static real x; X.x = 10`
                    // stores `10.0`.
                    let ty = self.static_field_type(*def_id, name);
                    let stored = match ty {
                        Some(t) => coerce_to_type(&value, &t),
                        None => value,
                    };
                    // Cache under the owning class so subclasses
                    // see the same value (mirrors `read_static_field`).
                    let owner = self.static_field_owner(*def_id, name).unwrap_or(*def_id);
                    self.static_fields.insert((owner, name.clone()), stored);
                    return Ok(());
                }
                if let Value::Instance(inst) = &base_v {
                    let class_def = inst.borrow().class;
                    // `final` fields are writable from inside the
                    // owning class's own methods/constructors —
                    // constructors typically initialise them. Block
                    // writes from outside that scope.
                    if self.is_instance_field_final(class_def, name)
                        && self.caller_class_def() != Some(class_def)
                    {
                        return Ok(());
                    }
                    if let Some(ty) = self.instance_field_type(class_def, name) {
                        let coerced = coerce_to_type(&value, &ty);
                        set_field(&base_v, name, coerced);
                        return Ok(());
                    }
                }
                set_field(&base_v, name, value);
            }
            Place::Index(base, idx) => {
                let base_v = locals
                    .get(base.0 as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .unbox();
                let idx_v = self.read_operand(locals, idx);
                // `ClassName['field'] = ...` writes to the static
                // slot, same as the dotted form.
                if let (Value::ClassRef(def_id, _), Value::String(s)) = (&base_v, &idx_v) {
                    if self.is_static_field_final(*def_id, s.as_str()) {
                        return Ok(());
                    }
                    let ty = self.static_field_type(*def_id, s.as_str());
                    let stored = match ty {
                        Some(t) => coerce_to_type(&value, &t),
                        None => value,
                    };
                    let owner = self
                        .static_field_owner(*def_id, s.as_str())
                        .unwrap_or(*def_id);
                    self.static_fields.insert((owner, (**s).clone()), stored);
                    return Ok(());
                }
                if let (Value::Instance(inst), Value::String(s)) = (&base_v, &idx_v) {
                    let class_def = inst.borrow().class;
                    if self.is_instance_field_final(class_def, s.as_str())
                        && self.caller_class_def() != Some(class_def)
                    {
                        return Ok(());
                    }
                }
                // Coerce the RHS to the array's declared element
                // type when one is known — `Array<real> a; a[0] =
                // round(1.5)` should store `2.0`, not `2`, so the
                // typed assertion holds when the slot is later read.
                let elt_ty = self.current_frame_local_type(*base);
                let coerced = if let Some(ty) = elt_ty {
                    coerce_to_type(&value, &ty)
                } else {
                    value
                };
                // v4 strict: out-of-bound array write is a runtime
                // error. Non-strict v4 silently ignores the write
                // (returning null) — matches upstream where
                // `code_strict_v4_(...)` errors but `code_v4_(...)`
                // returns null without throwing.
                if self.version >= 4
                    && self.strict
                    && let Value::Array(a) = &base_v
                {
                    let len = i64::try_from(a.borrow().len()).expect("array longer than i64::MAX");
                    let raw = idx_v.as_int().unwrap_or(0);
                    let i = if raw < 0 { raw + len } else { raw };
                    if i < 0 || i >= len {
                        return Err(Outcome::Error("ARRAY_OUT_OF_BOUND".into()));
                    }
                }
                if let Some(new_base) = set_index(&base_v, &idx_v, coerced, self.version) {
                    // v1-v3 LegacyArray promotion — the Array
                    // morphed into a sparse Map. Write the new
                    // value back into whatever held it.
                    if let Some(slot) = locals.get_mut(base.0 as usize) {
                        *slot = new_base;
                    }
                }
            }
            Place::Slice(_, _) => {
                // Slice assignment is rare in Leekscript and not
                // exercised by the current corpus; leave unimplemented
                // for now.
            }
            Place::LambdaCapture { lambda, slot } => {
                // The lambda's local may sit behind a `Value::Cell`
                // when it captures something (e.g. recursive
                // self-binding marks the slot as shared). Peek the
                // cell to reach the underlying `Lambda` value.
                let lv = locals
                    .get(lambda.0 as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
                    .unbox();
                if let Value::Function(FnValue::Lambda(cap)) = lv {
                    let mut captured = cap.captured.borrow_mut();
                    if *slot < captured.len() {
                        // If the capture slot itself holds a
                        // `Value::Cell` (the normal case for any
                        // captured-and-mutated local), write
                        // through the cell so other holders of the
                        // same `Rc` see the change. Unbox the
                        // incoming value to avoid writing a Cell
                        // into a Cell — in the recursive
                        // self-capture patch both sides reference
                        // the same cell, which would otherwise
                        // create a self-loop and hang `Value::unbox`.
                        if let Value::Cell(cell) = &captured[*slot] {
                            *cell.borrow_mut() = value.unbox();
                        } else {
                            captured[*slot] = value;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn eval_slice_bounds(
        &self,
        locals: &[Value],
        bounds: &SliceBounds,
    ) -> (Option<i64>, Option<i64>, Option<f64>) {
        // Upstream treats `null` inside a slice bound (`a[:null:3]`)
        // as "use the default for this side", not "coerce to 0".
        // Reflect that by flattening Value::Null operands to None.
        // Step is read as f64 so interval slices like
        // `[1..3][::-1.5]` keep their fractional stride.
        let map_int = |o: &Option<Operand>| {
            o.as_ref()
                .and_then(|op| match self.read_operand(locals, op) {
                    Value::Null => None,
                    v => Some(v.as_int().unwrap_or(0)),
                })
        };
        let map_real = |o: &Option<Operand>| {
            o.as_ref()
                .and_then(|op| match self.read_operand(locals, op) {
                    Value::Null => None,
                    v => Some(v.as_real().unwrap_or(0.0)),
                })
        };
        (
            map_int(&bounds.start),
            map_int(&bounds.end),
            map_real(&bounds.step),
        )
    }

    // ---- Rvalues ----

    #[allow(clippy::ptr_arg)] // `locals` frame stack threaded as `&mut Vec`; see `write_place`
    pub(crate) fn eval_rvalue(
        &mut self,
        locals: &mut Vec<Value>,
        rv: &Rvalue,
    ) -> Result<Value, Outcome> {
        Ok(match rv {
            Rvalue::Use(op) => {
                // Preserve `Value::Cell` when the source is a
                // temporary holding a returned-by-reference value
                // (`var a = f()` where `f` did `return @x`). Without
                // this the cell is unboxed at the Use step and the
                // alias is lost before the v1 clone check in
                // `write_place` can react to it.
                let raw = self.read_operand_raw(locals, op);
                if matches!(raw, Value::Cell(_)) {
                    raw
                } else {
                    self.read_operand(locals, op)
                }
            }
            // Same value as `Use`; the skip-clone signal is read off the
            // rvalue kind in the `Assign` handler.
            Rvalue::UseFresh(op) => {
                let raw = self.read_operand_raw(locals, op);
                if matches!(raw, Value::Cell(_)) {
                    raw
                } else {
                    self.read_operand(locals, op)
                }
            }
            Rvalue::Binary(op, l, r) => {
                let lv = self.read_operand_cow(locals, l);
                let rv = self.read_operand_cow(locals, r);
                if let Some(o) = self.charge_ops(binary_op_cost(*op)) {
                    return Err(o);
                }
                let result = apply_binary(*op, &lv, &rv, self.version)?;
                // String concatenation costs 1 op per character of the
                // result, on top of the node cost above (upstream's
                // `add` runtime cost). Only when a string operand made
                // this a concat — array `+` is unaffected.
                if *op == BinOp::Add
                    && (matches!(&*lv, Value::String(_)) || matches!(&*rv, Value::String(_)))
                    && let Value::String(s) = &result
                    && let Some(o) = self.charge_ops(s.chars().count() as u64)
                {
                    return Err(o);
                }
                result
            }
            Rvalue::Unary(op, x) => {
                let v = self.read_operand_cow(locals, x);
                apply_unary(*op, &v)
            }
            Rvalue::Cast(kind, x) => {
                let v = self.read_operand_cow(locals, x);
                apply_cast(*kind, &v)
            }
            Rvalue::Field(base, name) => {
                let base_v = locals[base.0 as usize].clone().unbox();
                self.read_field_with_methods(&base_v, name)
            }
            Rvalue::Index(base, idx) => {
                let base_v = locals[base.0 as usize].clone().unbox();
                let idx_v = self.read_operand(locals, idx);
                self.read_index_with_methods(&base_v, &idx_v)
            }
            Rvalue::Slice(base, bounds) => {
                let base_v = locals[base.0 as usize].clone().unbox();
                let (s, e, st) = self.eval_slice_bounds(locals, bounds);
                slice_value(&base_v, s, e, st)
            }
            Rvalue::Array(items) => {
                let vs: Vec<Value> = items.iter().map(|o| self.read_operand(locals, o)).collect();
                Value::Array(Rc::new(RefCell::new(vs)))
            }
            Rvalue::Map(pairs) => {
                let mut m = MapData::new();
                for (k, v) in pairs {
                    let kv = self.read_operand(locals, k);
                    let vv = self.read_operand(locals, v);
                    let canon = crate::value::key_repr(&kv);
                    m.insert_canonical(canon, kv, vv);
                }
                Value::Map(Rc::new(RefCell::new(m)))
            }
            Rvalue::Set(items) => {
                // Upstream charges 2 ops per element on set
                // literals (`<a, b, c>`). Array/Map literals are 0
                // because their `LeekArray.analyze` rolls each
                // element's ops into the binary-add chain instead.
                if let Some(o) = self.charge_ops(2 * items.len() as u64) {
                    return Err(o);
                }
                let mut s = SetData::new();
                for o in items {
                    s.insert(self.read_operand(locals, o));
                }
                Value::Set(Rc::new(RefCell::new(s)))
            }
            Rvalue::Object(pairs) => {
                let mut o = ObjectData::new();
                for (k, v) in pairs {
                    let vv = self.read_operand(locals, v);
                    o.set(k, vv);
                }
                Value::Object(Rc::new(RefCell::new(o)))
            }
            Rvalue::New { class, args } => {
                let args: Vec<Value> = args.iter().map(|o| self.read_operand(locals, o)).collect();
                if let Some(name) = builtin_class_name(class) {
                    construct_builtin_class(name, args)
                } else {
                    self.construct_user_class(class, args)?
                }
            }
            Rvalue::Interval(iv) => {
                // Upstream charges 2 ops for interval-literal
                // construction (`LeekInterval.analyze` → `operations = 2`).
                if let Some(o) = self.charge_ops(2) {
                    return Err(o);
                }
                self.materialize_interval(locals, iv)
            }
            Rvalue::MakeForeachIter(op) => {
                let v = self.read_operand(locals, op);
                make_foreach_iter(&v)
            }
            Rvalue::MakeLambda {
                function_idx,
                captures,
            } => {
                // Capture operands keep the raw slot (so
                // `Value::Cell` references stay shared between
                // outer scope and closure). `read_operand_raw`
                // skips the unbox that `read_operand` does for
                // every other consumer.
                let captured: Vec<Value> = captures
                    .iter()
                    .map(|o| self.read_operand_raw(locals, o))
                    .collect();
                Value::Function(FnValue::Lambda(Rc::new(crate::value::LambdaCapture {
                    function_idx: *function_idx,
                    captured: std::cell::RefCell::new(captured),
                })))
            }
            Rvalue::FunctionRef(def) => {
                // Allow user code to shadow a function name by
                // assignment — the name-keyed global store is
                // checked first. `function f() {}; f = 7;` then
                // `f` reads `7`.
                let name = self
                    .program
                    .functions
                    .iter()
                    .find(|f| f.def_id == Some(*def))
                    .map(|f| f.name.clone());
                if let Some(n) = &name {
                    if let Some(v) = self.globals.get(n).cloned() {
                        return Ok(v);
                    }
                }
                Value::Function(FnValue::User(*def))
            }
            Rvalue::GlobalRef(_, name) => self.globals.get(name).cloned().unwrap_or(Value::Null),
            Rvalue::BuiltinRef(name) => {
                // Shadowing: a name written via `abs = 2` lands
                // in the name-keyed global store. Check that
                // first so subsequent reads see the new value.
                if let Some(v) = self.globals.get(name).cloned() {
                    return Ok(v);
                }
                // Some builtin names resolve to a constant value
                // (`PI`, `INFINITY`, etc.) rather than a function.
                if let Some(v) = leek_runtime::lookup_constant(name) {
                    v
                } else if let Some(canonical) = builtin_class_name(name) {
                    Value::BuiltinClass(canonical)
                } else if leek_runtime::is_known_builtin(name) {
                    Value::Function(FnValue::Builtin(name.clone()))
                } else {
                    // Unknown bare name — neither a builtin, a
                    // constant, nor a builtin class. Treat as an
                    // undefined reference (returns `null`).
                    Value::Null
                }
            }
            Rvalue::This | Rvalue::Super | Rvalue::ClassSelf => {
                // These markers stay in the IR for unreachable
                // edge cases (e.g. `class` outside a method). The
                // lowerer prefers concrete locals when possible.
                Value::Null
            }
            Rvalue::MakeSuper { this, parent_class } => {
                let recv = locals.get(this.0 as usize).cloned().unwrap_or(Value::Null);
                Value::Super(Box::new(SuperValue {
                    parent_class: parent_class.clone(),
                    receiver: Rc::new(recv),
                }))
            }
            Rvalue::ClassRef(def, name) => {
                if let Some(v) = self.globals.get(name).cloned() {
                    return Ok(v);
                }
                Value::ClassRef(*def, Rc::new(name.clone()))
            }
            Rvalue::Unsupported(tag) => {
                return Err(Outcome::Error(format!(
                    "MIR Rvalue::Unsupported({tag}) — feature not lowered to MIR yet"
                )));
            }
        })
    }

    pub(crate) fn materialize_interval(&mut self, locals: &[Value], iv: &IntervalRvalue) -> Value {
        let s = iv
            .start
            .as_ref()
            .map(|o| self.read_operand(locals, o).to_real());
        let e = iv
            .end
            .as_ref()
            .map(|o| self.read_operand(locals, o).to_real());
        // Unbounded endpoints default to *real*-typed so an
        // intersection like `[1..2] ∩ ]..[` widens to `[1.0..2.0]`.
        let start_is_int = iv
            .start
            .as_ref()
            .is_some_and(|o| matches!(self.read_operand(locals, o), Value::Int(_)));
        let end_is_int = iv
            .end
            .as_ref()
            .is_some_and(|o| matches!(self.read_operand(locals, o), Value::Int(_)));
        Value::Interval(Rc::new(IntervalValue {
            start: s,
            end: e,
            start_inclusive: iv.start_inclusive,
            end_inclusive: iv.end_inclusive,
            integer_typed: start_is_int && end_is_int,
            start_is_int,
            end_is_int,
            start_forces_real: iv.start_forces_real,
            end_forces_real: iv.end_forces_real,
        }))
    }
}
