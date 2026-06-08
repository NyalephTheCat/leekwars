//! Pure value semantics shared by every execution backend.
//!
//! These functions implement Leekscript's runtime behavior on [`Value`]s —
//! binary/unary operators, indexing, iteration, equality, deep-clone — with
//! no dependency on any IR or interpreter state. The interpreter *calls*
//! them as it walks the tree; the native backend *links* them as C-ABI
//! runtime functions. Keeping them here (rather than in a backend) is what
//! lets both share one implementation.
//!
//! The binary operator is exposed as one function per operator (`add`,
//! `sub`, …) rather than a single `apply(op, …)` so that `leek-runtime`
//! need not depend on `leek-mir`'s `BinOp` — each backend maps its own op
//! enum onto these.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use crate::value::{Instance, IntervalValue, MapData, ObjectData, SetData};
use crate::{key_repr, Value};

// ---- slicing (`a[start:end:step]`) ----

/// `base[start:end:step]` for arrays, strings, and intervals. Shared by the
/// interpreter and the native backend so the semantics match.
pub fn slice(base: &Value, start: Option<i64>, end: Option<i64>, step: Option<f64>) -> Value {
    let int_step = step.map(crate::real_to_int);
    match base {
        Value::Array(a) => {
            let arr = a.borrow();
            let len = crate::len_as_int(arr.len());
            Value::Array(Rc::new(RefCell::new(slice_seq(&arr, len, start, end, int_step))))
        }
        Value::String(s) => {
            let bytes = s.as_bytes();
            let len = crate::len_as_int(bytes.len());
            let sliced = slice_seq(bytes, len, start, end, int_step);
            Value::String(Rc::new(String::from_utf8_lossy(&sliced).into_owned()))
        }
        Value::Interval(iv) => slice_interval(iv, start, end, step),
        _ => Value::Null,
    }
}

fn slice_interval(
    iv: &Rc<IntervalValue>,
    start: Option<i64>,
    end: Option<i64>,
    step: Option<f64>,
) -> Value {
    let (Some(from), Some(to)) = (iv.start, iv.end) else {
        return Value::Null;
    };
    let step_f = step.unwrap_or(1.0);
    let step_f = if step_f == 0.0 { 1.0 } else { step_f };
    let max_size = crate::real_to_int((to - from) / step_f.abs()) + 1;
    let start_i = start.unwrap_or(0);
    let end_i = end.unwrap_or(max_size);
    let resolve = |i: i64, cap: i64| -> i64 {
        if i < 0 {
            (max_size + i).max(0).min(cap)
        } else {
            i.max(0).min(cap)
        }
    };
    let min_idx = resolve(start_i, max_size);
    let max_idx = resolve(end_i, max_size);
    let mut out: Vec<Value> = Vec::new();
    let base_v = if step_f >= 0.0 { from } else { to };
    let mut i = min_idx;
    while i < max_idx {
        out.push(Value::Real(base_v + (crate::int_to_real(i)) * step_f));
        i += 1;
    }
    Value::Array(Rc::new(RefCell::new(out)))
}

fn slice_seq<T: Clone>(items: &[T], len: i64, s: Option<i64>, e: Option<i64>, st: Option<i64>) -> Vec<T> {
    let step = st.unwrap_or(1);
    if step == 0 {
        return Vec::new();
    }
    let (default_start, default_end) = if step > 0 { (0, len) } else { (len - 1, -1) };
    let resolve = |i: i64| -> i64 {
        let i = if i < 0 { i + len } else { i };
        if step > 0 {
            i.clamp(0, len)
        } else {
            i.clamp(-1, len - 1)
        }
    };
    let s = s.map_or(default_start, resolve);
    let e = e.map_or(default_end, resolve);
    let mut out = Vec::new();
    let mut i = s;
    if step > 0 {
        while i < e && i >= 0 && i < len {
            out.push(items[crate::clamp_index(i)].clone());
            i += step;
        }
    } else {
        while i > e && i >= 0 && i < len {
            out.push(items[crate::clamp_index(i)].clone());
            i += step;
        }
    }
    out
}

// ---- foreach iteration ----

