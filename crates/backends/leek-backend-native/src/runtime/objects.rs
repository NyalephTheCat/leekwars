//! Class / object / field shims: instances, member reads & writes
//! (by name and by slot), class reflection (`class_of` / `class_super`),
//! statics, and file-level globals.

#![allow(
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block,
    reason = "FFI conversion pending — see #114"
)]

use super::{
    CLASS_PARENT, CLASS_REFLECT, DISPATCH, GLOBALS, LambdaFn, STATIC_FIELD_OWNER, STATIC_FIELDS,
    STATIC_INIT, STRICT, aborting, builtin_name, builtin_name_ref, handle, raise_runtime_error,
    val,
};
use leek_runtime::{ClassId, Function, Instance, ObjectData, Value};
use std::cell::RefCell;
use std::rc::Rc;

shim! {
    /// `C.staticField` read — returns the stored handle, lazily running the
    /// field's initialiser on first access (a null sentinel is stored first to
    /// break self-referential init cycles). Mirrors upstream's lazy
    /// static-field initialisation.
    pub extern "C" fn leek_static_get(class_def: i64, name: *mut Value) -> *mut Value {
        let Some(field) = (unsafe { builtin_name(name) }) else {
            return handle(Value::Null);
        };
        static_get(class_def as u32, field)
    }
}

/// The storage behind `C.staticField`, lazily initialised. `owner` is the
/// class that *declares* the field, which is not always the one written: a
/// subclass shares its parent's box.
pub(super) fn static_get(owner: u32, field: String) -> *mut Value {
    let key = (owner, field);
    if let Some(h) = STATIC_FIELDS.with(|c| c.borrow().get(&key).copied()) {
        return h;
    }
    // Reserve a null sentinel so a recursive init reads null, not garbage.
    let sentinel = handle(Value::Null);
    STATIC_FIELDS.with(|c| c.borrow_mut().insert(key.clone(), sentinel));
    let init = STATIC_INIT.with(|c| c.borrow().get(&key).copied());
    if let Some(idx) = init
        && let Some((addr, _)) = DISPATCH.with(|c| c.borrow().lambda_fns.get(&idx).copied())
    {
        // SAFETY: `addr` is a finalized body address from the running
        // module's `lambda_fns` table, so it has the `LambdaFn` ABI; a
        // static initialiser takes no captures and no arguments, so an
        // empty `argv` with `argc == 0` is the whole contract.
        let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
        let v = unsafe { f(std::ptr::null(), 0) };
        STATIC_FIELDS.with(|c| c.borrow_mut().insert(key, v));
        return v;
    }
    sentinel
}

/// Which class declares the static field `name` reachable from `class_def`,
/// if any. The owner is what [`static_get`] and `leek_static_set` are keyed
/// on, so a subclass and its parent name one box.
pub(super) fn static_field_owner(class_def: u32, name: &str) -> Option<u32> {
    STATIC_FIELD_OWNER.with(|c| {
        c.borrow()
            .get(&class_def)
            .and_then(|m| m.get(name))
            .copied()
    })
}

/// The static method `name` reachable from `class_def`, as its
/// `program.functions` index.
pub(super) fn static_method_idx(class_def: u32, name: &str) -> Option<usize> {
    DISPATCH.with(|c| {
        c.borrow()
            .static_method_resolve
            .get(&class_def)
            .and_then(|m| m.get(name))
            .copied()
    })
}

shim! {
    /// `C.staticField = v` — store the handle.
    pub extern "C" fn leek_static_set(class_def: i64, name: *mut Value, val: *mut Value) {
        let Some(field) = (unsafe { builtin_name(name) }) else {
            return;
        };
        STATIC_FIELDS.with(|c| c.borrow_mut().insert((class_def as u32, field), val));
    }
}

/// A declared slot type, as the tag the typed-write shims take.
///
/// A typed slot converts what is written to it, and *refuses* what cannot be
/// converted — upstream compiles `a.x = 12` on a `string x` to a Java cast
/// that fails, logs, and leaves the field alone. Conversion is between
/// numbers, booleans and null; everything else keeps its old value.
pub mod slot {
    pub const INTEGER: i64 = 1;
    pub const REAL: i64 = 2;
    pub const BOOLEAN: i64 = 3;
    pub const STRING: i64 = 4;
    pub const BIG_INTEGER: i64 = 5;
    /// A class instance: only an instance (or null) may be stored.
    pub const INSTANCE: i64 = 6;
    /// Added to any of the above for a `T?` slot, where `null` stays `null`
    /// instead of becoming the type's own value.
    pub const NULLABLE: i64 = 8;
}

