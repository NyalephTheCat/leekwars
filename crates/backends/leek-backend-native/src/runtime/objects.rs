//! Class / object / field shims: instances, member reads & writes
//! (by name and by slot), class reflection (`class_of` / `class_super`),
//! statics, and file-level globals.

use super::{
    CLASS_PARENT, CLASS_REFLECT, DISPATCH, GLOBALS, LambdaFn, STATIC_FIELDS, STATIC_INIT, STRICT,
    builtin_name, builtin_name_ref, handle, raise_runtime_error, val,
};
use leek_hir::DefId;
use leek_runtime::{Function, Instance, ObjectData, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// `C.staticField` read — returns the stored handle, lazily running the
/// field's initialiser on first access (a null sentinel is stored first to
/// break self-referential init cycles). Mirrors the interpreter's lazy
/// static-field initialisation.
#[unsafe(no_mangle)]
pub extern "C" fn leek_static_get(class_def: i64, name: *mut Value) -> *mut Value {
    let Some(field) = builtin_name(name) else {
        return handle(Value::Null);
    };
    let key = (class_def as u32, field);
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
        let f: LambdaFn = unsafe { std::mem::transmute::<*const u8, LambdaFn>(addr) };
        let v = f(std::ptr::null(), 0);
        STATIC_FIELDS.with(|c| c.borrow_mut().insert(key, v));
        return v;
    }
    sentinel
}

/// `C.staticField = v` — store the handle.
#[unsafe(no_mangle)]
pub extern "C" fn leek_static_set(class_def: i64, name: *mut Value, val: *mut Value) {
    let Some(field) = builtin_name(name) else {
        return;
    };
    STATIC_FIELDS.with(|c| c.borrow_mut().insert((class_def as u32, field), val));
}

/// The native string-/index-keyed member read shared by [`leek_value_index`]
/// (boxed key) and [`read_member`] (`&str` key). Returns the value; the caller
/// boxes or coerces it. Mirrors the interpreter: a runtime class-ref's
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
    // `instance['name']` resolves to a stored field first, then (like the
    // interpreter's `read_index_with_methods`) to a bound method.
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
/// key and uses the exact same [`member_by_value`] logic as [`leek_value_index`],
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

/// `obj.field` read with the field name passed UNBOXED (`ptr`/`len`), returning
/// a boxed handle — identical to [`leek_value_index`] with a boxed string key,
/// minus the per-read `Value::String` allocation for the key (and skipping it
/// entirely on the hot instance-field path; see [`read_member`]).
#[unsafe(no_mangle)]
pub extern "C" fn leek_field_get(
    base: *mut Value,
    name_ptr: *const u8,
    name_len: i64,
    version: i64,
) -> *mut Value {
    let name = unsafe { member_name(name_ptr, name_len) };
    handle(read_member(unsafe { val(base) }, name, version as u8))
}

/// [`leek_field_get`] coerced to an unboxed `i64` (`read_member(..).to_long()`),
/// for `integer x = obj.field` — byte-identical to `leek_unbox_int` of the boxed
/// read, with neither key nor result boxed.
#[unsafe(no_mangle)]
pub extern "C" fn leek_field_get_int(
    base: *mut Value,
    name_ptr: *const u8,
    name_len: i64,
    version: i64,
) -> i64 {
    let name = unsafe { member_name(name_ptr, name_len) };
    read_member(unsafe { val(base) }, name, version as u8).to_long()
}

/// Mirror of [`leek_field_get_int`] returning an unboxed `f64` (`to_real`), for
/// `real x = obj.field`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_field_get_real(
    base: *mut Value,
    name_ptr: *const u8,
    name_len: i64,
    version: i64,
) -> f64 {
    let name = unsafe { member_name(name_ptr, name_len) };
    read_member(unsafe { val(base) }, name, version as u8).to_real()
}

/// [`leek_field_get`] with the field's dense `slot` resolved at compile time —
/// reads through [`read_member_slot`], skipping the field-name hash. `name`
/// (`ptr`/`len`) is carried only for the cold non-instance fallback.
#[unsafe(no_mangle)]
pub extern "C" fn leek_field_get_slot(
    base: *mut Value,
    slot: i64,
    name_ptr: *const u8,
    name_len: i64,
    version: i64,
) -> *mut Value {
    let name = unsafe { member_name(name_ptr, name_len) };
    handle(read_member_slot(
        unsafe { val(base) },
        slot as usize,
        name,
        version as u8,
    ))
}