/// Snapshot `v` into an `Array<[key, value]>` for `foreach`. Arrays iterate
/// by index, maps by entry, strings by byte position, intervals by unit
/// step, sets by element (synthetic integer keys), objects/instances by
/// field order. Non-iterables yield an empty array.
pub fn make_foreach_iter(v: &Value) -> Value {
    let mut pairs: Vec<Value> = Vec::new();
    let push_pair = |pairs: &mut Vec<Value>, k: Value, v: Value| {
        pairs.push(Value::Array(Rc::new(RefCell::new(vec![k, v]))));
    };
    match v {
        Value::Array(a) => {
            for (i, el) in a.borrow().iter().enumerate() {
                push_pair(&mut pairs, Value::Int(crate::len_as_int(i)), el.clone());
            }
        }
        Value::Map(m) => {
            for (k, val) in &m.borrow().entries {
                push_pair(&mut pairs, k.clone(), val.clone());
            }
        }
        Value::Set(s) => {
            for (i, el) in s.borrow().iter().enumerate() {
                push_pair(&mut pairs, Value::Int(crate::len_as_int(i)), el.clone());
            }
        }
        Value::Object(o) => {
            for (name, val) in o.borrow().iter() {
                push_pair(&mut pairs, Value::String(Rc::new(name.clone())), val.clone());
            }
        }
        Value::Instance(inst) => {
            for (name, val) in &inst.borrow().fields {
                push_pair(&mut pairs, Value::String(Rc::new(name.clone())), val.clone());
            }
        }
        Value::String(s) => {
            for (i, b) in s.as_bytes().iter().enumerate() {
                push_pair(
                    &mut pairs,
                    Value::Int(crate::len_as_int(i)),
                    Value::String(Rc::new((*b as char).to_string())),
                );
            }
        }
        Value::Interval(iv) => {
            let (Some(start), Some(end)) = (iv.start, iv.end) else {
                return Value::Array(Rc::new(RefCell::new(pairs)));
            };
            let lo = if iv.start_inclusive { start } else { start + 1.0 };
            let hi = if iv.end_inclusive { end } else { end - 1.0 };
            let mut x = lo;
            let mut i = 0i64;
            while x <= hi {
                let v = if iv.integer_typed {
                    Value::Int(crate::real_to_int(x))
                } else {
                    Value::Real(x)
                };
                push_pair(&mut pairs, Value::Int(i), v);
                x += 1.0;
                i += 1;
            }
        }
        _ => {}
    }
    Value::Array(Rc::new(RefCell::new(pairs)))
}

// ---- indexing & fields ----

pub fn read_field(base: &Value, name: &str) -> Value {
    match base {
        Value::Object(o) => o.borrow().get(name).cloned().unwrap_or(Value::Null),
        Value::Instance(inst) => inst.borrow().fields.get(name).cloned().unwrap_or(Value::Null),
        // A builtin class's members: a static field (`Integer.MAX_VALUE`),
        // its `name`, or reflection arrays (`Array.fields`/`.methods`/… are
        // empty for builtins).
        Value::BuiltinClass(cls) => match name {
            "name" => Value::String(Rc::new((*cls).to_string())),
            "fields" | "methods" | "static_fields" | "static_methods" | "constructors" => {
                Value::Array(Rc::new(RefCell::new(Vec::new())))
            }
            _ => builtin_class_static(cls, name).unwrap_or(Value::Null),
        },
        // A user class reference's `name` is carried on the value itself
        // (reflection like `.fields` needs the program, so isn't here).
        Value::ClassRef(_, cls_name) if name == "name" => {
            Value::String(Rc::new(cls_name.as_ref().clone()))
        }
        _ => Value::Null,
    }
}