/// What `value` becomes when stored in a slot declared with `tag`, or `None`
/// when the conversion is impossible and the slot must keep what it has.
pub(super) fn convert_for_slot(value: &Value, tag: i64) -> Option<Value> {
    let nullable = tag & slot::NULLABLE != 0;
    if nullable && matches!(value, Value::Null) {
        return Some(Value::Null);
    }
    let converted = match tag & 7 {
        slot::INTEGER => match value {
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Real(_) | Value::BigInt(_) => {
                Value::Int(value.to_long())
            }
            _ => return None,
        },
        slot::REAL => match value {
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Real(_) | Value::BigInt(_) => {
                Value::Real(value.to_real())
            }
            _ => return None,
        },
        slot::BOOLEAN => match value {
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Real(_) => {
                Value::Bool(value.is_truthy())
            }
            _ => return None,
        },
        // A `string` slot holds a string or null — a Java `String` field, so
        // a number is a cast that fails rather than a conversion.
        slot::STRING => match value {
            Value::Null | Value::String(_) => value.clone(),
            _ => return None,
        },
        slot::BIG_INTEGER => match value {
            Value::BigInt(_) => value.clone(),
            Value::Null => leek_runtime::coerce_value_to_bigint(&Value::Int(0)),
            Value::Bool(_) | Value::Int(_) | Value::Real(_) => {
                leek_runtime::coerce_value_to_bigint(value)
            }
            _ => return None,
        },
        // A class-typed slot takes an instance or null. A scalar is the
        // invalid cast upstream logs and drops.
        slot::INSTANCE => match value {
            Value::Bool(_)
            | Value::Int(_)
            | Value::Real(_)
            | Value::BigInt(_)
            | Value::String(_) => return None,
            _ => value.clone(),
        },
        _ => value.clone(),
    };
    Some(converted)
}

shim! {
    /// The value to store into `base.<name>`, converted to the field's
    /// declared type — or the field's current value when the conversion is
    /// impossible, so the write lands as a no-op.
    ///
    /// # Safety
    /// `base` and `value` must satisfy the
    /// [handle contract](super#handle-safety-contract).
    pub extern "C" fn leek_field_convert(
        base: *mut Value,
        name_ptr: *const u8,
        name_len: i64,
        value: *mut Value,
        tag: i64,
    ) -> *mut Value {
        let name = unsafe { member_name(name_ptr, name_len) };
        // SAFETY: handle contract on `value`.
        let v = unsafe { val(&value) };
        match convert_for_slot(v, tag) {
            Some(converted) => handle(converted),
            // SAFETY: handle contract on `base`.
            None => handle(read_member(unsafe { val(&base) }, name, 4)),
        }
    }
}

shim! {
    /// [`leek_field_convert`] for a static field, whose current value lives in
    /// the owning class's storage rather than an instance.
    ///
    /// # Safety
    /// `value` must satisfy the
    /// [handle contract](super#handle-safety-contract).
    pub extern "C" fn leek_static_convert(
        owner: i64,
        name_ptr: *const u8,
        name_len: i64,
        value: *mut Value,
        tag: i64,
    ) -> *mut Value {
        let name = unsafe { member_name(name_ptr, name_len) };
        // SAFETY: handle contract on `value`.
        let v = unsafe { val(&value) };
        match convert_for_slot(v, tag) {
            Some(converted) => handle(converted),
            None => static_get(owner as u32, name.to_owned()),
        }
    }
}

shim! {
    /// [`leek_field_convert`] for a file-level global, whose current value
    /// lives in the globals table.
    ///
    /// # Safety
    /// `value` must satisfy the
    /// [handle contract](super#handle-safety-contract).
    pub extern "C" fn leek_global_convert(
        name_ptr: *const u8,
        name_len: i64,
        value: *mut Value,
        tag: i64,
    ) -> *mut Value {
        let name = unsafe { member_name(name_ptr, name_len) };
        // SAFETY: handle contract on `value`.
        let v = unsafe { val(&value) };
        match convert_for_slot(v, tag) {
            Some(converted) => handle(converted),
            None => GLOBALS
                .with(|g| g.borrow().get(name).copied())
                .unwrap_or_else(|| handle(Value::Null)),
        }
    }
}

