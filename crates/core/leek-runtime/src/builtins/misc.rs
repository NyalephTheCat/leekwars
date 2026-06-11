//! Builtin misc dispatch.
//!
//! The Leekscript stdlib has hundreds of free functions; this is a
//! best-effort subset focused on what the upstream test corpus
//! exercises. Anything unrecognised returns `null`, matching
//! upstream's "missing builtin" runtime behavior.

use std::cell::RefCell;
use std::rc::Rc;

use crate::value::Value;
use crate::{BuiltinFlow, BuiltinHost};

use super::array::contains_in;

/// Whether an interval is real-typed: a fully-unbounded `]..[` lives on
/// the real domain, as does any interval carrying a non-integer bound. A
/// half-bounded integer interval (`[1..[`) stays integer-typed.
fn interval_is_real(iv: &crate::value::IntervalValue) -> bool {
    if iv.start.is_none() && iv.end.is_none() {
        return true;
    }
    (iv.start.is_some() && !iv.start_is_int) || (iv.end.is_some() && !iv.end_is_int)
}

/// Insertion-ordered `(field, value)` pairs of an object literal or a
/// class instance. `None` for any other value kind.
fn object_fields(v: &Value) -> Option<Vec<(String, Value)>> {
    match v {
        Value::Object(o) => Some(o.borrow().fields.clone()),
        Value::Instance(i) => Some(i.borrow().fields.fields.clone()),
        _ => None,
    }
}

