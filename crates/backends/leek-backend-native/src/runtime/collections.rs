//! Array / map / set / interval shims: construction, push/put/add,
//! value- and int-indexed reads and writes, slices, counts, and the
//! foreach iterator.

use super::{handle, member_by_value, set_member, val};
use leek_runtime::{IntervalValue, MapData, SetData, Value, key_repr};
use std::cell::RefCell;
use std::rc::Rc;

/// `base[start:end:step]` — slice an array / string / interval. Each bound
/// is a boxed handle; a `null` handle (absent or null-valued bound) means
/// "use the default for that side", matching the interpreter.
#[unsafe(no_mangle)]
pub extern "C" fn leek_slice(
    base: *mut Value,
    start: *mut Value,
    end: *mut Value,
    step: *mut Value,
) -> *mut Value {
    let opt_int = |h: *mut Value| match unsafe { val(h) } {
        Value::Null => None,
        v => Some(v.to_long()),
    };
    let opt_real = |h: *mut Value| match unsafe { val(h) } {
        Value::Null => None,
        v => Some(v.to_real()),
    };
    handle(leek_runtime::slice(
        unsafe { val(base) },
        opt_int(start),
        opt_int(end),
        opt_real(step),
    ))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_array_new() -> *mut Value {
    handle(Value::Array(Rc::new(RefCell::new(Vec::new()))))
}

/// Append a (clone of the) element to an array, in place. Peels a
/// `Value::Cell` receiver (a `@x`-by-ref array returned from a closure can
/// reach here boxed in its shared cell) — `unbox` clones the inner `Value`,
/// which for an array is a shallow `Rc` clone sharing the same backing `Vec`,
/// so the in-place push still mutates the aliased array.
#[unsafe(no_mangle)]
pub extern "C" fn leek_array_push(arr: *mut Value, elem: *mut Value) {
    if let Value::Array(a) = unsafe { val(arr) }.unbox() {
        a.borrow_mut().push(unsafe { val(elem) }.clone());
    }
}

/// Read `base[idx]` for any indexable value (array / string / map / set /
/// object), delegating to the interpreter's `read_index`. `idx` is itself
/// a handle (so map string keys work too).
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_index(base: *mut Value, idx: *mut Value, version: i64) -> *mut Value {
    handle(member_by_value(
        unsafe { val(base) },
        unsafe { val(idx) },
        version as u8,
    ))
}

/// Read `base[idx]` with an **unboxed** integer index, returning a boxed
/// handle. Identical to [`leek_value_index`] called with a boxed `Int` index:
/// that shim's class-reference / instance-method special cases only fire for a
/// `String` (or `ClassRef`) key, so for an integer index it falls straight
/// through to `read_index_versioned` — exactly what this does. The backend uses
/// it for `base[i]` when `i` is statically `integer`, saving one heap box (the
/// index) per read. The result is still a handle, so every consumer is
/// unaffected.
#[unsafe(no_mangle)]
pub extern "C" fn leek_index_int(base: *mut Value, idx: i64, version: i64) -> *mut Value {
    let b = unsafe { val(base) };
    handle(leek_runtime::read_index_versioned(
        b,
        &Value::Int(idx),
        version as u8,
    ))
}

/// Read `base[idx]` (unboxed integer index) and return the element coerced to
/// an **unboxed `i64`** — equivalent to `leek_unbox_int(leek_index_int(..))`
/// (`read_index_versioned(..).to_long()`), with neither the index nor the
/// result ever boxed. Used when the read flows directly into a scalar
/// `integer`-typed slot, whose assignment would `to_long`-coerce the boxed read
/// anyway; the produced value is byte-identical regardless of the element's
/// actual runtime kind (an out-of-bounds `null` reads as `0`, exactly as the
/// boxed path's `to_long(null)` would).
#[unsafe(no_mangle)]
pub extern "C" fn leek_array_get_int(base: *mut Value, idx: i64, version: i64) -> i64 {
    let b = unsafe { val(base) };
    leek_runtime::read_index_versioned(b, &Value::Int(idx), version as u8).to_long()
}

/// Mirror of [`leek_array_get_int`] returning an unboxed `f64` (the read
/// coerced via `to_real`), for a read flowing directly into a `real`-typed
/// slot. Equivalent to `leek_unbox_real(leek_index_int(..))`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_array_get_real(base: *mut Value, idx: i64, version: i64) -> f64 {
    let b = unsafe { val(base) };
    leek_runtime::read_index_versioned(b, &Value::Int(idx), version as u8).to_real()
}

/// Write `base[idx] = value` for any indexable handle (array / map /
/// object), delegating to the interpreter's `set_index`. Both `idx` and
/// `value` are handles. If `base` had to morph to hold the write, the new
/// value is written back into the handle in place.
#[unsafe(no_mangle)]
pub extern "C" fn leek_value_set_index(
    base: *mut Value,
    idx: *mut Value,
    value: *mut Value,
    version: i64,
) {
    let v = unsafe { val(value) }.clone();
    unsafe { set_member(base, val(idx), v, version as u8) };
}