/// The native string-/index-keyed member read shared by `leek_value_index`
/// (boxed key) and [`read_member`] (`&str` key). Returns the value; the caller
/// boxes or coerces it. Mirrors upstream: a runtime class-ref's
/// reflection arrays, an instance's stored field then bound-method fallback,
/// otherwise the shared `read_index_versioned`.
pub(super) fn member_by_value(base: &Value, idx: &Value, version: u8) -> Value {
    // `x.class.fields` (and `.methods` / `.static_fields` / …) on a runtime
    // class-reference value: return the registered reflection name array (a
    // fresh `Array<String>` each read). The compile-time `C.fields` form is
    // handled in the translator; this is for a `ClassRef` reached dynamically.
    if let (Value::ClassRef(def, _), Value::String(member)) = (base, idx)
        && let Some(names) = CLASS_REFLECT.with(|c| {
            c.borrow()
                .get(&def.0)
                .and_then(|m| m.get(member.as_str()).cloned())
        })
    {
        return Value::Array(std::rc::Rc::new(std::cell::RefCell::new(
            names
                .into_iter()
                .map(|n| Value::String(std::rc::Rc::new(n)))
                .collect(),
        )));
    }
    // A static member on a runtime class-reference: `class.x` / `class.m`
    // inside an instance method, where `class` is the *receiver's* class and
    // so is only known here. The compile-time `C.x` form is handled in the
    // translator; this reaches the same storage through the owning class.
    if let (Value::ClassRef(def, _), Value::String(member)) = (base, idx) {
        if let Some(owner) = static_field_owner(def.0, member.as_str()) {
            let h = static_get(owner, member.as_ref().clone());
            // SAFETY: `static_get` returns a live handle from the static-field
            // store, which outlives this read.
            return unsafe { val(&h) }.clone();
        }
        if let Some(idx) = static_method_idx(def.0, member.as_str()) {
            return Value::Function(Function::Lambda(Rc::new(leek_runtime::LambdaCapture {
                function_idx: idx,
                captured: RefCell::new(Vec::new()),
            })));
        }
    }
    // `instance['name']` resolves to a stored field first, then (like
    // upstream's indexed member read) to a bound method.
    if let (Value::Instance(inst), Value::String(name)) = (base, idx) {
        let b = inst.borrow();
        if b.fields.get(name.as_str()).is_none() {
            let class_def = b.class.0;
            drop(b);
            if let Some(fidx) = DISPATCH.with(|c| {
                c.borrow()
                    .method_resolve
                    .get(&class_def)
                    .and_then(|mm| mm.get(name.as_str()))
                    .copied()
            }) {
                return Value::Function(Function::BoundMethod {
                    function_idx: fidx,
                    receiver: Box::new(base.clone()),
                });
            }
        }
    }
    leek_runtime::read_index_versioned(base, idx, version)
}

/// Read member `name` of `base` (the static-name `obj.field` / `obj['name']`
/// path). FAST PATH: an existing instance field needs NO string allocation —
/// borrow the `&str`, clone the value out. Everything else (method fallback,
/// object, class-ref reflection, non-composite) builds the boxed `Value::String`
/// key and uses the exact same [`member_by_value`] logic as `leek_value_index`,
/// so the result is byte-identical to the boxed-key path.
pub(super) fn read_member(base: &Value, name: &str, version: u8) -> Value {
    if let Value::Instance(inst) = base
        && let Some(v) = inst.borrow().fields.get(name)
    {
        return v.clone();
    }
    member_by_value(
        base,
        &Value::String(std::rc::Rc::new(name.to_owned())),
        version,
    )
}