/// The runtime class of a value — its `.class` meta-property. Primitives
/// and composites yield a `BuiltinClass`; a user instance yields a
/// `ClassRef` (class id + name, both stored on the instance). A class
/// value's class is `Class`. Pure `Value` logic, shared by both backends.
pub fn class_of(v: &Value) -> Value {
    match v {
        Value::Null => Value::BuiltinClass("Null"),
        Value::Bool(_) => Value::BuiltinClass("Boolean"),
        Value::Int(_) => Value::BuiltinClass("Integer"),
        Value::Real(_) => Value::BuiltinClass("Real"),
        Value::String(_) => Value::BuiltinClass("String"),
        Value::Array(_) => Value::BuiltinClass("Array"),
        Value::Map(_) => Value::BuiltinClass("Map"),
        Value::Set(_) => Value::BuiltinClass("Set"),
        Value::Object(_) => Value::BuiltinClass("Object"),
        Value::Interval(_) => Value::BuiltinClass("Interval"),
        Value::Function(_) => Value::BuiltinClass("Function"),
        Value::ClassRef(_, _) | Value::BuiltinClass(_) => Value::BuiltinClass("Class"),
        Value::Instance(i) => {
            let b = i.borrow();
            Value::ClassRef(b.class, Rc::new(b.class_name.clone()))
        }
        Value::Super { receiver, .. } => class_of(receiver),
        Value::Cell(c) => class_of(&c.borrow()),
    }
}

/// The canonical `&'static str` for a builtin class name (`Integer`,
/// `Array`, `Map`, …), or `None` if `name` isn't a builtin class. Shared
/// by both backends so a class reference (`var c = Array`, `x instanceof
/// Map`, `Integer.MAX_VALUE`) resolves identically.
pub fn builtin_class_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "Array" => "Array",
        "Map" => "Map",
        "Set" => "Set",
        "Object" => "Object",
        "Number" => "Number",
        "Integer" => "Integer",
        "Real" => "Real",
        "Float" => "Float",
        "String" => "String",
        "Boolean" => "Boolean",
        "Null" => "Null",
        "Function" => "Function",
        "Class" => "Class",
        "Interval" => "Interval",
        "Error" => "Error",
        "Value" => "Value",
        "JSON" => "JSON",
        "System" => "System",
        _ => return None,
    })
}

/// A builtin class's static field value (`Integer.MAX_VALUE`,
/// `Real.NaN`, …), or `None` when the class has no such field.
pub fn builtin_class_static(class: &str, field: &str) -> Option<Value> {
    use std::f64::consts;
    Some(match (class, field) {
        ("Integer", "MIN_VALUE") => Value::Int(i64::MIN),
        ("Integer", "MAX_VALUE") => Value::Int(i64::MAX),
        // Java `Double.MIN_VALUE` is the smallest positive non-zero double
        // (~4.9e-324, a subnormal) — `from_bits(1)`, not `MIN_POSITIVE`.
        ("Real" | "Number" | "Float", "MIN_VALUE") => Value::Real(f64::from_bits(1)),
        ("Real" | "Number" | "Float", "MAX_VALUE") => Value::Real(f64::MAX),
        ("Real" | "Number" | "Float", "POSITIVE_INFINITY") => Value::Real(f64::INFINITY),
        ("Real" | "Number" | "Float", "NEGATIVE_INFINITY") => Value::Real(f64::NEG_INFINITY),
        ("Real" | "Number" | "Float", "NaN") => Value::Real(f64::NAN),
        ("Real" | "Number" | "Float", "PI") => Value::Real(consts::PI),
        ("Real" | "Number" | "Float", "E") => Value::Real(consts::E),
        _ => return None,
    })
}

/// Construct a builtin class instance (`new Array()`, `new Map()`,
/// `new Integer(5)`, …). Unknown classes yield `Null`.
pub fn construct_builtin_class(name: &str, args: Vec<Value>) -> Value {
    match name {
        "Array" => Value::Array(Rc::new(RefCell::new(args))),
        "Map" => Value::Map(Rc::new(RefCell::new(MapData::new()))),
        "Set" => Value::Set(Rc::new(RefCell::new(args.into_iter().collect::<SetData>()))),
        "Object" => Value::Object(Rc::new(RefCell::new(ObjectData::new()))),
        "Integer" => Value::Int(args.first().and_then(super::value::types::Value::as_int).unwrap_or(0)),
        "Number" | "Real" | "Float" => {
            Value::Real(args.first().and_then(super::value::types::Value::as_real).unwrap_or(0.0))
        }
        "String" => Value::String(Rc::new(
            args.first().map(std::string::ToString::to_string).unwrap_or_default(),
        )),
        "Boolean" => Value::Bool(args.first().is_some_and(super::value::types::Value::is_truthy)),
        "Interval" => Value::Interval(Rc::new(IntervalValue {
            start: None,
            end: None,
            start_inclusive: true,
            end_inclusive: true,
            integer_typed: true,
            start_is_int: true,
            end_is_int: true,
            start_forces_real: false,
            end_forces_real: false,
        })),
        _ => Value::Null,
    }
}