/// Slot-resolved [`leek_field_get_int`] (`read_member_slot(..).to_long()`), for
/// `integer x = obj.field` on a known class.
#[unsafe(no_mangle)]
pub extern "C" fn leek_field_get_slot_int(
    base: *mut Value,
    slot: i64,
    name_ptr: *const u8,
    name_len: i64,
    version: i64,
) -> i64 {
    let name = unsafe { member_name(name_ptr, name_len) };
    read_member_slot(unsafe { val(base) }, slot as usize, name, version as u8).to_long()
}

/// Slot-resolved [`leek_field_get_real`] (`read_member_slot(..).to_real()`), for
/// `real x = obj.field` on a known class.
#[unsafe(no_mangle)]
pub extern "C" fn leek_field_get_slot_real(
    base: *mut Value,
    slot: i64,
    name_ptr: *const u8,
    name_len: i64,
    version: i64,
) -> f64 {
    let name = unsafe { member_name(name_ptr, name_len) };
    read_member_slot(unsafe { val(base) }, slot as usize, name, version as u8).to_real()
}

/// Shared `base[idx] = value` writeback used by [`leek_value_set_index`] (boxed
/// key) and [`leek_field_set`]'s fallback (`&str` key built into a `Value`).
///
/// # Safety
/// `base` must be a live handle.
pub(super) unsafe fn set_member(base: *mut Value, idx: &Value, value: Value, version: u8) {
    // v4-strict: an out-of-bounds array write is a runtime error
    // (`ARRAY_OUT_OF_BOUND`). Non-strict v4 silently drops the write and
    // v1–v3 promote the array to a sparse map, so the check is gated exactly
    // like the interpreter's (`exec.rs`). The write below then no-ops on the
    // OOB index; `run()` surfaces the recorded error after `main` returns.
    if version >= 4
        && STRICT.with(std::cell::Cell::get)
        && let Value::Array(a) = unsafe { val(base) }
    {
        let len = leek_runtime::len_as_int(a.borrow().len());
        let raw = idx.as_int().unwrap_or(0);
        let i = if raw < 0 { raw + len } else { raw };
        if i < 0 || i >= len {
            raise_runtime_error("ARRAY_OUT_OF_BOUND");
            return;
        }
    }
    let morphed = leek_runtime::set_index(unsafe { val(base) }, idx, value, version);
    if let Some(new_base) = morphed {
        // SAFETY: `base` is a live, owned handle (leaked box).
        unsafe {
            *base = new_base;
        }
    }
}

/// `obj.field = value` / `obj['field'] = value` with the field name passed
/// UNBOXED (`ptr`,`len`). For an instance/object base — the target of `.field`
/// syntax — writes via `set_field` with the `&str` directly (no `Value::String`
/// key allocation). Any other base type falls back to the shared [`set_member`]
/// (building the boxed key then), identical to [`leek_value_set_index`].
#[unsafe(no_mangle)]
pub extern "C" fn leek_field_set(
    base: *mut Value,
    name_ptr: *const u8,
    name_len: i64,
    value: *mut Value,
    version: i64,
) {
    let name = unsafe { member_name(name_ptr, name_len) };
    let v = unsafe { val(value) }.clone();
    match unsafe { val(base) } {
        Value::Instance(_) | Value::Object(_) => {
            leek_runtime::set_field(unsafe { val(base) }, name, v);
        }
        _ => unsafe {
            set_member(
                base,
                &Value::String(std::rc::Rc::new(name.to_owned())),
                v,
                version as u8,
            );
        },
    }
}