/// Read member `name` of `base` by its compile-time-resolved dense **slot**
/// (its position in the class's `field_layout`). FAST PATH: an instance field
/// reads through `get_slot(slot)` — a direct `Vec` index, skipping the `index`
/// hash that [`read_member`]'s `fields.get(name)` pays. Sound because the native
/// `new_instance` lays every instance's fields out in `field_layout` slot order,
/// and an inherited field keeps the same slot in every subclass, so a base of
/// static class `C` (or any subclass) holds `name` at `slot`. Any non-instance
/// base (e.g. a `null` slot typed as an instance) falls back to the exact
/// [`member_by_value`] name path, so the result is byte-identical to
/// [`read_member`]. The `name` is used only for that cold fallback (and a
/// debug-only slot/name consistency assert).
pub(super) fn read_member_slot(base: &Value, slot: usize, name: &str, version: u8) -> Value {
    if let Value::Instance(inst) = base {
        let b = inst.borrow();
        if let Some(v) = b.fields.get_slot(slot) {
            debug_assert_eq!(
                b.fields.fields.get(slot).map(|(n, _)| n.as_str()),
                Some(name),
                "native field-slot/name mismatch"
            );
            return v.clone();
        }
    }
    member_by_value(
        base,
        &Value::String(std::rc::Rc::new(name.to_owned())),
        version,
    )
}