/// Version-aware indexing. In v1–v3, map lookups coerce the key the way
/// `LegacyArray` did — a real index truncates to an integer (`m[5.7]` on
/// `[5: 12]` finds `12`), and collection keys collapse via
/// [`legacy_map_key`]. v4 uses strict key equality. Falls through to the
/// version-independent [`read_index`].
pub fn read_index_versioned(base: &Value, idx: &Value, version: u8) -> Value {
    if version <= 3 {
        if let (Value::Map(m), Value::Real(r)) = (base, idx) {
            let truncated = Value::Int(crate::real_to_int(*r));
            if let Some(v) = m.borrow().get(&truncated).cloned() {
                return v;
            }
        }
        if let Value::Map(m) = base {
            let key = legacy_map_key(idx);
            if let Some(v) = m.borrow().get(&key).cloned() {
                return v;
            }
        }
    }
    read_index(base, idx)
}

pub fn read_index(base: &Value, idx: &Value) -> Value {
    match base {
        Value::Array(a) => {
            let arr = a.borrow();
            let len = crate::len_as_int(arr.len());
            let raw = idx.as_int().unwrap_or(0);
            let i = if raw < 0 { raw + len } else { raw };
            if i < 0 || i >= len {
                Value::Null
            } else {
                arr[crate::clamp_index(i)].clone()
            }
        }
        Value::Map(m) => m.borrow().get(idx).cloned().unwrap_or(Value::Null),
        Value::Set(s) => {
            if s.borrow().contains(idx) {
                idx.clone()
            } else {
                Value::Null
            }
        }
        Value::String(s) => {
            let bytes = s.as_bytes();
            let len = crate::len_as_int(bytes.len());
            let raw = idx.as_int().unwrap_or(0);
            let i = if raw < 0 { raw + len } else { raw };
            if i < 0 || i >= len {
                Value::Null
            } else {
                Value::String(Rc::new((bytes[crate::clamp_index(i)] as char).to_string()))
            }
        }
        Value::Object(_) | Value::Instance(_) | Value::BuiltinClass(_) | Value::ClassRef(_, _) => {
            let key = match idx {
                Value::String(s) => s.as_ref().clone(),
                other => other.to_string(),
            };
            read_field(base, &key)
        }
        _ => Value::Null,
    }
}

pub fn set_field(base: &Value, name: &str, value: Value) {
    match base {
        Value::Instance(inst) => inst.borrow_mut().fields.set(name, value),
        Value::Object(o) => o.borrow_mut().set(name, value),
        _ => {}
    }
}

/// Mutate `base[index] = value`. Returns `Some(new_base)` when `base` had
/// to morph to hold the write (a v1–v3 array promoting to a sparse map);
/// the caller writes that back into the slot that held `base`.
pub fn set_index(base: &Value, index: &Value, value: Value, version: u8) -> Option<Value> {
    if let Value::Object(_) | Value::Instance(_) = base {
        let key = match index {
            Value::String(s) => s.as_ref().clone(),
            other => other.to_string(),
        };
        set_field(base, &key, value);
        return None;
    }
    match base {
        Value::Array(a) => {
            let len = crate::len_as_int(a.borrow().len());
            let raw = index.as_int().unwrap_or(0);
            let i = if raw < 0 { raw + len } else { raw };
            if i < 0 {
                if version <= 3 {
                    let mut map = MapData::new();
                    for (j, v) in a.borrow().iter().enumerate() {
                        let k = Value::Int(crate::len_as_int(j));
                        map.insert_canonical(key_repr(&k), k, v.clone());
                    }
                    let k = Value::Int(raw);
                    map.insert_canonical(key_repr(&k), k, value);
                    return Some(Value::Map(Rc::new(RefCell::new(map))));
                }
                return None;
            }
            let i = crate::clamp_index(i);
            let mut arr = a.borrow_mut();
            if i < arr.len() {
                arr[i] = value;
                None
            } else if i == arr.len() && version <= 3 {
                arr.push(value);
                None
            } else if version >= 4 {
                None
            } else {
                let mut map = MapData::new();
                for (j, v) in arr.iter().enumerate() {
                    let k = Value::Int(crate::len_as_int(j));
                    map.insert_canonical(key_repr(&k), k, v.clone());
                }
                let k = Value::Int(crate::len_as_int(i));
                map.insert_canonical(key_repr(&k), k, value);
                drop(arr);
                Some(Value::Map(Rc::new(RefCell::new(map))))
            }
        }
        Value::Map(m) => {
            let key = if version <= 3 {
                legacy_map_key(index)
            } else {
                index.clone()
            };
            let stored = if version <= 1 {
                deep_clone(&value)
            } else {
                value
            };
            // Compute the canonical key *before* taking the mutable borrow
            // — `key_repr` may itself read the map (e.g. `m[m] = …`).
            let canonical = key_repr(&key);
            m.borrow_mut().insert_canonical(canonical, key, stored);
            None
        }
        _ => None,
    }
}