/// `base[idx] = value` with an **unboxed** integer index — identical to
/// [`leek_value_set_index`] called with a boxed `Int` index (the v4-strict OOB
/// check and `set_index` both read the index as an integer), minus the per-write
/// heap box for the index. Used for `a[i] = v` when `i` is statically `integer`.
#[unsafe(no_mangle)]
pub extern "C" fn leek_set_index_int(base: *mut Value, idx: i64, value: *mut Value, version: i64) {
    let v = unsafe { val(value) }.clone();
    unsafe { set_member(base, &Value::Int(idx), v, version as u8) };
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_map_new() -> *mut Value {
    handle(Value::Map(Rc::new(RefCell::new(MapData::new()))))
}

/// Insert `key → value` into a map, with the interpreter's key
/// canonicalization (so collection keys reduce the same way).
#[unsafe(no_mangle)]
pub extern "C" fn leek_map_put(map: *mut Value, key: *mut Value, value: *mut Value) {
    if let Value::Map(m) = unsafe { val(map) } {
        let k = unsafe { val(key) }.clone();
        let v = unsafe { val(value) }.clone();
        let canon = key_repr(&k);
        m.borrow_mut().insert_canonical(canon, k, v);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_set_new() -> *mut Value {
    handle(Value::Set(Rc::new(RefCell::new(SetData::new()))))
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_set_add(set: *mut Value, elem: *mut Value) {
    if let Value::Set(s) = unsafe { val(set) } {
        s.borrow_mut().insert(unsafe { val(elem) }.clone());
    }
}

/// Add the integer range `start..end` (inclusive, both directions) to a set
/// literal under construction (#2335). Charges 1 op up front plus 1 op per
/// element *inside* the loop, mirroring `AI.setLiteralRange` — the op budget
/// bounds execution, so extreme bounds (`<MIN..MAX>`) can't exhaust host
/// memory or overflow an `end - start` length computation.
#[unsafe(no_mangle)]
pub extern "C" fn leek_set_add_range(set: *mut Value, start: *mut Value, end: *mut Value) {
    let start = unsafe { val(start) }.to_long();
    let end = unsafe { val(end) }.to_long();
    super::leek_charge_ops(1);
    if let Value::Set(s) = unsafe { val(set) } {
        let mut s = s.borrow_mut();
        let step: i64 = if start <= end { 1 } else { -1 };
        let mut i = start;
        loop {
            super::leek_charge_ops(1);
            if super::leek_op_budget_exceeded() != 0 {
                return;
            }
            s.insert(Value::Int(i));
            if i == end {
                break;
            }
            i += step;
        }
    }
}

/// Build an interval value `[start..end]` from boxed endpoint handles
/// (a null handle means an unbounded end). `flags` packs inclusivity and
/// the `Infinity`-forces-real bits: bit0 start-inclusive, bit1
/// end-inclusive, bit2 start-forces-real, bit3 end-forces-real. Mirrors
/// the interpreter's `materialize_interval` (step is ignored, as there).
#[unsafe(no_mangle)]
pub extern "C" fn leek_interval(start: *mut Value, end: *mut Value, flags: i64) -> *mut Value {
    let bound = |p: *mut Value| -> (Option<f64>, bool) {
        if p.is_null() {
            (None, false)
        } else {
            let v = unsafe { val(p) };
            (Some(v.to_real()), matches!(v, Value::Int(_)))
        }
    };
    let (s, start_is_int) = bound(start);
    let (e, end_is_int) = bound(end);
    handle(Value::Interval(Rc::new(IntervalValue {
        start: s,
        end: e,
        start_inclusive: flags & 1 != 0,
        end_inclusive: flags & 2 != 0,
        integer_typed: start_is_int && end_is_int,
        start_is_int,
        end_is_int,
        start_forces_real: flags & 4 != 0,
        end_forces_real: flags & 8 != 0,
    })))
}

/// Element count of an array / map / set (0 otherwise). A string counts
/// its characters only in v4 — v1–v3 `count("…")` is 0 (strings aren't
/// collections there).
#[unsafe(no_mangle)]
pub extern "C" fn leek_count(p: *mut Value, version: i64) -> i64 {
    match unsafe { val(p) } {
        Value::Array(a) => a.borrow().len() as i64,
        Value::Map(m) => m.borrow().len() as i64,
        Value::Set(s) => s.borrow().len() as i64,
        Value::String(s) if version >= 4 => s.chars().count() as i64,
        _ => 0,
    }
}

/// Build a `foreach` iterator (`[key, value]` pairs) for an iterable.
#[unsafe(no_mangle)]
pub extern "C" fn leek_foreach_iter(iterable: *mut Value) -> *mut Value {
    handle(leek_runtime::make_foreach_iter(unsafe { val(iterable) }))
}