pub(crate) fn dispatch_misc(
    host: &mut dyn BuiltinHost,
    name: &str,
    args: &[Value],
) -> Result<Option<Value>, BuiltinFlow> {
    Ok(Some(match (name, args.len()) {
        ("println" | "print", _) => Value::Null,
        // `Object.keys()` / `Object.values()` over an object literal
        // (`{a: 5}`) or a class instance (`new A()`). Field order is
        // insertion order for literals, declaration order for classes
        // — both preserved by the underlying `ObjectData`.
        ("keys", 1) => match object_fields(&args[0]) {
            Some(fields) => Value::Array(Rc::new(RefCell::new(
                fields
                    .iter()
                    .map(|(k, _)| Value::String(Rc::new(k.clone())))
                    .collect(),
            ))),
            None => Value::Null,
        },
        ("values", 1) => match object_fields(&args[0]) {
            Some(fields) => Value::Array(Rc::new(RefCell::new(
                fields.iter().map(|(_, v)| v.clone()).collect(),
            ))),
            None => Value::Null,
        },
        ("getInstructionsCount" | "getOperations", 0) => Value::Int(0),
        ("getDate" | "getTime", 0) => Value::Int(0),
        // Leek Wars game-specific colour helper: pack (r,g,b) into
        // a single integer the engine uses for chip colours.
        ("getColor", 3) => match (args[0].as_int(), args[1].as_int(), args[2].as_int()) {
            (Some(r), Some(g), Some(b)) => Value::Int((r << 16) | (g << 8) | b),
            _ => Value::Null,
        },
        ("hash" | "hashCode", 1) => {
            let s = args[0].to_string();
            let mut h: u64 = 5381;
            for c in s.chars() {
                h = h.wrapping_mul(33).wrapping_add(u64::from(c));
            }
            // djb2 hash reinterpreted into the signed `integer` domain.
            #[allow(clippy::cast_possible_wrap)]
            Value::Int(h as i64)
        }
        ("setPut", 2) => match &args[0] {
            Value::Set(s) => Value::Bool(s.borrow_mut().insert(args[1].clone())),
            _ => Value::Null,
        },
        ("setRemove", 2) => match &args[0] {
            Value::Set(s) => Value::Bool(s.borrow_mut().remove(&args[1])),
            _ => Value::Null,
        },
        ("setContains", 2) => Value::Bool(contains_in(&args[0], &args[1])),
        ("setClear", 1) => match &args[0] {
            Value::Set(s) => {
                s.borrow_mut().clear();
                args[0].clone()
            }
            _ => Value::Null,
        },
        ("setIsEmpty", 1) => match &args[0] {
            Value::Set(s) => Value::Bool(s.borrow().is_empty()),
            _ => Value::Null,
        },
        ("setSize", 1) => match &args[0] {
            Value::Set(s) => Value::Int(crate::len_as_int(s.borrow().len())),
            _ => Value::Null,
        },
        ("setToArray", 1) => match &args[0] {
            Value::Set(s) => {
                let items: Vec<Value> = s.borrow().iter().cloned().collect();
                Value::Array(Rc::new(RefCell::new(items)))
            }
            _ => Value::Null,
        },
        ("setIsSubsetOf", 2) => match (&args[0], &args[1]) {
            (Value::Set(a), Value::Set(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                Value::Bool(aa.iter().all(|x| bb.contains(x)))
            }
            _ => return Ok(None),
        },
        ("setIsSupersetOf", 2) => match (&args[0], &args[1]) {
            (Value::Set(a), Value::Set(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                Value::Bool(bb.iter().all(|x| aa.contains(x)))
            }
            _ => return Ok(None),
        },
        ("setUnion", 2) => match (&args[0], &args[1]) {
            (Value::Set(a), Value::Set(b)) => {
                let mut out = a.borrow().clone();
                for v in b.borrow().iter() {
                    out.insert(v.clone());
                }
                Value::Set(Rc::new(RefCell::new(out)))
            }
            _ => return Ok(None),
        },
        ("setIntersection", 2) => match (&args[0], &args[1]) {
            (Value::Set(a), Value::Set(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                let out: crate::value::SetData =
                    aa.iter().filter(|x| bb.contains(x)).cloned().collect();
                Value::Set(Rc::new(RefCell::new(out)))
            }
            _ => return Ok(None),
        },
        ("setDifference", 2) => match (&args[0], &args[1]) {
            (Value::Set(a), Value::Set(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                let out: crate::value::SetData =
                    aa.iter().filter(|x| !bb.contains(x)).cloned().collect();
                Value::Set(Rc::new(RefCell::new(out)))
            }
            _ => return Ok(None),
        },
        ("setDisjunction", 2) => match (&args[0], &args[1]) {
            (Value::Set(a), Value::Set(b)) => {
                let aa = a.borrow();
                let bb = b.borrow();
                let mut out: crate::value::SetData =
                    aa.iter().filter(|x| !bb.contains(x)).cloned().collect();
                for v in bb.iter().filter(|x| !aa.contains(x)) {
                    out.insert(v.clone());
                }
                Value::Set(Rc::new(RefCell::new(out)))
            }
            _ => return Ok(None),
        },
        ("setFilter", 2) => match &args[0] {
            Value::Set(s) => {
                let items: Vec<Value> = s.borrow().iter().cloned().collect();
                let mut out = crate::value::SetData::new();
                for v in items {
                    let r = host.call_value(&args[1], vec![v.clone()])?;
                    if r.is_truthy() {
                        out.insert(v);
                    }
                }
                Value::Set(Rc::new(RefCell::new(out)))
            }
            _ => return Ok(None),
        },
        ("setMap", 2) => match &args[0] {
            Value::Set(s) => {
                let items: Vec<Value> = s.borrow().iter().cloned().collect();
                let mut out = crate::value::SetData::new();
                for v in items {
                    let r = host.call_value(&args[1], vec![v])?;
                    out.insert(r);
                }
                Value::Set(Rc::new(RefCell::new(out)))
            }
            _ => return Ok(None),
        },
        ("setIter" | "setForEach", 2) => match &args[0] {
            Value::Set(s) => {
                let items: Vec<Value> = s.borrow().iter().cloned().collect();
                for v in items {
                    host.call_value(&args[1], vec![v])?;
                }
                args[0].clone()
            }
            _ => return Ok(None),
        },
        ("intervalMin" | "min", 1) => match &args[0] {
            Value::Interval(iv) => match iv.start {
                Some(s) if iv.start_inclusive => {
                    if iv.start_is_int {
                        Value::Int(crate::real_to_int(s))
                    } else {
                        Value::Real(s)
                    }
                }
                Some(s) if iv.start_is_int => Value::Int(crate::real_to_int(s) + 1),
                Some(s) => Value::Real(s),
                None => Value::Real(f64::NEG_INFINITY),
            },
            _ => return Ok(None),
        },
        ("intervalMax" | "max", 1) => match &args[0] {
            Value::Interval(iv) => match iv.end {
                Some(e) if iv.end_inclusive => {
                    if iv.end_is_int {
                        Value::Int(crate::real_to_int(e))
                    } else {
                        Value::Real(e)
                    }
                }
                Some(e) if iv.end_is_int => Value::Int(crate::real_to_int(e) - 1),
                Some(e) => Value::Real(e),
                None => Value::Real(f64::INFINITY),
            },
            _ => return Ok(None),
        },
        ("intervalSize", 1) => match &args[0] {
            Value::Interval(_) => Value::Int(args[0].to_long()),
            _ => return Ok(None),
        },
        ("intervalContains", 2) => Value::Bool(interval_contains(&args[0], &args[1])),
        ("intervalIntersection", 2) => match (&args[0], &args[1]) {
            (Value::Interval(a), Value::Interval(b)) => {
                use crate::value::IntervalValue;
                // Pick the *tighter* bound; the int-ness flag
                // follows the operand that won, not the AND of
                // both (otherwise `[-1..2] ∩ [1..[` would borrow
                // the unbounded side's "real" status and display
                // `[1.0..2]`).
                let (start, start_inclusive, start_is_int, start_forces_real) =
                    match (a.start, b.start) {
                        (Some(x), Some(y)) => {
                            if x > y {
                                (
                                    Some(x),
                                    a.start_inclusive,
                                    a.start_is_int,
                                    a.start_forces_real,
                                )
                            } else if y > x {
                                (
                                    Some(y),
                                    b.start_inclusive,
                                    b.start_is_int,
                                    b.start_forces_real,
                                )
                            } else {
                                (
                                    Some(x),
                                    a.start_inclusive && b.start_inclusive,
                                    a.start_is_int && b.start_is_int,
                                    a.start_forces_real || b.start_forces_real,
                                )
                            }
                        }
                        (Some(x), None) => (
                            Some(x),
                            a.start_inclusive,
                            a.start_is_int,
                            a.start_forces_real,
                        ),
                        (None, Some(y)) => (
                            Some(y),
                            b.start_inclusive,
                            b.start_is_int,
                            b.start_forces_real,
                        ),
                        (None, None) => (None, true, false, false),
                    };
                let (end, end_inclusive, end_is_int, end_forces_real) = match (a.end, b.end) {
                    (Some(x), Some(y)) => {
                        if x < y {
                            (Some(x), a.end_inclusive, a.end_is_int, a.end_forces_real)
                        } else if y < x {
                            (Some(y), b.end_inclusive, b.end_is_int, b.end_forces_real)
                        } else {
                            (
                                Some(x),
                                a.end_inclusive && b.end_inclusive,
                                a.end_is_int && b.end_is_int,
                                a.end_forces_real || b.end_forces_real,
                            )
                        }
                    }
                    (Some(x), None) => (Some(x), a.end_inclusive, a.end_is_int, a.end_forces_real),
                    (None, Some(y)) => (Some(y), b.end_inclusive, b.end_is_int, b.end_forces_real),
                    (None, None) => (None, true, false, false),
                };
                // Result type widens to real if *either* operand is
                // real-typed: a fully-unbounded `]..[` (real domain) or
                // an interval with a non-integer bound. A half-bounded
                // integer interval like `[1..[` stays integer, so
                // `[-1..2] ∩ [1..[` is `[1..2]` while `[1..2] ∩ ]..[`
                // widens to `[1.0..2.0]`.
                let result_real = interval_is_real(a) || interval_is_real(b);
                Value::Interval(Rc::new(IntervalValue {
                    start,
                    end,
                    start_inclusive,
                    end_inclusive,
                    integer_typed: a.integer_typed && b.integer_typed,
                    start_is_int: start_is_int && !result_real,
                    end_is_int: end_is_int && !result_real,
                    start_forces_real,
                    end_forces_real,
                }))
            }
            _ => Value::Null,
        },
        ("intervalCombine", 2) => match (&args[0], &args[1]) {
            (Value::Interval(a), Value::Interval(b)) => {
                use crate::value::IntervalValue;
                // Union extent: an unbounded side always wins
                // (`-∞` for start, `∞` for end), so the result is
                // unbounded whenever EITHER operand is.
                fn min_start(a: Option<f64>, b: Option<f64>) -> Option<f64> {
                    match (a, b) {
                        (Some(x), Some(y)) => Some(x.min(y)),
                        _ => None,
                    }
                }
                fn max_end(a: Option<f64>, b: Option<f64>) -> Option<f64> {
                    match (a, b) {
                        (Some(x), Some(y)) => Some(x.max(y)),
                        _ => None,
                    }
                }
                let start = min_start(a.start, b.start);
                let end = max_end(a.end, b.end);
                Value::Interval(Rc::new(IntervalValue {
                    start,
                    end,
                    // Unbounded sides print as `]-∞` / `∞[` — the
                    // inclusive flag must reflect that, not just
                    // ORing the operands' flags (which would emit
                    // `]…∞]` for a union with an open-end input).
                    start_inclusive: start.is_some() && (a.start_inclusive || b.start_inclusive),
                    end_inclusive: end.is_some() && (a.end_inclusive || b.end_inclusive),
                    integer_typed: a.integer_typed && b.integer_typed,
                    start_is_int: a.start_is_int && b.start_is_int,
                    end_is_int: a.end_is_int && b.end_is_int,
                    start_forces_real: a.start_forces_real || b.start_forces_real,
                    end_forces_real: a.end_forces_real || b.end_forces_real,
                }))
            }
            _ => Value::Null,
        },
        ("intervalIsEmpty", 1) => match &args[0] {
            Value::Interval(iv) => Value::Bool(iv.is_empty()),
            _ => Value::Bool(true),
        },
        ("intervalToArray", 1) => interval_to_array(&args[0], None),
        ("intervalToArray", 2) => interval_to_array(&args[0], Some(&args[1])),
        ("intervalToSet", 1) => interval_to_set(&args[0], None),
        ("intervalToSet", 2) => interval_to_set(&args[0], Some(&args[1])),
        ("intervalAverage", 1) => match &args[0] {
            Value::Interval(iv) => match (iv.start, iv.end) {
                (Some(s), Some(e)) => {
                    // Empty interval (`[1..0]`) → NaN, matching
                    // upstream's "no defined center" rule.
                    if iv.is_empty() {
                        return Ok(Some(Value::Real(f64::NAN)));
                    }
                    // For integer-typed intervals, average the
                    // *contained* integers — openness shifts the
                    // bounds. `]0..5]` contains 1..5 (avg = 3) even
                    // though midpoint of the raw bounds is 2.5.
                    let (lo_eff, hi_eff) = if iv.integer_typed {
                        let lo = if iv.start_inclusive { s } else { s + 1.0 };
                        let hi = if iv.end_inclusive { e } else { e - 1.0 };
                        (lo, hi)
                    } else {
                        (s, e)
                    };
                    let avg = f64::midpoint(lo_eff, hi_eff);
                    if iv.integer_typed && avg.fract() == 0.0 {
                        Value::Int(crate::real_to_int(avg))
                    } else {
                        Value::Real(avg)
                    }
                }
                // Unbounded ends: the average of `]..b]` is `-∞`,
                // of `[a..[` is `+∞`, of `]..[` is `NaN`.
                (None, None) => Value::Real(f64::NAN),
                (None, Some(_)) => Value::Real(f64::NEG_INFINITY),
                (Some(_), None) => Value::Real(f64::INFINITY),
            },
            _ => Value::Null,
        },
        ("intervalIsBounded", 1) => match &args[0] {
            Value::Interval(iv) => Value::Bool(iv.start.is_some() && iv.end.is_some()),
            _ => Value::Bool(false),
        },
        ("intervalIsLeftBounded", 1) => match &args[0] {
            Value::Interval(iv) => Value::Bool(iv.start.is_some()),
            _ => Value::Bool(false),
        },
        ("intervalIsRightBounded", 1) => match &args[0] {
            Value::Interval(iv) => Value::Bool(iv.end.is_some()),
            _ => Value::Bool(false),
        },
        ("intervalIsClosed", 1) => match &args[0] {
            Value::Interval(iv) => Value::Bool(iv.start_inclusive && iv.end_inclusive),
            _ => Value::Bool(false),
        },
        ("intervalIsLeftClosed", 1) => match &args[0] {
            Value::Interval(iv) => Value::Bool(iv.start_inclusive),
            _ => Value::Bool(false),
        },
        ("intervalIsRightClosed", 1) => match &args[0] {
            Value::Interval(iv) => Value::Bool(iv.end_inclusive),
            _ => Value::Bool(false),
        },
        ("jsonEncode", 1) => {
            let mut out = String::new();
            let mut visited = std::collections::HashSet::new();
            json_encode(&mut out, &args[0], &mut visited);
            Value::String(Rc::new(out))
        }
        ("jsonDecode", 1) => match &args[0] {
            Value::String(s) => json_decode(s, host.version()).unwrap_or(Value::Null),
            _ => Value::Null,
        },
        _ => return Ok(None),
    }))
}

/// `intervalToArray(iv, [step])` — enumerate the interval at the
/// given step. Default step is `1`; real-typed intervals can carry
/// a fractional step. Half-open ends drop the corresponding
/// boundary (mirrors `[a..b[` upstream).
pub(crate) fn interval_to_array(iv_val: &Value, step_arg: Option<&Value>) -> Value {
    let Value::Interval(iv) = iv_val else {
        return Value::Null;
    };
    let mut out: Vec<Value> = Vec::new();
    // Unbounded intervals can't be enumerated; upstream returns
    // `null` rather than an empty array.
    let (Some(start), Some(end)) = (iv.start, iv.end) else {
        return Value::Null;
    };
    let step = step_arg
        .and_then(super::super::value::types::Value::as_real)
        .unwrap_or(1.0);
    if step == 0.0 {
        return Value::Array(Rc::new(RefCell::new(out)));
    }
    let descending = step < 0.0;
    let integer_step = step.fract() == 0.0;
    let abs_step = step.abs();
    if descending {
        // Walk from end downward by |step|, ending at start.
        let lo = if iv.start_inclusive {
            start
        } else {
            start + abs_step
        };
        let hi = if iv.end_inclusive {
            end
        } else {
            end - abs_step
        };
        let mut x = hi;
        while x >= lo - 1e-9 {
            let v = if iv.integer_typed && integer_step {
                Value::Int(crate::real_to_int(x))
            } else {
                Value::Real(x)
            };
            out.push(v);
            x -= abs_step;
        }
    } else {
        let lo = if iv.start_inclusive {
            start
        } else {
            start + abs_step
        };
        let hi_check: Box<dyn Fn(f64) -> bool> = if iv.end_inclusive {
            Box::new(move |x| x <= end + 1e-9)
        } else {
            Box::new(move |x| x < end - 1e-9)
        };
        let mut x = lo;
        while hi_check(x) {
            let v = if iv.integer_typed && integer_step {
                Value::Int(crate::real_to_int(x))
            } else {
                Value::Real(x)
            };
            out.push(v);
            x += abs_step;
        }
    }
    Value::Array(Rc::new(RefCell::new(out)))
}

/// `intervalToSet(iv, [step])` — same enumeration as
/// `intervalToArray` but de-duplicates into a `Set`.
pub(crate) fn interval_to_set(iv_val: &Value, step_arg: Option<&Value>) -> Value {
    let arr = interval_to_array(iv_val, step_arg);
    let Value::Array(a) = arr else {
        return Value::Null;
    };
    let mut s = crate::value::SetData::new();
    for v in a.borrow().iter() {
        s.insert(v.clone());
    }
    Value::Set(Rc::new(RefCell::new(s)))
}

pub(crate) fn interval_contains(haystack: &Value, needle: &Value) -> bool {
    let Value::Interval(iv) = haystack else {
        return false;
    };
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

pub(crate) fn json_encode(
    out: &mut String,
    v: &Value,
    visited: &mut std::collections::HashSet<usize>,
) {
    // Cycle protection — emit "null" the second time we hit the
    // same Rc-addressed composite.
    let id: Option<usize> = match v {
        Value::Array(a) => Some(Rc::as_ptr(a) as usize),
        Value::Map(m) => Some(Rc::as_ptr(m) as usize),
        Value::Set(s) => Some(Rc::as_ptr(s) as usize),
        Value::Object(o) => Some(Rc::as_ptr(o) as usize),
        Value::Instance(i) => Some(Rc::as_ptr(i) as usize),
        _ => None,
    };
    if let Some(id) = id
        && !visited.insert(id)
    {
        out.push_str("null");
        return;
    }
    json_encode_inner(out, v, visited);
    if let Some(id) = id {
        visited.remove(&id);
    }
}

pub(crate) fn json_encode_inner(
    out: &mut String,
    v: &Value,
    visited: &mut std::collections::HashSet<usize>,
) {
    use std::fmt::Write;
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        // jsonEncode bypasses the display crop — full digits.
        Value::BigInt(b) => {
            let _ = write!(out, "{}", crate::value::big_full_decimal(b));
        }
        Value::Real(r) => {
            if r.is_nan() || r.is_infinite() {
                out.push_str("null");
            } else if r.fract() == 0.0 && r.abs() < 1e15 {
                let _ = write!(out, "{}", crate::real_to_int(*r));
            } else {
                let _ = write!(out, "{r}");
            }
        }
        Value::String(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\t' => out.push_str("\\t"),
                    '\r' => out.push_str("\\r"),
                    c if (c as u32) < 0x20 => {
                        let _ = write!(out, "\\u{:04x}", c as u32);
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::Array(a) => {
            out.push('[');
            let arr = a.borrow();
            for (i, x) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_encode(out, x, visited);
            }
            out.push(']');
        }
        Value::Map(m) => {
            // jsonEncode sorts string keys alphabetically; numeric
            // keys go through their string representation (so 50
            // sorts as "50" between "5" and "6"). Matches Java's
            // `JSONClass.encode` (uses a sorted iteration).
            out.push('{');
            let mp = m.borrow();
            let mut entries: Vec<(String, &Value)> = mp
                .entries
                .iter()
                .map(|(k, v)| {
                    let key_str = match k {
                        Value::String(s) => s.as_ref().clone(),
                        other => other.to_string(),
                    };
                    (key_str, v)
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (i, (key_str, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                for c in key_str.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        c => out.push(c),
                    }
                }
                out.push_str("\":");
                json_encode(out, v, visited);
            }
            out.push('}');
        }
        Value::Set(s) => {
            out.push('[');
            let st = s.borrow();
            for (i, x) in st.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_encode(out, x, visited);
            }
            out.push(']');
        }
        Value::Object(o) => {
            // Same sorted-key convention as Map. Java's encoder
            // wraps `Object` in the same JSON-object machinery.
            out.push('{');
            let ob = o.borrow();
            let mut entries: Vec<(&String, &Value)> = ob.iter().map(|(k, v)| (k, v)).collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                for c in k.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        c => out.push(c),
                    }
                }
                out.push_str("\":");
                json_encode(out, v, visited);
            }
            out.push('}');
        }
        Value::Instance(i) => {
            out.push('{');
            for (n, (k, v)) in i.borrow().fields.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                let _ = write!(out, "\"{k}\":");
                json_encode(out, v, visited);
            }
            out.push('}');
        }
        _ => out.push_str("null"),
    }
}

/// `join`'s element conversion: strings drop quotes, everything
/// else uses the standard `Display`. Mirrors Java
/// `String.valueOf(elem)` over the Object array.
pub(crate) fn value_as_concat_string_for_join(v: &Value) -> String {
    match v {
        Value::String(s) => s.as_ref().clone(),
        other => other.to_string(),
    }
}

pub(crate) fn json_decode(s: &str, version: u8) -> Option<Value> {
    let mut chars = s.chars().peekable();
    json_parse_depth(&mut chars, version, 0)
}

/// Mirrors upstream's parser cap: arrays / objects nested deeper
/// than this return `null` from `jsonDecode`. Without a limit we'd
/// blow the stack on adversarial inputs.
const JSON_MAX_DEPTH: usize = 200;

pub(crate) fn json_parse_depth(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    version: u8,
    depth: usize,
) -> Option<Value> {
    if depth > JSON_MAX_DEPTH {
        return None;
    }
    json_parse(chars, version, depth)
}

pub(crate) fn json_parse(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    version: u8,
    depth: usize,
) -> Option<Value> {
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
    match chars.peek()? {
        '"' => {
            chars.next();
            let mut out = String::new();
            while let Some(c) = chars.next() {
                if c == '"' {
                    return Some(Value::String(Rc::new(out)));
                }
                if c == '\\' {
                    let esc = chars.next()?;
                    out.push(match esc {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '"' => '"',
                        '\\' => '\\',
                        '/' => '/',
                        other => other,
                    });
                } else {
                    out.push(c);
                }
            }
            None
        }
        '[' => {
            chars.next();
            let mut arr = Vec::new();
            loop {
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&']') {
                    chars.next();
                    return Some(Value::Array(Rc::new(RefCell::new(arr))));
                }
                arr.push(json_parse_depth(chars, version, depth + 1)?);
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&',') {
                    chars.next();
                }
            }
        }
        '{' => {
            chars.next();
            // Decode JSON `{...}`. v1–v3 returned a `Map`; v4
            // switched to `Object` to match the source-literal
            // shape (and Java's `LeekValueManager.parseJSON` for
            // v4 builds `ObjectLeekValue`).
            if version >= 4 {
                let mut obj = crate::value::ObjectData::new();
                loop {
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if chars.peek() == Some(&'}') {
                        chars.next();
                        return Some(Value::Object(Rc::new(RefCell::new(obj))));
                    }
                    let key = json_parse_depth(chars, version, depth + 1)?;
                    let key_s = match key {
                        Value::String(s) => s.as_ref().clone(),
                        other => other.to_string(),
                    };
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() || c == ':' {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    let val = json_parse_depth(chars, version, depth + 1)?;
                    obj.set(&key_s, val);
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if chars.peek() == Some(&',') {
                        chars.next();
                    }
                }
            } else {
                // v1-v3 collected JSON object entries into a
                // sorted Map (Java's `TreeMap` semantics for
                // `LegacyMapLeekValue`). Sort by key string after
                // parsing so subsequent iteration matches.
                let mut pairs: Vec<(Value, Value)> = Vec::new();
                loop {
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if chars.peek() == Some(&'}') {
                        chars.next();
                        let mut map = crate::value::MapData::new();
                        pairs.sort_by(|(a, _), (b, _)| {
                            crate::value::key_repr(a).cmp(&crate::value::key_repr(b))
                        });
                        for (k, v) in pairs {
                            map.insert(k, v);
                        }
                        return Some(Value::Map(Rc::new(RefCell::new(map))));
                    }
                    let key = json_parse_depth(chars, version, depth + 1)?;
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() || c == ':' {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    let val = json_parse_depth(chars, version, depth + 1)?;
                    pairs.push((key, val));
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if chars.peek() == Some(&',') {
                        chars.next();
                    }
                }
            }
        }
        c if c.is_ascii_digit() || *c == '-' => {
            let mut num = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() || c == '-' || c == '.' || c == 'e' || c == 'E' || c == '+' {
                    num.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if num.contains('.') || num.contains('e') || num.contains('E') {
                num.parse::<f64>().ok().map(Value::Real)
            } else {
                num.parse::<i64>().ok().map(Value::Int)
            }
        }
        't' => {
            for c in "true".chars() {
                if chars.next() != Some(c) {
                    return None;
                }
            }
            Some(Value::Bool(true))
        }
        'f' => {
            for c in "false".chars() {
                if chars.next() != Some(c) {
                    return None;
                }
            }
            Some(Value::Bool(false))
        }
        'n' => {
            for c in "null".chars() {
                if chars.next() != Some(c) {
                    return None;
                }
            }
            Some(Value::Null)
        }
        _ => None,
    }
}