/// v1–v3 `LegacyArrayLeekValue.transformKey` — collection keys collapse to
/// their size (`longint`); other values pass through.
pub fn legacy_map_key(k: &Value) -> Value {
    match k {
        Value::Array(_)
        | Value::Map(_)
        | Value::Set(_)
        | Value::Instance(_)
        | Value::Interval(_)
        | Value::Function(_)
        | Value::ClassRef(_, _)
        | Value::BuiltinClass(_) => Value::Int(k.to_long()),
        _ => k.clone(),
    }
}

// ---- deep clone (v1 value semantics) ----

/// Recursively copy composite values, for v1 pass-by-value semantics.
pub fn deep_clone(v: &Value) -> Value {
    match v {
        Value::Array(a) => {
            let copied: Vec<Value> = a.borrow().iter().map(deep_clone).collect();
            Value::Array(Rc::new(RefCell::new(copied)))
        }
        Value::Map(m) => {
            let mut out = MapData::new();
            for (k, val) in &m.borrow().entries {
                out.insert_canonical(key_repr(k), deep_clone(k), deep_clone(val));
            }
            Value::Map(Rc::new(RefCell::new(out)))
        }
        Value::Set(s) => {
            let mut out = SetData::new();
            for x in s.borrow().iter() {
                out.insert(deep_clone(x));
            }
            Value::Set(Rc::new(RefCell::new(out)))
        }
        Value::Object(o) => {
            let mut out = ObjectData::new();
            for (k, val) in o.borrow().iter() {
                out.set(k, deep_clone(val));
            }
            Value::Object(Rc::new(RefCell::new(out)))
        }
        Value::Instance(inst) => {
            let src = inst.borrow();
            let mut fields = ObjectData::new();
            for (k, val) in &src.fields {
                fields.set(k, deep_clone(val));
            }
            Value::Instance(Rc::new(RefCell::new(Instance {
                class: src.class,
                class_name: src.class_name.clone(),
                fields,
            })))
        }
        other => other.clone(),
    }
}

// ---- helpers ----

pub fn value_as_concat_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.as_ref().clone(),
        other => other.to_string(),
    }
}

fn numeric_op(
    l: &Value,
    r: &Value,
    int_op: impl FnOnce(i64, i64) -> i64,
    real_op: impl FnOnce(f64, f64) -> f64,
) -> Value {
    // Fast paths for the overwhelmingly common same-typed scalar operands —
    // avoids routing already-unwrapped numbers through the general `to_long` /
    // `to_real` conversion (a sizeable match that doesn't inline, ~4% of a
    // tight arithmetic loop per profiling). Behaviour is identical to the
    // general arms below.
    match (l, r) {
        (Value::Int(a), Value::Int(b)) => return Value::Int(int_op(*a, *b)),
        (Value::Real(a), Value::Real(b)) => return Value::Real(real_op(*a, *b)),
        _ => {}
    }
    if matches!(l, Value::Real(_)) || matches!(r, Value::Real(_)) {
        Value::Real(real_op(l.to_real(), r.to_real()))
    } else {
        Value::Int(int_op(l.to_long(), r.to_long()))
    }
}