/// Build a `&str` from a backend-materialised name (`ptr`/`len` of bytes on the
/// caller's stack — valid for the call). The bytes come from a compile-time
/// `&str` literal, so they're valid UTF-8.
unsafe fn member_name<'a>(ptr: *const u8, len: i64) -> &'a str {
    if len <= 0 {
        return "";
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    unsafe { std::str::from_utf8_unchecked(slice) }
}

shim! {
    /// `obj.field` read with the field name passed UNBOXED (`ptr`/`len`), returning
    /// a boxed handle — identical to `leek_value_index` with a boxed string key,
    /// minus the per-read `Value::String` allocation for the key (and skipping it
    /// entirely on the hot instance-field path; see [`read_member`]).
    pub extern "C" fn leek_field_get(
        base: *mut Value,
        name_ptr: *const u8,
        name_len: i64,
        version: i64,
    ) -> *mut Value {
        let name = unsafe { member_name(name_ptr, name_len) };
        handle(read_member(unsafe { val(&base) }, name, version as u8))
    }
}

shim! {
    /// [`leek_field_get`] coerced to an unboxed `i64` (`read_member(..).to_long()`),
    /// for `integer x = obj.field` — byte-identical to `leek_unbox_int` of the boxed
    /// read, with neither key nor result boxed.
    pub extern "C" fn leek_field_get_int(
        base: *mut Value,
        name_ptr: *const u8,
        name_len: i64,
        version: i64,
    ) -> i64 {
        let name = unsafe { member_name(name_ptr, name_len) };
        read_member(unsafe { val(&base) }, name, version as u8).to_long()
    }
}

shim! {
    /// Mirror of [`leek_field_get_int`] returning an unboxed `f64` (`to_real`), for
    /// `real x = obj.field`.
    pub extern "C" fn leek_field_get_real(
        base: *mut Value,
        name_ptr: *const u8,
        name_len: i64,
        version: i64,
    ) -> f64 {
        let name = unsafe { member_name(name_ptr, name_len) };
        read_member(unsafe { val(&base) }, name, version as u8).to_real()
    }
}

shim! {
    /// [`leek_field_get`] with the field's dense `slot` resolved at compile time —
    /// reads through [`read_member_slot`], skipping the field-name hash. `name`
    /// (`ptr`/`len`) is carried only for the cold non-instance fallback.
    pub extern "C" fn leek_field_get_slot(
        base: *mut Value,
        slot: i64,
        name_ptr: *const u8,
        name_len: i64,
        version: i64,
    ) -> *mut Value {
        let name = unsafe { member_name(name_ptr, name_len) };
        handle(read_member_slot(
            unsafe { val(&base) },
            slot as usize,
            name,
            version as u8,
        ))
    }
}

shim! {
    /// Slot-resolved [`leek_field_get_int`] (`read_member_slot(..).to_long()`), for
    /// `integer x = obj.field` on a known class.
    pub extern "C" fn leek_field_get_slot_int(
        base: *mut Value,
        slot: i64,
        name_ptr: *const u8,
        name_len: i64,
        version: i64,
    ) -> i64 {
        let name = unsafe { member_name(name_ptr, name_len) };
        read_member_slot(unsafe { val(&base) }, slot as usize, name, version as u8).to_long()
    }
}

shim! {
    /// Slot-resolved [`leek_field_get_real`] (`read_member_slot(..).to_real()`), for
    /// `real x = obj.field` on a known class.
    pub extern "C" fn leek_field_get_slot_real(
        base: *mut Value,
        slot: i64,
        name_ptr: *const u8,
        name_len: i64,
        version: i64,
    ) -> f64 {
        let name = unsafe { member_name(name_ptr, name_len) };
        read_member_slot(unsafe { val(&base) }, slot as usize, name, version as u8).to_real()
    }
}

/// Shared `base[idx] = value` writeback used by `leek_value_set_index` (boxed
/// key) and [`leek_field_set`]'s fallback (`&str` key built into a `Value`).
///
/// `idx` is a raw pointer, not a `&Value`, on purpose: `a[a] = x` reaches here
/// with `idx == base`, and the morph write-back below stores through `base`. A
/// `&Value` *argument* would be live — and, under Stacked Borrows, protected —
/// across that write, which is undefined behaviour. Taking it raw keeps every
/// borrow derived from `idx` scoped to a single statement that performs no
/// write, so the aliasing case is merely a same-location read-then-write.
///
/// # Safety
/// `base` and `idx` must both satisfy the
/// [module-level handle contract](super#handle-safety-contract); they may alias.
pub(super) unsafe fn set_member(base: *mut Value, idx: *const Value, value: Value, version: u8) {
    // The run already errored: upstream threw, so the store never happens.
    if aborting() {
        return;
    }
    // v4-strict: an out-of-bounds array write is a runtime error
    // (`ARRAY_OUT_OF_BOUND`). Non-strict v4 silently drops the write and
    // v1–v3 promote the array to a sparse map, so the check is gated exactly
    // like upstream's. The write below then no-ops on the
    // OOB index; `run()` surfaces the recorded error after `main` returns.
    if version >= 4
        && STRICT.with(std::cell::Cell::get)
        && let Value::Array(a) = unsafe { val(&base) }
    {
        let len = leek_runtime::len_as_int(a.borrow().len());
        // SAFETY: caller's contract — `idx` is a live handle. The borrow dies
        // at the end of this statement, before any write through `base`.
        let raw = unsafe { &*idx }.as_int().unwrap_or(0);
        let i = if raw < 0 { raw + len } else { raw };
        if i < 0 || i >= len {
            raise_runtime_error("ARRAY_OUT_OF_BOUND");
            return;
        }
    }
    // SAFETY: caller's contract — both are live handles. `set_index` only reads
    // through `idx`, so the two borrows (which alias for `a[a] = x`) are both
    // shared and both end with this statement.
    let morphed = unsafe { leek_runtime::set_index(val(&base), &*idx, value, version) };
    if let Some(new_base) = morphed {
        // SAFETY: `base` is a live, owned handle (leaked box), and no borrow
        // derived from `base` or `idx` is alive here.
        unsafe {
            *base = new_base;
        }
    }
}

shim! {
    /// `obj.field = value` / `obj['field'] = value` with the field name passed
    /// UNBOXED (`ptr`,`len`). For an instance/object base — the target of `.field`
    /// syntax — writes via `set_field` with the `&str` directly (no `Value::String`
    /// key allocation). Any other base type falls back to the shared [`set_member`]
    /// (building the boxed key then), identical to `leek_value_set_index`.
    pub extern "C" fn leek_field_set(
        base: *mut Value,
        name_ptr: *const u8,
        name_len: i64,
        value: *mut Value,
        version: i64,
    ) {
        if aborting() {
            return;
        }
        let name = unsafe { member_name(name_ptr, name_len) };
        let v = unsafe { val(&value) }.clone();
        match unsafe { val(&base) } {
            // `class.x = v` on a runtime class-reference writes the owning
            // class's static box, like the compile-time `C.x = v` form.
            Value::ClassRef(def, _)
                if static_field_owner(def.0, name).is_some() =>
            {
                let owner = static_field_owner(def.0, name).unwrap_or(def.0);
                STATIC_FIELDS
                    .with(|c| c.borrow_mut().insert((owner, name.to_owned()), handle(v)));
            }
            Value::Instance(_) | Value::Object(_) => {
                leek_runtime::set_field(unsafe { val(&base) }, name, v);
            }
            _ => {
                let key = Value::String(std::rc::Rc::new(name.to_owned()));
                unsafe { set_member(base, &raw const key, v, version as u8) };
            }
        }
    }
}

shim! {
    /// [`leek_field_set`] with the field's dense `slot` resolved at compile time.
    /// FAST PATH: an instance field writes through `set_slot(slot, ..)` — a direct
    /// `Vec` index, skipping the `index` hash. Sound for the same reason as
    /// [`read_member_slot`]: a natively-built instance of the known class has `name`
    /// at `slot`. Any other base, or a slot somehow out of range, falls back to the
    /// exact [`leek_field_set`] name path (instance/object `set_field`, else
    /// [`set_member`]). `name` is carried only for that fallback.
    pub extern "C" fn leek_field_set_slot(
        base: *mut Value,
        slot: i64,
        name_ptr: *const u8,
        name_len: i64,
        value: *mut Value,
        version: i64,
    ) {
        if aborting() {
            return;
        }
        if let Value::Instance(inst) = unsafe { val(&base) } {
            let mut b = inst.borrow_mut();
            if (slot as usize) < b.fields.len() {
                let v = unsafe { val(&value) }.clone();
                b.fields.set_slot(slot as usize, v);
                return;
            }
        }
        // Cold fallback (non-instance base, or an unexpectedly out-of-range slot):
        // the exact `leek_field_set` name path.
        let v = unsafe { val(&value) }.clone();
        let name = unsafe { member_name(name_ptr, name_len) };
        match unsafe { val(&base) } {
            Value::Instance(_) | Value::Object(_) => {
                leek_runtime::set_field(unsafe { val(&base) }, name, v);
            }
            _ => {
                let key = Value::String(std::rc::Rc::new(name.to_owned()));
                unsafe { set_member(base, &raw const key, v, version as u8) };
            }
        }
    }
}

shim! {
    pub extern "C" fn leek_object_new() -> *mut Value {
        handle(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
    }
}

shim! {
    /// Allocate a fresh class instance with no fields set. Reads of unset
    /// fields return `null` (matching `leek_runtime`'s `read_field`), so the
    /// emitted `new` only needs to set fields that have initializers. The
    /// field initializers and constructor run as separate emitted calls.
    /// `class_def` is the class's [`ClassId`] raw value; `name_box` is a boxed-string
    /// handle carrying the class name (used by `Display`).
    pub extern "C" fn leek_instance_new(class_def: i64, name_box: *mut Value) -> *mut Value {
        let class_name = match unsafe { val(&name_box) } {
            Value::String(s) => s.to_string(),
            _ => String::new(),
        };
        handle(Value::Instance(Rc::new(RefCell::new(Instance {
            class: ClassId(class_def as u32),
            class_name,
            fields: ObjectData::new(),
        }))))
    }
}

shim! {
    /// Read a global by name (a null handle → a fresh `null`, matching the
    /// upstream's treatment of an unset global).
    pub extern "C" fn leek_global_get(name: *mut Value) -> *mut Value {
        let Some(name) = (unsafe { builtin_name_ref(&name) }) else {
            return handle(Value::Null);
        };
        GLOBALS.with(|g| {
            g.borrow()
                .get(name)
                .copied()
                .unwrap_or_else(|| handle(Value::Null))
        })
    }
}

shim! {
    /// Store a global by name. The handle aliases rather than being copied,
    /// matching v4 reference semantics.
    ///
    /// The displaced handle is simply dropped from the map: it still belongs
    /// to the per-run arena, so `free_run_boxes` reclaims it at run end along
    /// with everything else. Nothing frees it here — freeing at a write site
    /// is how a handle another local still holds becomes a dangling pointer.
    pub extern "C" fn leek_global_set(name: *mut Value, value: *mut Value) {
        if let Some(name) = unsafe { builtin_name(name) } {
            GLOBALS.with(|g| g.borrow_mut().insert(name, value));
        }
    }
}

shim! {
    /// The `.class` meta-property: the runtime class of a value.
    pub extern "C" fn leek_class_of(v: *mut Value) -> *mut Value {
        handle(leek_runtime::class_of(unsafe { val(&v) }))
    }
}

shim! {
    /// `.super` on a (runtime) class value: the parent class. A user class with an
    /// explicit parent yields that class's ref; one with no explicit parent yields
    /// the builtin `Value` base. A non-class value yields null.
    pub extern "C" fn leek_class_super(v: *mut Value) -> *mut Value {
        match unsafe { val(&v) } {
            Value::ClassRef(def, _) => match CLASS_PARENT.with(|c| c.borrow().get(&def.0).cloned()) {
                // Explicit user parent.
                Some(Some((pdef, pname))) => handle(Value::ClassRef(ClassId(pdef), Rc::new(pname))),
                // User class with no explicit parent → the implicit `Value` root.
                Some(None) => handle(Value::BuiltinClass("Value")),
                None => handle(Value::Null),
            },
            // Every builtin class extends the `Value` root; `Value` itself has no
            // super.
            Value::BuiltinClass("Value") => handle(Value::Null),
            Value::BuiltinClass(_) => handle(Value::BuiltinClass("Value")),
            _ => handle(Value::Null),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::set_member;
    use crate::runtime::{
        free_run_boxes, handle, reset_runtime_error, set_strict, take_runtime_error,
    };
    use leek_runtime::Value;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// `Value` has no `PartialEq`; compare arrays through `as_int`.
    fn ints(a: &Rc<RefCell<Vec<Value>>>) -> Vec<Option<i64>> {
        a.borrow().iter().map(Value::as_int).collect()
    }

    /// `a[a] = x`: the index handle IS the base handle. The shim must survive
    /// it, and (an array's `as_int()` being `None`, i.e. 0) write slot 0 —
    /// exactly what a boxed non-integer key does. Run under Miri this also
    /// checks that no borrow derived from the index is live across the
    /// write-back below.
    #[test]
    fn set_member_accepts_an_index_handle_that_aliases_the_base() {
        reset_runtime_error();
        let h = handle(Value::Array(Rc::new(RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
        ]))));
        // SAFETY: `h` is a live handle, deliberately passed as both arguments.
        unsafe { set_member(h, h, Value::Int(9), 1) };
        // SAFETY: `h` is still live.
        let Value::Array(a) = (unsafe { &*h }) else {
            panic!("base morphed unexpectedly")
        };
        assert_eq!(ints(a), [Some(9), Some(2)]);
        free_run_boxes();
    }

    /// v1–v3 write past the end promotes the array to a sparse map, and the new
    /// value must be written back THROUGH the handle so the caller's local sees
    /// it. Guards the write-back Part 2 of #114/#80 reorders.
    #[test]
    fn set_member_writes_a_morphed_base_back_through_the_handle() {
        reset_runtime_error();
        let h = handle(Value::Array(Rc::new(RefCell::new(vec![Value::Int(1)]))));
        let key = Value::Int(5);
        // SAFETY: both are live handles; `key` is a stack temporary.
        unsafe { set_member(h, &raw const key, Value::Int(9), 1) };
        // SAFETY: `h` is still live.
        let Value::Map(m) = (unsafe { &*h }) else {
            panic!("expected the array to morph into a map")
        };
        assert_eq!(
            m.borrow().get(&Value::Int(5)).and_then(Value::as_int),
            Some(9)
        );
        free_run_boxes();
    }

    /// v4-strict reads the index *before* any write, including the negative
    /// wrap (`a[-1]` is the last element). Guards the reordered OOB block.
    #[test]
    fn set_member_v4_strict_bounds_check_reads_the_index_including_the_wrap() {
        reset_runtime_error();
        set_strict(true);
        let h = handle(Value::Array(Rc::new(RefCell::new(vec![
            Value::Int(1),
            Value::Int(2),
        ]))));

        let in_bounds = Value::Int(-1);
        // SAFETY: both are live; `in_bounds` is a stack temporary.
        unsafe { set_member(h, &raw const in_bounds, Value::Int(9), 4) };
        assert_eq!(take_runtime_error(), None);

        let oob = Value::Int(7);
        // SAFETY: both are live; `oob` is a stack temporary.
        unsafe { set_member(h, &raw const oob, Value::Int(9), 4) };
        assert_eq!(take_runtime_error().as_deref(), Some("ARRAY_OUT_OF_BOUND"));

        // SAFETY: `h` is still live.
        let Value::Array(a) = (unsafe { &*h }) else {
            panic!("base morphed unexpectedly")
        };
        assert_eq!(ints(a), [Some(1), Some(9)]);
        set_strict(false);
        free_run_boxes();
    }
}