/// [`leek_field_set`] with the field's dense `slot` resolved at compile time.
/// FAST PATH: an instance field writes through `set_slot(slot, ..)` — a direct
/// `Vec` index, skipping the `index` hash. Sound for the same reason as
/// [`read_member_slot`]: a natively-built instance of the known class has `name`
/// at `slot`. Any other base, or a slot somehow out of range, falls back to the
/// exact [`leek_field_set`] name path (instance/object `set_field`, else
/// [`set_member`]). `name` is carried only for that fallback.
#[unsafe(no_mangle)]
pub extern "C" fn leek_field_set_slot(
    base: *mut Value,
    slot: i64,
    name_ptr: *const u8,
    name_len: i64,
    value: *mut Value,
    version: i64,
) {
    if let Value::Instance(inst) = unsafe { val(base) } {
        let mut b = inst.borrow_mut();
        if (slot as usize) < b.fields.len() {
            let v = unsafe { val(value) }.clone();
            b.fields.set_slot(slot as usize, v);
            return;
        }
    }
    // Cold fallback (non-instance base, or an unexpectedly out-of-range slot):
    // the exact `leek_field_set` name path.
    let v = unsafe { val(value) }.clone();
    let name = unsafe { member_name(name_ptr, name_len) };
    match unsafe { val(base) } {
        Value::Instance(_) | Value::Object(_) => {
            leek_runtime::set_field(unsafe { val(base) }, name, v);
        }
        _ => unsafe {
            set_member(
                base,
                &Value::String(std::rc::Rc::new(name.to_owned())),
                v,
                version as u8,
            );
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_object_new() -> *mut Value {
    handle(Value::Object(Rc::new(RefCell::new(ObjectData::new()))))
}

/// Allocate a fresh class instance with no fields set. Reads of unset
/// fields return `null` (matching the interpreter's `read_field`), so the
/// emitted `new` only needs to set fields that have initializers. The
/// field initializers and constructor run as separate emitted calls.
/// `class_def` is the class's `DefId.0`; `name_box` is a boxed-string
/// handle carrying the class name (used by `Display`).
#[unsafe(no_mangle)]
pub extern "C" fn leek_instance_new(class_def: i64, name_box: *mut Value) -> *mut Value {
    let class_name = match unsafe { val(name_box) } {
        Value::String(s) => s.to_string(),
        _ => String::new(),
    };
    handle(Value::Instance(Rc::new(RefCell::new(Instance {
        class: DefId(class_def as u32),
        class_name,
        fields: ObjectData::new(),
    }))))
}

/// Read a global by name (a null handle → a fresh `null`, matching the
/// interpreter's treatment of an unset global).
#[unsafe(no_mangle)]
pub extern "C" fn leek_global_get(name: *mut Value) -> *mut Value {
    let Some(name) = builtin_name_ref(name) else {
        return handle(Value::Null);
    };
    GLOBALS.with(|g| {
        g.borrow()
            .get(name)
            .copied()
            .unwrap_or_else(|| handle(Value::Null))
    })
}

/// Store a global by name (the handle aliases, matching v4 reference
/// semantics; the previous handle is left to leak).
#[unsafe(no_mangle)]
pub extern "C" fn leek_global_set(name: *mut Value, value: *mut Value) {
    if let Some(name) = builtin_name(name) {
        GLOBALS.with(|g| g.borrow_mut().insert(name, value));
    }
}

/// Call a stdlib builtin by name on boxed argument handles, dispatching
/// through the shared `leek_runtime::call_builtin` with a trivial native
/// host. One shim per small arity (most builtins take ≤ 3 args).
/// The `.class` meta-property: the runtime class of a value.
#[unsafe(no_mangle)]
pub extern "C" fn leek_class_of(v: *mut Value) -> *mut Value {
    handle(leek_runtime::class_of(unsafe { val(v) }))
}

/// `.super` on a (runtime) class value: the parent class. A user class with an
/// explicit parent yields that class's ref; one with no explicit parent yields
/// the builtin `Value` base. A non-class value yields null.
#[unsafe(no_mangle)]
pub extern "C" fn leek_class_super(v: *mut Value) -> *mut Value {
    match unsafe { val(v) } {
        Value::ClassRef(def, _) => match CLASS_PARENT.with(|c| c.borrow().get(&def.0).cloned()) {
            // Explicit user parent.
            Some(Some((pdef, pname))) => handle(Value::ClassRef(DefId(pdef), Rc::new(pname))),
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