fn cmp_bool(l: &Value, r: &Value, pred: impl FnOnce(Ordering) -> bool) -> Value {
    match l.cmp_partial(r) {
        Some(o) => Value::Bool(pred(o)),
        None => Value::Bool(false),
    }
}

/// Version-aware loose equality (`==`).
pub fn eq_for_version(l: &Value, r: &Value, version: u8) -> bool {
    if version >= 4 {
        let same_family = matches!(
            (l, r),
            (Value::Int(_) | Value::Real(_), Value::Int(_) | Value::Real(_))
        ) || std::mem::discriminant(l) == std::mem::discriminant(r);
        if !same_family {
            return false;
        }
        return l.loose_eq(r);
    }
    if version == 1
        && let (Value::Array(a), Value::Null) = (l, r)
    {
        let arr = a.borrow();
        return arr.len() == 1 && eq_legacy(&arr[0], &Value::Null);
    }
    eq_legacy(l, r)
}

fn eq_legacy(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Bool(b), Value::String(s)) | (Value::String(s), Value::Bool(b)) => {
            let is_falsy = s.is_empty() || s.as_str() == "false" || s.as_str() == "0";
            *b != is_falsy
        }
        (Value::Bool(b), Value::Int(i)) | (Value::Int(i), Value::Bool(b)) => *b == (*i != 0),
        (Value::Bool(b), Value::Real(r)) | (Value::Real(r), Value::Bool(b)) => *b == (*r != 0.0),
        (Value::Bool(b), Value::Array(a)) | (Value::Array(a), Value::Bool(b)) => {
            let arr = a.borrow();
            if arr.is_empty() {
                !*b
            } else if arr.len() == 1 || *b {
                eq_legacy(&arr[0], &Value::Bool(*b))
            } else {
                false
            }
        }
        (Value::Int(_) | Value::Real(_), Value::Array(a))
        | (Value::Array(a), Value::Int(_) | Value::Real(_)) => {
            let arr = a.borrow();
            let n = if let Value::Int(i) = l {
                crate::int_to_real(*i)
            } else if let Value::Real(r) = l {
                *r
            } else if let Value::Int(i) = r {
                crate::int_to_real(*i)
            } else if let Value::Real(r) = r {
                *r
            } else {
                return false;
            };
            if arr.is_empty() {
                n == 0.0
            } else if arr.len() == 1 {
                eq_legacy(&arr[0], if matches!(l, Value::Array(_)) { r } else { l })
            } else {
                false
            }
        }
        (Value::String(s), Value::Int(i)) | (Value::Int(i), Value::String(s)) => match s.as_str() {
            "true" => *i != 0,
            "false" | "0" | "" => *i == 0,
            other => other.parse::<i64>().ok() == Some(*i),
        },
        (Value::String(s), Value::Real(r)) | (Value::Real(r), Value::String(s)) => match s.as_str() {
            "true" => *r != 0.0,
            "false" | "0" | "" => *r == 0.0,
            other => other.parse::<f64>().ok().is_some_and(|f| f == *r),
        },
        _ => l.loose_eq(r),
    }
}

pub fn value_instanceof(value: &Value, class: &Value) -> bool {
    match class {
        Value::ClassRef(id, _) => matches!(value, Value::Instance(i) if i.borrow().class == *id),
        Value::BuiltinClass(name) => matches!(
            (*name, value),
            ("Array", Value::Array(_)) | ("Map", Value::Map(_)) | ("Set", Value::Set(_)) |
("Object", Value::Object(_) | Value::Instance(_)) |
("String", Value::String(_)) | ("Boolean", Value::Bool(_)) |
("Null", Value::Null) | ("Function", Value::Function(_)) |
("Interval", Value::Interval(_)) |
("Integer" | "Real" | "Float" | "Number", Value::Int(_)) |
("Real" | "Float" | "Number", Value::Real(_)) |
("Class", Value::ClassRef(_, _) | Value::BuiltinClass(_))
        ),
        _ => false,
    }
}

pub fn contains_value(haystack: &Value, needle: &Value) -> bool {
    match haystack {
        Value::Array(a) => a.borrow().iter().any(|v| v.loose_eq(needle)),
        Value::Set(s) => s.borrow().contains(needle),
        Value::Map(m) => m.borrow().get(needle).is_some(),
        Value::String(s) => {
            if let Value::String(n) = needle {
                s.contains(n.as_str())
            } else {
                false
            }
        }
        Value::Interval(iv) => {
            if iv.is_empty() {
                return false;
            }
            let Some(x) = needle.as_real() else {
                return false;
            };
            let lo_ok = match iv.start {
                None => true,
                Some(s) if iv.start_inclusive => x >= s,
                Some(s) => x > s,
            };
            let hi_ok = match iv.end {
                None => true,
                Some(e) if iv.end_inclusive => x <= e,
                Some(e) => x < e,
            };
            lo_ok && hi_ok
        }
        _ => false,
    }
}

// ---- binary operators (one per op, so callers map their own op enum) ----

pub fn add(l: &Value, r: &Value) -> Value {
    match (l, r) {
        (Value::String(a), b) => Value::String(Rc::new(format!("{a}{}", value_as_concat_string(b)))),
        (a, Value::String(b)) => Value::String(Rc::new(format!("{}{b}", value_as_concat_string(a)))),
        (Value::Array(a), Value::Array(b)) => {
            let mut out = a.borrow().clone();
            out.extend(b.borrow().iter().cloned());
            Value::Array(Rc::new(RefCell::new(out)))
        }
        (Value::Array(a), other) => {
            let mut out = a.borrow().clone();
            out.push(other.clone());
            Value::Array(Rc::new(RefCell::new(out)))
        }
        (Value::Map(a), Value::Map(b)) => {
            let mut out = a.borrow().clone();
            for (k, v) in &b.borrow().entries {
                if out.get(k).is_none() {
                    out.insert(k.clone(), v.clone());
                }
            }
            Value::Map(Rc::new(RefCell::new(out)))
        }
        (Value::Set(a), Value::Set(b)) => {
            let mut out = a.borrow().clone();
            for v in b.borrow().iter() {
                out.insert(v.clone());
            }
            Value::Set(Rc::new(RefCell::new(out)))
        }
        _ => numeric_op(l, r, i64::wrapping_add, |a, b| a + b),
    }
}

pub fn sub(l: &Value, r: &Value) -> Value {
    numeric_op(l, r, i64::wrapping_sub, |a, b| a - b)
}

pub fn mul(l: &Value, r: &Value) -> Value {
    numeric_op(l, r, i64::wrapping_mul, |a, b| a * b)
}

pub fn div(l: &Value, r: &Value, version: u8) -> Value {
    let (a, b) = (l.to_real(), r.to_real());
    if b == 0.0 && version <= 1 {
        Value::Null
    } else {
        Value::Real(a / b)
    }
}

pub fn int_div(l: &Value, r: &Value) -> Value {
    let (a, b) = (l.to_long(), r.to_long());
    if b == 0 {
        Value::Null
    } else {
        Value::Int(a.wrapping_div(b))
    }
}

pub fn rem(l: &Value, r: &Value) -> Value {
    if matches!(l, Value::Real(_)) || matches!(r, Value::Real(_)) {
        let (a, b) = (l.to_real(), r.to_real());
        if b == 0.0 {
            Value::Null
        } else {
            Value::Real(a % b)
        }
    } else {
        let (a, b) = (l.to_long(), r.to_long());
        if b == 0 {
            Value::Null
        } else {
            Value::Int(a.wrapping_rem(b))
        }
    }
}

pub fn pow(l: &Value, r: &Value) -> Value {
    if matches!(l, Value::Real(_)) || matches!(r, Value::Real(_)) {
        Value::Real(l.to_real().powf(r.to_real()))
    } else {
        let a = l.to_long();
        let b = r.to_long();
        if (0..64).contains(&b) {
            Value::Int(a.checked_pow(u32::try_from(b).unwrap_or(u32::MAX)).unwrap_or(i64::MAX))
        } else {
            Value::Real(crate::int_to_real(a).powf(crate::int_to_real(b)))
        }
    }
}

pub fn eq(l: &Value, r: &Value, version: u8) -> Value {
    Value::Bool(eq_for_version(l, r, version))
}

pub fn ne(l: &Value, r: &Value, version: u8) -> Value {
    Value::Bool(!eq_for_version(l, r, version))
}

pub fn identity_eq(l: &Value, r: &Value) -> Value {
    Value::Bool(l.identity_eq(r))
}

pub fn identity_ne(l: &Value, r: &Value) -> Value {
    Value::Bool(!l.identity_eq(r))
}

pub fn lt(l: &Value, r: &Value) -> Value {
    cmp_bool(l, r, |o| matches!(o, Ordering::Less))
}

pub fn le(l: &Value, r: &Value) -> Value {
    cmp_bool(l, r, |o| !matches!(o, Ordering::Greater))
}

pub fn gt(l: &Value, r: &Value) -> Value {
    cmp_bool(l, r, |o| matches!(o, Ordering::Greater))
}

pub fn ge(l: &Value, r: &Value) -> Value {
    cmp_bool(l, r, |o| !matches!(o, Ordering::Less))
}

pub fn bit_and(l: &Value, r: &Value) -> Value {
    Value::Int(l.as_int().unwrap_or(0) & r.as_int().unwrap_or(0))
}

pub fn bit_or(l: &Value, r: &Value) -> Value {
    Value::Int(l.as_int().unwrap_or(0) | r.as_int().unwrap_or(0))
}

pub fn bit_xor(l: &Value, r: &Value) -> Value {
    Value::Int(l.as_int().unwrap_or(0) ^ r.as_int().unwrap_or(0))
}

/// `^=` desugar: POW-assign in v1, XOR-assign in v2+.
pub fn compound_xor(l: &Value, r: &Value, version: u8) -> Value {
    if version <= 1 {
        pow(l, r)
    } else {
        bit_xor(l, r)
    }
}

/// Logical `xor` — boolean xor of truthiness.
pub fn xor(l: &Value, r: &Value) -> Value {
    Value::Bool(l.is_truthy() ^ r.is_truthy())
}

pub fn shl(l: &Value, r: &Value) -> Value {
    Value::Int(l.as_int().unwrap_or(0) << (r.as_int().unwrap_or(0) & 63))
}

pub fn shr(l: &Value, r: &Value) -> Value {
    Value::Int(l.as_int().unwrap_or(0) >> (r.as_int().unwrap_or(0) & 63))
}

// Logical (unsigned) shift-right: the operand is reinterpreted as `u64` for
// the shift, then back to `i64`, so both casts are deliberate bit ops.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub fn ushr(l: &Value, r: &Value) -> Value {
    Value::Int(((l.as_int().unwrap_or(0) as u64) >> (r.as_int().unwrap_or(0) & 63)) as i64)
}

/// `l in r`.
pub fn in_op(l: &Value, r: &Value) -> Value {
    Value::Bool(contains_value(r, l))
}

/// `l not in r`.
pub fn not_in(l: &Value, r: &Value) -> Value {
    Value::Bool(!contains_value(r, l))
}

/// `l is r` — loose equality.
pub fn is(l: &Value, r: &Value) -> Value {
    Value::Bool(l.loose_eq(r))
}

/// `l instanceof r`.
pub fn instanceof(l: &Value, r: &Value) -> Value {
    Value::Bool(value_instanceof(l, r))
}

/// Arithmetic negation (`-x`). Upstream treats `-null` as `0`, `-bool` as
/// `-1`/`0`, and non-numbers as null.
pub fn neg(v: &Value) -> Value {
    match v {
        // `wrapping_neg` so `-Number.MIN_VALUE` (i64::MIN) doesn't overflow —
        // matches how `abs` already uses `wrapping_abs` in this crate.
        Value::Int(i) => Value::Int(i.wrapping_neg()),
        Value::Real(r) => Value::Real(-r),
        Value::Bool(b) => Value::Int(if *b { -1 } else { 0 }),
        Value::Null => Value::Int(0),
        _ => Value::Null,
    }
}

/// Bitwise NOT (`~x`) — operates on the integer view of the value.
pub fn bit_not(v: &Value) -> Value {
    Value::Int(!v.as_int().unwrap_or(0))
}
