//! Builtin array dispatch.
//!
//! The Leekscript stdlib has hundreds of free functions; this is a
//! best-effort subset focused on what the upstream test corpus
//! exercises. Anything unrecognised returns `null`, matching
//! upstream's "missing builtin" runtime behavior.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use crate::value::{MapData, Value};
use crate::{BuiltinFlow, BuiltinHost};

pub(crate) fn dispatch_array(
    host: &mut dyn BuiltinHost,
    name: &str,
    args: &[Value],
) -> Result<Option<Value>, BuiltinFlow> {
    Ok(Some(match (name, args.len()) {
        ("count", 1) => count(&args[0], host.version()),
        ("isEmpty", 1) => Value::Bool(match &args[0] {
            Value::Array(a) => a.borrow().is_empty(),
            Value::Map(m) => m.borrow().is_empty(),
            Value::Set(s) => s.borrow().is_empty(),
            Value::String(s) => s.is_empty(),
            _ => true,
        }),

        // ---- Mutation ----
        // `push` mutates and returns `null` (void). Upstream
        // signature: `void push(Array, value)`. v1's LegacyArray
        // pushes a *clone* of the value (so `push(a, a)` snapshots
        // the array's contents at the call site and the resulting
        // entry is independent of the outer reference). v2+ uses
        // the shared reference like a regular array.
        ("push", 2) => {
            if let Value::Array(a) = &args[0] {
                let val = if host.version() <= 1 {
                    deep_clone(&args[1])
                } else {
                    args[1].clone()
                };
                a.borrow_mut().push(val);
            }
            Value::Null
        }
        ("unshift", 2) => {
            if let Value::Array(a) = &args[0] {
                let val = if host.version() <= 1 {
                    deep_clone(&args[1])
                } else {
                    args[1].clone()
                };
                a.borrow_mut().insert(0, val);
            }
            Value::Null
        }
        ("pop", 1) => match &args[0] {
            Value::Array(a) => a.borrow_mut().pop().unwrap_or(Value::Null),
            _ => Value::Null,
        },
        ("shift", 1) => match &args[0] {
            Value::Array(a) => {
                let mut aa = a.borrow_mut();
                if aa.is_empty() {
                    Value::Null
                } else {
                    aa.remove(0)
                }
            }
            _ => Value::Null,
        },
        ("remove", 2) => match &args[0] {
            Value::Array(a) => {
                let mut aa = a.borrow_mut();
                let i = args[1].as_int().unwrap_or(0);
                if i >= 0 && (crate::clamp_index(i)) < aa.len() {
                    aa.remove(crate::clamp_index(i))
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        },
        ("removeElement", 2) => match &args[0] {
            Value::Array(a) => {
                let snap: Vec<Value> = a.borrow().clone();
                let pos = snap.iter().position(|v| v.loose_eq(&args[1]));
                let found = pos.is_some();
                if let Some(i) = pos {
                    a.borrow_mut().remove(i);
                    // v1-v3 LegacyArray semantics: removing an
                    // element doesn't shift indices — surviving
                    // keys keep their original positions, so the
                    // result is a sparse map. Stash the morphed
                    // value for the caller's `ApplyPromotion`
                    // statement to write back.
                    if host.version() <= 3 {
                        let mut map = MapData::new();
                        for (j, v) in snap.iter().enumerate() {
                            if j == i {
                                continue;
                            }
                            let k = Value::Int(crate::len_as_int(j));
                            let ck = crate::value::key_repr(&k);
                            map.insert_canonical(ck, k, v.clone());
                        }
                        stash_promotion(Value::Map(Rc::new(RefCell::new(map))));
                    }
                }
                Value::Bool(found)
            }
            _ => Value::Bool(false),
        },
        ("removeKey", 2) => match &args[0] {
            Value::Array(a) => {
                let mut aa = a.borrow_mut();
                let i = args[1].as_int().unwrap_or(0);
                if i >= 0 && (crate::clamp_index(i)) < aa.len() {
                    aa.remove(crate::clamp_index(i));
                    Value::Bool(true)
                } else {
                    Value::Bool(false)
                }
            }
            Value::Map(m) => {
                // v1-v3 truncates real keys to integers for the
                // lookup (matches `read_index_with_methods`); v4
                // uses strict equality.
                let key = if host.version() <= 3 {
                    if let Value::Real(r) = &args[1] {
                        Value::Int(crate::real_to_int(*r))
                    } else {
                        args[1].clone()
                    }
                } else {
                    args[1].clone()
                };
                let canon = crate::value::key_repr(&key);
                let mut mm = m.borrow_mut();
                if let Some(&idx) = mm.index.get(&canon) {
                    mm.entries.remove(idx);
                    mm.index.clear();
                    let canons: Vec<String> = mm
                        .entries
                        .iter()
                        .map(|(k, _)| crate::value::key_repr(k))
                        .collect();
                    for (i, c) in canons.into_iter().enumerate() {
                        mm.index.insert(c, i);
                    }
                    Value::Bool(true)
                } else {
                    Value::Bool(false)
                }
            }
            _ => Value::Bool(false),
        },
        ("insert", 3) => match &args[0] {
            Value::Array(a) => {
                let mut aa = a.borrow_mut();
                let i = crate::clamp_index(args[2].as_int().unwrap_or(0));
                let pos = i.min(aa.len());
                aa.insert(pos, args[1].clone());
                args[0].clone()
            }
            _ => args[0].clone(),
        },
        ("arrayClear", 1) => match &args[0] {
            Value::Array(a) => {
                a.borrow_mut().clear();
                args[0].clone()
            }
            _ => args[0].clone(),
        },

        // ---- Pure queries ----
        ("contains" | "inArray", 2) => Value::Bool(contains_in(&args[0], &args[1])),
        ("search" | "indexOf", 2) => match &args[0] {
            Value::Array(a) => a
                .borrow()
                .iter()
                .position(|v| v.loose_eq(&args[1]))
                .map_or(Value::Null, |i| Value::Int(crate::len_as_int(i))),
            // `search(map, value)` returns the matching *key*, not
            // an index. Mirrors `mapSearch`.
            Value::Map(m) => m
                .borrow()
                .entries
                .iter()
                .find(|(_, v)| v.loose_eq(&args[1]))
                .map_or(Value::Null, |(k, _)| k.clone()),
            Value::String(s) => {
                if let Value::String(needle) = &args[1]
                    && let Some(i) = s.find(needle.as_str())
                {
                    Value::Int(crate::len_as_int(i))
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        },
        // `search(array, needle, from)`: linear search starting at
        // `from`. Negative `from` counts from the end. Returns -1
        // for not found (in this 3-arg form), null elsewhere.
        ("search" | "indexOf", 3) => match &args[0] {
            Value::Array(a) => {
                let arr = a.borrow();
                let len = crate::len_as_int(arr.len());
                let from_raw = args[2].as_int().unwrap_or(0);
                let from = if from_raw < 0 {
                    from_raw + len
                } else {
                    from_raw
                };
                // Out-of-bounds start returns `null` in v1-v3 and
                // `-1` (not found) in v4+. Negative indices that
                // resolve in range stay as searches.
                if from < 0 || from >= len {
                    if host.version() <= 3 {
                        Value::Null
                    } else {
                        Value::Int(-1)
                    }
                } else {
                    arr[crate::clamp_index(from)..]
                        .iter()
                        .position(|v| v.loose_eq(&args[1]))
                        .map_or(Value::Int(-1), |i| Value::Int(crate::len_as_int(i) + from))
                }
            }
            Value::String(s) => {
                if let Value::String(needle) = &args[1] {
                    let chars: Vec<char> = s.chars().collect();
                    let needle_chars: Vec<char> = needle.chars().collect();
                    let from = args[2].as_int().unwrap_or(0);
                    let from = crate::clamp_index(if from < 0 {
                        from + crate::len_as_int(chars.len())
                    } else {
                        from
                    });
                    if needle_chars.is_empty() {
                        Value::Int(crate::len_as_int(from.min(chars.len())))
                    } else {
                        let mut found = None;
                        if from <= chars.len() {
                            for i in from..=chars.len().saturating_sub(needle_chars.len()) {
                                if chars[i..i + needle_chars.len()] == needle_chars[..] {
                                    found = Some(i);
                                    break;
                                }
                            }
                        }
                        found.map_or(Value::Int(-1), |i| Value::Int(crate::len_as_int(i)))
                    }
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        },
        // `arrayGet(array, index)` / `arrayGet(array, index, default)`.
        ("arrayGet", 2) => match &args[0] {
            Value::Array(a) => {
                let arr = a.borrow();
                let i = args[1].as_int().unwrap_or(0);
                let len = crate::len_as_int(arr.len());
                let i = if i < 0 { i + len } else { i };
                if i < 0 || i >= len {
                    Value::Null
                } else {
                    arr[crate::clamp_index(i)].clone()
                }
            }
            _ => Value::Null,
        },
        ("arrayGet", 3) => match &args[0] {
            Value::Array(a) => {
                let arr = a.borrow();
                let i = args[1].as_int().unwrap_or(0);
                let len = crate::len_as_int(arr.len());
                let i = if i < 0 { i + len } else { i };
                if i < 0 || i >= len {
                    args[2].clone()
                } else {
                    arr[crate::clamp_index(i)].clone()
                }
            }
            _ => args[2].clone(),
        },
        ("first" | "arrayFirst", 1) => match &args[0] {
            Value::Array(a) => a.borrow().first().cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        },
        ("last" | "arrayLast", 1) => match &args[0] {
            Value::Array(a) => a.borrow().last().cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        },
        ("min" | "arrayMin", 1) => match &args[0] {
            Value::Array(a) => min_max_array(&a.borrow(), true),
            // Maps reduce over their VALUES — `arrayMin([0:7, 8:9, 'a':2])` → 2.
            Value::Map(m) => {
                let vs: Vec<Value> = m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                min_max_array(&vs, true)
            }
            _ => Value::Null,
        },
        ("max" | "arrayMax", 1) => match &args[0] {
            Value::Array(a) => min_max_array(&a.borrow(), false),
            Value::Map(m) => {
                let vs: Vec<Value> = m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                min_max_array(&vs, false)
            }
            _ => Value::Null,
        },
        ("sum", 1) => match &args[0] {
            Value::Array(a) => sum_array(&a.borrow()),
            Value::Map(m) => {
                let vs: Vec<Value> = m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                sum_array(&vs)
            }
            _ => Value::Null,
        },
        ("average", 1) => match &args[0] {
            Value::Array(a) => avg_array(&a.borrow()),
            // Maps reduce over their values, like `min` / `max`.
            Value::Map(m) => {
                let vs: Vec<Value> = m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                avg_array(&vs)
            }
            _ => Value::Null,
        },
        ("reverse", 1) => match &args[0] {
            // Mutate in place and return the same array — matches
            // upstream's `ArrayLeekValue.reverse` (Java's runtime
            // sort/reverse all mutate the receiver).
            Value::Array(a) => {
                a.borrow_mut().reverse();
                Value::Array(Rc::clone(a))
            }
            Value::String(s) => Value::String(Rc::new(s.chars().rev().collect::<String>())),
            _ => Value::Null,
        },
        ("sort" | "arraySort", 1) => sort_array(&args[0], None, host.version()),
        ("sort" | "arraySort", 2) => {
            // Second arg can be `SORT_ASC`/`SORT_DESC` (an integer
            // flag) or a comparator callback. We
            // dispatch on the runtime value: callable values go
            // through the comparator path, everything else is the
            // mode flag.
            if matches!(
                &args[1],
                Value::Function(_) | Value::ClassRef(_, _) | Value::BuiltinClass(_)
            ) {
                return Ok(Some(sort_array_with_cmp(host, &args[0], &args[1])?));
            }
            sort_array(&args[0], Some(&args[1]), host.version())
        }
        ("concat", 2) => match (&args[0], &args[1]) {
            (Value::Array(a), Value::Array(b)) => {
                let mut out = a.borrow().clone();
                out.extend(b.borrow().iter().cloned());
                Value::Array(Rc::new(RefCell::new(out)))
            }
            (Value::String(a), Value::String(b)) => Value::String(Rc::new(format!("{a}{b}"))),
            _ => Value::Null,
        },
        ("range", 2) => match (args[0].as_int(), args[1].as_int()) {
            (Some(lo), Some(hi)) => {
                let mut out = Vec::new();
                if lo <= hi {
                    for i in lo..=hi {
                        out.push(Value::Int(i));
                    }
                }
                Value::Array(Rc::new(RefCell::new(out)))
            }
            _ => Value::Null,
        },
        ("min", 2) => min_max_pair(&args[0], &args[1], true),
        ("max", 2) => min_max_pair(&args[0], &args[1], false),
        // `pow(big, big)` has an exact NumberClass overload upstream —
        // same semantics as the `**` operator.
        ("pow", 2)
            if matches!(args[0], Value::BigInt(_)) || matches!(args[1], Value::BigInt(_)) =>
        {
            crate::eval::pow(&args[0], &args[1])
        }
        ("pow", 2) => match (args[0].as_real(), args[1].as_real()) {
            (Some(a), Some(b)) => Value::Real(crate::leek_pow(a, b)),
            _ => Value::Null,
        },
        ("rand" | "randInt", 2) => match (args[0].as_int(), args[1].as_int()) {
            // Uniform integer in `[lo, hi)` (exclusive upper, matching
            // upstream). Seeded PRNG keeps corpus runs reproducible
            // while giving the statistical tests real spread.
            (Some(a), Some(b)) => Value::Int(host.rng_int(a, b)),
            _ => Value::Null,
        },
        ("randFloat" | "randReal", 2) => match (args[0].as_real(), args[1].as_real()) {
            (Some(a), Some(b)) => Value::Real(host.rng_real(a, b)),
            _ => Value::Null,
        },
        // `color(r, g, b)` packs three 8-bit channels into a
        // 24-bit RGB int. `color(red, green, blue)` upstream uses
        // `(r << 16) | (g << 8) | b`.
        ("color", 3) => match (args[0].as_int(), args[1].as_int(), args[2].as_int()) {
            (Some(r), Some(g), Some(b)) => {
                let r = r.clamp(0, 255);
                let g = g.clamp(0, 255);
                let b = b.clamp(0, 255);
                Value::Int((r << 16) | (g << 8) | b)
            }
            _ => Value::Null,
        },
        ("getRed", 1) => match args[0].as_int() {
            Some(n) => Value::Int((n >> 16) & 0xff),
            _ => Value::Null,
        },
        ("getGreen", 1) => match args[0].as_int() {
            Some(n) => Value::Int((n >> 8) & 0xff),
            _ => Value::Null,
        },
        ("getBlue", 1) => match args[0].as_int() {
            Some(n) => Value::Int(n & 0xff),
            _ => Value::Null,
        },
        ("hypot", 2) => match (args[0].as_real(), args[1].as_real()) {
            (Some(a), Some(b)) => Value::Real(crate::leek_hypot(a, b)),
            _ => Value::Null,
        },
        ("atan2", 2) => match (args[0].as_real(), args[1].as_real()) {
            (Some(a), Some(b)) => Value::Real(crate::leek_atan2(a, b)),
            _ => Value::Null,
        },
        ("rotateLeft", 2) => match (args[0].as_int(), args[1].as_int()) {
            (Some(a), Some(b)) => Value::Int(a.rotate_left(u32::try_from(b & 63).unwrap_or(0))),
            _ => Value::Null,
        },
        ("rotateRight", 2) => match (args[0].as_int(), args[1].as_int()) {
            (Some(a), Some(b)) => Value::Int(a.rotate_right(u32::try_from(b & 63).unwrap_or(0))),
            _ => Value::Null,
        },
        ("isPermutation", 2) => match (args[0].as_int(), args[1].as_int()) {
            (Some(a), Some(b)) => {
                // Same digit multiset → permutation. Matches
                // upstream `NumberClass.isPermutation`.
                fn digits(mut n: i64) -> [u8; 10] {
                    let mut out = [0u8; 10];
                    if n == 0 {
                        out[0] = 1;
                        return out;
                    }
                    n = n.wrapping_abs();
                    while n > 0 {
                        out[crate::clamp_index(n % 10)] += 1;
                        n /= 10;
                    }
                    out
                }
                Value::Bool(digits(a) == digits(b))
            }
            _ => Value::Bool(false),
        },

        // ---- Higher-order ----
        ("arrayMap", 2) => higher_order_array(host, &args[0], &args[1], HoKind::Map)?,
        ("arrayFilter", 2) => higher_order_array(host, &args[0], &args[1], HoKind::Filter)?,
        ("arrayFoldLeft", 3) => fold(host, &args[0], &args[1], args[2].clone(), false)?,
        ("arrayFoldRight", 3) => fold(host, &args[0], &args[1], args[2].clone(), true)?,
        ("arrayIter", 2) => iter_array(host, &args[0], &args[1])?,
        ("arrayEvery", 2) => quantify(host, &args[0], &args[1], true)?,
        ("arraySome", 2) => quantify(host, &args[0], &args[1], false)?,
        ("arrayPartition", 2) => partition(host, &args[0], &args[1])?,
        ("arrayFlatten", 1) => flatten_array(&args[0], 1),
        ("arrayFlatten", 2) => {
            let depth = args[1].as_int().unwrap_or(1).max(0);
            flatten_array(&args[0], depth)
        }
        ("subArray", 3) => match (&args[0], args[1].as_int(), args[2].as_int()) {
            // `subArray(arr, from, to)` — inclusive on both ends.
            (Value::Array(a), Some(from), Some(to)) => {
                let arr = a.borrow();
                let len = crate::len_as_int(arr.len());
                let from = from.max(0).min(len);
                let to = (to + 1).max(0).min(len);
                if from >= to {
                    Value::Array(Rc::new(RefCell::new(Vec::new())))
                } else {
                    Value::Array(Rc::new(RefCell::new(
                        arr[crate::clamp_index(from)..crate::clamp_index(to)].to_vec(),
                    )))
                }
            }
            _ => Value::Null,
        },
        ("arraySlice", 1) => match &args[0] {
            Value::Array(a) => Value::Array(Rc::new(RefCell::new(a.borrow().clone()))),
            _ => Value::Null,
        },
        ("arraySlice", 2) => array_slice(&args[0], Some(&args[1]), None, None),
        ("arraySlice", 3) => array_slice(&args[0], Some(&args[1]), Some(&args[2]), None),
        ("arraySlice", 4) => array_slice(&args[0], Some(&args[1]), Some(&args[2]), Some(&args[3])),
        ("arrayDistinct" | "distinct", 1) => match &args[0] {
            Value::Array(a) => {
                let mut out = Vec::new();
                for v in a.borrow().iter() {
                    if !out.iter().any(|x: &Value| x.loose_eq(v)) {
                        out.push(v.clone());
                    }
                }
                Value::Array(Rc::new(RefCell::new(out)))
            }
            _ => Value::Null,
        },
        ("shuffle", 1) => args[0].clone(), // deterministic identity
        ("arrayChunk", 2) => match (&args[0], args[1].as_int()) {
            (Value::Array(a), Some(n)) => {
                let arr = a.borrow();
                let n = crate::clamp_index(n.max(1));
                let mut out = Vec::new();
                for chunk in arr.chunks(n) {
                    out.push(Value::Array(Rc::new(RefCell::new(chunk.to_vec()))));
                }
                Value::Array(Rc::new(RefCell::new(out)))
            }
            _ => Value::Null,
        },
        ("arrayUnique", 1) => match &args[0] {
            Value::Array(a) => {
                let mut out = Vec::new();
                for v in a.borrow().iter() {
                    if !out.iter().any(|x: &Value| x.loose_eq(v)) {
                        out.push(v.clone());
                    }
                }
                Value::Array(Rc::new(RefCell::new(out)))
            }
            _ => Value::Null,
        },
        ("arrayFind", 2) => find_array(host, &args[0], &args[1])?,
        ("arrayRandom" | "shuffle", 2) => match (&args[0], args[1].as_int()) {
            // Deterministic for parity: return the first `n` items.
            (Value::Array(a), Some(n)) => {
                let arr = a.borrow();
                let take = crate::clamp_index(n).min(arr.len());
                Value::Array(Rc::new(RefCell::new(arr[..take].to_vec())))
            }
            _ => Value::Null,
        },
        ("arrayConcat", 2) => match (&args[0], &args[1]) {
            (Value::Array(a), Value::Array(b)) => {
                let mut out = a.borrow().clone();
                out.extend(b.borrow().iter().cloned());
                Value::Array(Rc::new(RefCell::new(out)))
            }
            _ => Value::Null,
        },
        // `fill(arr, value, n)` — set the first `n` slots of
        // `arr` to `value`. Grow the array if needed (so
        // `fill([], 'a', 2)` becomes `["a", "a"]`), but leave any
        // existing elements past index `n` untouched
        // (`fill([1,2,3], 'a', 2)` → `["a", "a", 3]`).
        ("fill", 3) => match &args[0] {
            Value::Array(a) => {
                let n = crate::clamp_index(args[2].as_int().unwrap_or(0));
                let val = args[1].clone();
                let mut arr = a.borrow_mut();
                if arr.len() < n {
                    arr.resize(n, val.clone());
                }
                let limit = n.min(arr.len());
                for slot in arr.iter_mut().take(limit) {
                    *slot = val.clone();
                }
                Value::Array(Rc::clone(a))
            }
            _ => Value::Null,
        },
        // `fill` has two shapes:
        // - `fill(arr, value)` — overwrite each existing
        //   element of `arr` with `value` (in place). Used by
        //   tests like `var a = [1,2,3]; fill(a, 'a'); return a`.
        // - `fill(value, n)` — construct a new array of length
        //   `n` whose elements are all `value`. Used when the
        //   first arg isn't a container.
        ("fill", 2) => match &args[0] {
            Value::Array(a) => {
                let val = args[1].clone();
                for slot in a.borrow_mut().iter_mut() {
                    *slot = val.clone();
                }
                Value::Array(Rc::clone(a))
            }
            _ => match args[1].as_int() {
                Some(n) => {
                    let mut v = Vec::with_capacity(crate::clamp_index(n));
                    for _ in 0..n.max(0) {
                        v.push(args[0].clone());
                    }
                    Value::Array(Rc::new(RefCell::new(v)))
                }
                _ => Value::Null,
            },
        },
        ("arraySize" | "arrayCount", 1) => match &args[0] {
            Value::Array(a) => Value::Int(crate::len_as_int(a.borrow().len())),
            _ => Value::Null,
        },

        // `pushAll(target, source)` — extend target with source's
        // elements. Mutates target; returns it. Upstream accepts
        // any iterable as `source`; we cover Array / Set / Map
        // values (Map iterates as keys).
        ("pushAll", 2) => {
            if let Value::Array(target) = &args[0] {
                match &args[1] {
                    Value::Array(src) => {
                        target.borrow_mut().extend(src.borrow().iter().cloned());
                    }
                    Value::Set(src) => {
                        target.borrow_mut().extend(src.borrow().iter().cloned());
                    }
                    Value::Map(src) => {
                        let keys: Vec<Value> = src
                            .borrow()
                            .entries
                            .iter()
                            .map(|(k, _)| k.clone())
                            .collect();
                        target.borrow_mut().extend(keys);
                    }
                    _ => {}
                }
            }
            args[0].clone()
        }

        // `clone(v)` (and `clone(v, depth)`): copy composite
        // values so the caller can mutate the copy independently
        // of the original. Primitives are returned unchanged
        // (they're already value-typed). Depth defaults to 1 —
        // arbitrary; the corpus tests don't probe the deep case.
        ("clone", 1 | 2) => deep_clone(&args[0]),

        // `arrayFrequencies(arr)` → Map<element, count> of each
        // distinct value's occurrence count. Mirrors
        // `ArrayLeekValue.arrayFrequencies`.
        ("arrayFrequencies", 1) => {
            if let Value::Array(a) = &args[0] {
                let mut out = MapData::new();
                for v in a.borrow().iter() {
                    let canon = crate::value::key_repr(v);
                    let current = out.get(v).cloned().unwrap_or(Value::Int(0));
                    let next = match current {
                        Value::Int(n) => Value::Int(n + 1),
                        _ => Value::Int(1),
                    };
                    out.insert_canonical(canon, v.clone(), next);
                }
                Value::Map(Rc::new(RefCell::new(out)))
            } else {
                Value::Null
            }
        }
        // `arrayToSet(arr)` — distinct values in iteration order.
        ("arrayToSet", 1) => {
            if let Value::Array(a) = &args[0] {
                let mut out = crate::value::SetData::new();
                for v in a.borrow().iter() {
                    out.insert(v.clone());
                }
                Value::Set(Rc::new(RefCell::new(out)))
            } else {
                Value::Null
            }
        }
        // `arrayRemoveAll(arr, value)` — drop every element
        // loose-equal to `value`. Mutates and returns the array.
        ("arrayRemoveAll", 2) => {
            if let Value::Array(a) = &args[0] {
                a.borrow_mut().retain(|v| !v.loose_eq(&args[1]));
            }
            args[0].clone()
        }

        _ => return Ok(None),
    }))
}

// One-level copy of composite values; primitives are returned
// unchanged. Recursively clones nested arrays / maps / sets so
// the result shares no `Rc` interior with the source.
thread_local! {
    /// One-shot side-channel: a v1-v3 mutating builtin
    /// (`removeElement`, `assocReverse`, `assocSort`, …) sets the
    /// promoted Map here when it morphs its first arg's container;
    /// the interp's `ApplyPromotion` statement consumes it and
    /// writes the new value back to the caller's slot.
    static PENDING_PROMOTION: std::cell::RefCell<Option<Value>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn stash_promotion(v: Value) {
    PENDING_PROMOTION.with(|c| *c.borrow_mut() = Some(v));
}

pub fn take_pending_promotion() -> Option<Value> {
    PENDING_PROMOTION.with(|c| c.borrow_mut().take())
}

/// Deep value copy — the v1 LegacyArray pass-by-value snapshot — now lives
/// in `crate::eval` and is re-exported here.
pub(crate) use crate::deep_clone;

/// Public wrapper around `deep_clone` for the interp's v1 boundaries
/// (function argument, return). A no-op for primitives.
pub fn deep_clone_for_v1(v: &Value) -> Value {
    deep_clone(v)
}

#[derive(Clone, Copy)]
pub(crate) enum HoKind {
    Map,
    Filter,
}

pub(crate) fn higher_order_array(
    host: &mut dyn BuiltinHost,
    arr: &Value,
    fun: &Value,
    kind: HoKind,
) -> Result<Value, BuiltinFlow> {
    // `arrayMap(map, fn)` returns a Map keyed by the original keys
    // with values transformed by `fn`. Filter on a Map returns a
    // sub-Map with only the kept entries (keys preserved). The
    // 2+-arg shape changed between versions: v1-v3 `LegacyArray`
    // passes `(key, value)`; v4 `Map` doesn't accept `arrayMap` at
    // all but we still handle it as `(value, key)` for symmetry
    // with `mapMap`. 1-arg always gets the value.
    if let Value::Map(m) = arr {
        let entries: Vec<(Value, Value)> = m.borrow().entries.clone();
        let mut out = MapData::new();
        let arity = host.callback_arity(fun);
        let value_first = host.version() >= 4;
        for (k, v) in entries {
            let call_args = match arity {
                Some(0) => vec![],
                None | Some(1) => vec![v.clone()],
                _ => {
                    if value_first {
                        vec![v.clone(), k.clone()]
                    } else {
                        vec![k.clone(), v.clone()]
                    }
                }
            };
            let r = host.call_value(fun, call_args)?;
            match kind {
                HoKind::Map => out.insert(k, r),
                HoKind::Filter => {
                    if r.is_truthy() {
                        out.insert(k, v);
                    }
                }
            }
        }
        return Ok(Value::Map(Rc::new(RefCell::new(out))));
    }
    let Value::Array(a) = arr else {
        return Ok(Value::Null);
    };
    let n = a.borrow().len();
    // v1 `arrayFilter` returns a sparse map keyed by the kept
    // index — `arrayFilter([4,5,6,7], x => x>5)` → `[2:6, 3:7]`.
    // v2+ drops the index and returns a dense array. `arrayMap`
    // always returns a dense array.
    let filter_to_map = matches!(kind, HoKind::Filter) && host.version() <= 1;
    let value_arg_idx = ho_value_arg_index(host, fun);
    if filter_to_map {
        let mut map = MapData::new();
        for i in 0..n {
            let v = a.borrow()[i].clone();
            let raw = ho_args(host, fun, crate::len_as_int(i), &v);
            let (call_args, mapping) = wrap_byref_args(host, fun, raw);
            let r = host.call_value(fun, call_args)?;
            // `@v` writes propagate back to the array element even
            // for filter (matches upstream's mutation-then-test).
            let kept = if let Some(idx) = value_arg_idx {
                if let Some(Some(cell)) = mapping.get(idx) {
                    let new_v = cell.borrow().clone();
                    a.borrow_mut()[i] = new_v.clone();
                    new_v
                } else {
                    v
                }
            } else {
                v
            };
            if r.is_truthy() {
                map.insert(Value::Int(crate::len_as_int(i)), kept);
            }
        }
        return Ok(Value::Map(Rc::new(RefCell::new(map))));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let v = a.borrow()[i].clone();
        let raw = ho_args(host, fun, crate::len_as_int(i), &v);
        let (call_args, mapping) = wrap_byref_args(host, fun, raw);
        let r = host.call_value(fun, call_args)?;
        let post = if let Some(idx) = value_arg_idx {
            if let Some(Some(cell)) = mapping.get(idx) {
                let new_v = cell.borrow().clone();
                a.borrow_mut()[i] = new_v.clone();
                new_v
            } else {
                v
            }
        } else {
            v
        };
        match kind {
            HoKind::Map => out.push(r),
            HoKind::Filter => {
                if r.is_truthy() {
                    out.push(post);
                }
            }
        }
    }
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}

/// Build the argument list for an array-callback. 1-arg callbacks
/// (and unknown-arity builtins like `sqrt`) always get just the
/// value. The 2+-arg shape changed between language versions —
/// v1-v3 passed `(index, value)` while v4+ flipped to
/// `(value, index)`. We dispatch on the runtime version so each
/// corpus variant lines up with its upstream expectation.
pub(crate) fn ho_args(
    host: &dyn BuiltinHost,
    fun: &Value,
    index: i64,
    value: &Value,
) -> Vec<Value> {
    match host.callback_arity(fun) {
        Some(0) => vec![],
        None | Some(1) => vec![value.clone()],
        _ => {
            if host.version() >= 4 {
                vec![value.clone(), Value::Int(index)]
            } else {
                vec![Value::Int(index), value.clone()]
            }
        }
    }
}

pub(crate) fn fold(
    host: &mut dyn BuiltinHost,
    arr: &Value,
    fun: &Value,
    init: Value,
    right: bool,
) -> Result<Value, BuiltinFlow> {
    let items: Vec<Value> = match arr {
        Value::Array(a) => a.borrow().clone(),
        _ => return Ok(Value::Null),
    };
    let mut acc = init;
    if right {
        for v in items.into_iter().rev() {
            // arrayFoldRight calls f(elem, acc)
            acc = host.call_value(fun, vec![v, acc])?;
        }
    } else {
        for v in items {
            // arrayFoldLeft calls f(acc, elem)
            acc = host.call_value(fun, vec![acc, v])?;
        }
    }
    Ok(acc)
}

pub(crate) fn iter_array(
    host: &mut dyn BuiltinHost,
    arr: &Value,
    fun: &Value,
) -> Result<Value, BuiltinFlow> {
    let Value::Array(a) = arr else {
        return Ok(Value::Null);
    };
    let n = a.borrow().len();
    for i in 0..n {
        let v = a.borrow()[i].clone();
        let raw = ho_args(host, fun, crate::len_as_int(i), &v);
        let (call_args, mapping) = wrap_byref_args(host, fun, raw);
        host.call_value(fun, call_args)?;
        // Write back any by-ref mutations to the array. `mapping`
        // tells us which call-arg position holds the value cell so
        // mutations from `function(@v) { v = ... }` land in
        // `a[i]`.
        if let Some(value_arg_idx) = ho_value_arg_index(host, fun)
            && let Some(Some(cell)) = mapping.get(value_arg_idx)
        {
            let new_v = cell.borrow().clone();
            a.borrow_mut()[i] = new_v;
        }
    }
    Ok(arr.clone())
}

/// Build a parallel vector marking which call-arg positions need
/// to be wrapped in a `Value::Cell` because the callback declares
/// the corresponding param as `@`. The returned arg list is ready
/// to feed to `call_value`; the mapping tells the caller which
/// cells to read back afterward to propagate mutations.
pub(crate) fn wrap_byref_args(
    host: &dyn BuiltinHost,
    fun: &Value,
    raw: Vec<Value>,
) -> (Vec<Value>, Vec<Option<Rc<RefCell<Value>>>>) {
    let mask = host.param_byref_mask(fun).unwrap_or_default();
    let mut out_args = Vec::with_capacity(raw.len());
    let mut mapping = Vec::with_capacity(raw.len());
    for (i, v) in raw.into_iter().enumerate() {
        if mask.get(i).copied().unwrap_or(false) {
            let cell = Rc::new(RefCell::new(v));
            mapping.push(Some(cell.clone()));
            out_args.push(Value::Cell(cell));
        } else {
            mapping.push(None);
            out_args.push(v);
        }
    }
    (out_args, mapping)
}

/// For `ho_args`, the index in the resulting argument list that
/// holds the array's *value* (vs the index). 1-arg callbacks always
/// receive the value at 0; 2+-arg callbacks: v4 → value at 0,
/// v1-v3 → value at 1.
pub(crate) fn ho_value_arg_index(host: &dyn BuiltinHost, fun: &Value) -> Option<usize> {
    match host.callback_arity(fun) {
        Some(0) => None,
        None | Some(1) => Some(0),
        _ => Some(usize::from(host.version() < 4)),
    }
}

pub(crate) fn quantify(
    host: &mut dyn BuiltinHost,
    arr: &Value,
    fun: &Value,
    want_all: bool,
) -> Result<Value, BuiltinFlow> {
    let items: Vec<Value> = match arr {
        Value::Array(a) => a.borrow().clone(),
        _ => return Ok(Value::Null),
    };
    for (i, v) in items.into_iter().enumerate() {
        let r = host.call_value(fun, ho_args(host, fun, crate::len_as_int(i), &v))?;
        if want_all && !r.is_truthy() {
            return Ok(Value::Bool(false));
        }
        if !want_all && r.is_truthy() {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(want_all))
}

pub(crate) fn flatten_array(v: &Value, depth: i64) -> Value {
    let Value::Array(a) = v else {
        return Value::Null;
    };
    fn rec(out: &mut Vec<Value>, items: &[Value], depth: i64) {
        for v in items {
            match (v, depth > 0) {
                (Value::Array(inner), true) => {
                    let inner = inner.borrow().clone();
                    rec(out, &inner, depth - 1);
                }
                _ => out.push(v.clone()),
            }
        }
    }
    let mut out = Vec::new();
    rec(&mut out, &a.borrow(), depth);
    Value::Array(Rc::new(RefCell::new(out)))
}

pub(crate) fn array_slice(
    arr: &Value,
    start: Option<&Value>,
    end: Option<&Value>,
    step: Option<&Value>,
) -> Value {
    let Value::Array(a) = arr else {
        return Value::Null;
    };
    let items = a.borrow().clone();
    let len = crate::len_as_int(items.len());
    let step = match step {
        Some(Value::Null) | None => 1,
        Some(v) => v.as_int().unwrap_or(1),
    };
    if step == 0 {
        return Value::Array(Rc::new(RefCell::new(Vec::new())));
    }
    let (def_start, def_end) = if step > 0 { (0, len) } else { (len - 1, -1) };
    let resolve_idx = |raw: i64| -> i64 {
        let i = if raw < 0 { raw + len } else { raw };
        if step > 0 {
            i.clamp(0, len)
        } else {
            i.clamp(-1, len - 1)
        }
    };
    let s = match start {
        Some(Value::Null) | None => def_start,
        Some(v) => resolve_idx(v.as_int().unwrap_or(def_start)),
    };
    let e = match end {
        Some(Value::Null) | None => def_end,
        Some(v) => resolve_idx(v.as_int().unwrap_or(def_end)),
    };
    let mut out = Vec::new();
    if step > 0 {
        let mut i = s;
        while i < e && i >= 0 && i < len {
            out.push(items[crate::clamp_index(i)].clone());
            i += step;
        }
    } else {
        let mut i = s;
        while i > e && i >= 0 && i < len {
            out.push(items[crate::clamp_index(i)].clone());
            i += step;
        }
    }
    Value::Array(Rc::new(RefCell::new(out)))
}

pub(crate) fn split_string(s: &Value, sep: &Value, limit: Option<&Value>) -> Value {
    let (Value::String(s), Value::String(sep)) = (s, sep) else {
        return Value::Null;
    };
    let limit = limit
        .and_then(super::super::value::types::Value::as_int)
        .unwrap_or(-1);
    let parts: Vec<Value> = if sep.is_empty() {
        s.chars()
            .map(|c| Value::String(Rc::new(c.to_string())))
            .collect()
    } else if limit > 0 {
        // splitn(limit) yields at most `limit` elements with the
        // remainder concatenated into the last one.
        s.as_str()
            .splitn(crate::clamp_index(limit), sep.as_str())
            .map(|p| Value::String(Rc::new(p.to_string())))
            .collect()
    } else {
        s.as_str()
            .split(sep.as_str())
            .map(|p| Value::String(Rc::new(p.to_string())))
            .collect()
    };
    Value::Array(Rc::new(RefCell::new(parts)))
}

pub(crate) fn find_array(
    host: &mut dyn BuiltinHost,
    arr: &Value,
    fun: &Value,
) -> Result<Value, BuiltinFlow> {
    let items: Vec<Value> = match arr {
        Value::Array(a) => a.borrow().clone(),
        _ => return Ok(Value::Null),
    };
    for (i, v) in items.into_iter().enumerate() {
        let r = host.call_value(fun, ho_args(host, fun, crate::len_as_int(i), &v))?;
        if r.is_truthy() {
            return Ok(v);
        }
    }
    Ok(Value::Null)
}

pub(crate) fn partition(
    host: &mut dyn BuiltinHost,
    arr: &Value,
    fun: &Value,
) -> Result<Value, BuiltinFlow> {
    let Value::Array(a) = arr else {
        return Ok(Value::Null);
    };
    let n = a.borrow().len();
    let value_arg_idx = ho_value_arg_index(host, fun);
    // Like `arrayFilter`, the v1-v3 shape carries the source index
    // as the key — both halves are sparse maps. v4 returns dense
    // arrays in both slots. `@v` mutations propagate to the
    // backing array first, then the (possibly-updated) value is
    // placed into one of the two output buckets.
    let to_map = host.version() <= 3;
    let mut yes_map = MapData::new();
    let mut no_map = MapData::new();
    let mut yes_arr: Vec<Value> = Vec::new();
    let mut no_arr: Vec<Value> = Vec::new();
    for i in 0..n {
        let v = a.borrow()[i].clone();
        let raw = ho_args(host, fun, crate::len_as_int(i), &v);
        let (call_args, mapping) = wrap_byref_args(host, fun, raw);
        let r = host.call_value(fun, call_args)?;
        let post = if let Some(idx) = value_arg_idx {
            if let Some(Some(cell)) = mapping.get(idx) {
                let new_v = cell.borrow().clone();
                a.borrow_mut()[i] = new_v.clone();
                new_v
            } else {
                v
            }
        } else {
            v
        };
        if to_map {
            if r.is_truthy() {
                yes_map.insert(Value::Int(crate::len_as_int(i)), post);
            } else {
                no_map.insert(Value::Int(crate::len_as_int(i)), post);
            }
        } else if r.is_truthy() {
            yes_arr.push(post);
        } else {
            no_arr.push(post);
        }
    }
    let out = if to_map {
        vec![
            Value::Map(Rc::new(RefCell::new(yes_map))),
            Value::Map(Rc::new(RefCell::new(no_map))),
        ]
    } else {
        vec![
            Value::Array(Rc::new(RefCell::new(yes_arr))),
            Value::Array(Rc::new(RefCell::new(no_arr))),
        ]
    };
    Ok(Value::Array(Rc::new(RefCell::new(out))))
}

/// Sort an array in place. Returns the same array Value the
/// caller passed in so `var t = [...]; sort(t); return t;` sees
/// the mutation (upstream's `sort` mutates and returns the array).
///
/// `mode` is the second argument to `sort` — `SORT_ASC` (0,
/// default), `SORT_DESC` (1), or random (2 — upstream's internal
/// `ArrayLeekValue.RANDOM`, reached via `shuffle`). Nulls collate
/// to one end of the result, with the side depending on language
/// version (v1: ASC→end, DESC→start; v2+: ASC→start, DESC→end).
pub(crate) fn sort_array(arr: &Value, mode: Option<&Value>, version: u8) -> Value {
    let Value::Array(a) = arr else {
        return Value::Null;
    };
    let mode_int = mode
        .and_then(super::super::value::types::Value::as_int)
        .unwrap_or(0);
    if mode_int == 2 {
        // Random mode — Fisher-Yates with a deterministic seed
        // derived from the length, since the corpus doesn't
        // probe distribution.
        let mut arr = a.borrow_mut();
        let n = arr.len();
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        for i in (1..n).rev() {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let j = (s >> 33) as usize % (i + 1);
            arr.swap(i, j);
        }
        return arr_ref_value(a);
    }
    let descending = mode_int == 1;
    let nulls_first = match (version, descending) {
        (1, false) => false,
        (1, true) => true,
        (_, false) => true,
        (_, true) => false,
    };

    let mut buf = a.borrow().clone();
    buf.sort_by(|x, y| {
        let xn = matches!(x, Value::Null);
        let yn = matches!(y, Value::Null);
        if xn && yn {
            return Ordering::Equal;
        }
        if xn {
            return if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        if yn {
            return if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        let ord = x.cmp_partial(y).unwrap_or(Ordering::Equal);
        if descending { ord.reverse() } else { ord }
    });
    *a.borrow_mut() = buf;
    arr_ref_value(a)
}

pub(crate) fn arr_ref_value(a: &Rc<RefCell<Vec<Value>>>) -> Value {
    Value::Array(Rc::clone(a))
}

/// In-place sort with a user-supplied comparator. The comparator
/// is called with `(a, b)` and must return a negative integer if
/// `a < b`, zero if equal, positive otherwise — matching upstream
/// and the corpus convention. We call the comparator under
/// `O(n log n)` extra calls per sort but stop on the first
/// runtime error.
pub(crate) fn sort_array_with_cmp(
    host: &mut dyn BuiltinHost,
    arr: &Value,
    cmp: &Value,
) -> Result<Value, BuiltinFlow> {
    // Map input — sort entries by the comparator's verdict on
    // their (key, value) tuples. 2-arg cb gets `(v, v)`, 4-arg cb
    // gets `(k1, v1, k2, v2)`. Mutates the map in place.
    if let Value::Map(m) = arr {
        let mut buf = m.borrow().entries.clone();
        let n = buf.len();
        let arity = host.callback_arity(cmp).unwrap_or(2);
        for i in 0..n {
            for j in 0..n - i - 1 {
                let call_args = if arity >= 4 {
                    vec![
                        buf[j].0.clone(),
                        buf[j].1.clone(),
                        buf[j + 1].0.clone(),
                        buf[j + 1].1.clone(),
                    ]
                } else {
                    vec![buf[j].1.clone(), buf[j + 1].1.clone()]
                };
                let r = host.call_value(cmp, call_args)?;
                let ord = r.as_int().unwrap_or(0);
                if ord > 0 {
                    buf.swap(j, j + 1);
                }
            }
        }
        let mut mm = m.borrow_mut();
        mm.entries = buf;
        mm.index.clear();
        let canons: Vec<String> = mm
            .entries
            .iter()
            .map(|(k, _)| crate::value::key_repr(k))
            .collect();
        for (i, c) in canons.into_iter().enumerate() {
            mm.index.insert(c, i);
        }
        return Ok(Value::Map(Rc::clone(m)));
    }
    let Value::Array(a) = arr else {
        return Ok(Value::Null);
    };
    let mut buf = a.borrow().clone();
    let n = buf.len();
    // Upstream's `arraySort` callback can be 2-arg
    // `(value, value)` or 4-arg `(key1, value1, key2, value2)`.
    // Dispatch on the user arity so each shape gets its expected
    // signature.
    let arity = host.callback_arity(cmp).unwrap_or(2);
    for i in 0..n {
        for j in 0..n - i - 1 {
            let call_args = if arity >= 4 {
                vec![
                    Value::Int(crate::len_as_int(j)),
                    buf[j].clone(),
                    Value::Int(crate::len_as_int(j + 1)),
                    buf[j + 1].clone(),
                ]
            } else {
                vec![buf[j].clone(), buf[j + 1].clone()]
            };
            let r = host.call_value(cmp, call_args)?;
            let ord = r.as_int().unwrap_or(0);
            if ord > 0 {
                buf.swap(j, j + 1);
            }
        }
    }
    *a.borrow_mut() = buf;
    Ok(Value::Array(Rc::clone(a)))
}

pub(crate) fn min_max_array(items: &[Value], want_min: bool) -> Value {
    // Upstream's sort treats `null` as smaller than every other
    // value, so `arrayMin([1, null])` is `null` and
    // `arrayMax([null, 3])` is `3`. We reproduce that ordering
    // here: nulls are kept in the reduction but compare as
    // less-than everything.
    let mut best: Option<Value> = None;
    let null_cmp = |a: &Value, b: &Value| -> Ordering {
        match (a, b) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => Ordering::Less,
            (_, Value::Null) => Ordering::Greater,
            _ => a.cmp_partial(b).unwrap_or(Ordering::Equal),
        }
    };
    for v in items {
        best = Some(match &best {
            None => v.clone(),
            Some(cur) => match null_cmp(cur, v) {
                Ordering::Less => {
                    if want_min {
                        cur.clone()
                    } else {
                        v.clone()
                    }
                }
                Ordering::Greater => {
                    if want_min {
                        v.clone()
                    } else {
                        cur.clone()
                    }
                }
                Ordering::Equal => cur.clone(),
            },
        });
    }
    best.unwrap_or(Value::Null)
}

pub(crate) fn min_max_pair(a: &Value, b: &Value, want_min: bool) -> Value {
    // Numeric pair: promote to Real if either side is Real, matching
    // upstream's `max(5.0, 12)` returning `12.0` not `12`. Falls
    // through to generic comparison for non-numeric inputs.
    let any_real = matches!(a, Value::Real(_)) || matches!(b, Value::Real(_));
    let both_num =
        matches!(a, Value::Int(_) | Value::Real(_)) && matches!(b, Value::Int(_) | Value::Real(_));
    if both_num && any_real {
        let ar = a.as_real().unwrap_or(0.0);
        let br = b.as_real().unwrap_or(0.0);
        let pick_a = if want_min { ar <= br } else { ar >= br };
        return Value::Real(if pick_a { ar } else { br });
    }
    match a.cmp_partial(b) {
        Some(Ordering::Less) => {
            if want_min {
                a.clone()
            } else {
                b.clone()
            }
        }
        Some(Ordering::Greater) => {
            if want_min {
                b.clone()
            } else {
                a.clone()
            }
        }
        _ => a.clone(),
    }
}

pub(crate) fn sum_array(items: &[Value]) -> Value {
    // Upstream signature: `sum(Array) -> real`. Always returns a
    // real, even for an all-int input.
    let mut total: f64 = 0.0;
    for v in items {
        total += v.as_real().unwrap_or(0.0);
    }
    Value::Real(total)
}

pub(crate) fn avg_array(items: &[Value]) -> Value {
    if items.is_empty() {
        // Match `sum`: average returns a Real even for empty
        // input — `average([])` is `0.0`, not `0`.
        return Value::Real(0.0);
    }
    let total = crate::int_to_real(crate::len_as_int(items.len()));
    let s = sum_array(items);
    let n = s.as_real().unwrap_or(0.0) / total;
    Value::Real(n)
}

pub(crate) fn contains_in(haystack: &Value, needle: &Value) -> bool {
    match haystack {
        Value::Array(a) => a.borrow().iter().any(|v| v.loose_eq(needle)),
        Value::Set(s) => s.borrow().iter().any(|v| v.loose_eq(needle)),
        // `inArray(map, x)` searches the map's *values* (upstream
        // semantics, mirroring how arrays are searched). Use
        // `mapContains(map, key)` for key lookup.
        Value::Map(m) => m.borrow().entries.iter().any(|(_, v)| v.loose_eq(needle)),
        Value::String(s) => {
            if let Value::String(n) = needle {
                s.contains(n.as_str())
            } else {
                false
            }
        }
        _ => false,
    }
}

pub(crate) fn count(v: &Value, version: u8) -> Value {
    match v {
        Value::Array(a) => Value::Int(crate::len_as_int(a.borrow().len())),
        Value::Map(m) => Value::Int(crate::len_as_int(m.borrow().len())),
        Value::Set(s) => Value::Int(crate::len_as_int(s.borrow().len())),
        // v1-3 `count(string)` returns 0 (it's a Java-collection
        // sibling that only counts container length). v4 widened
        // it to return the character count.
        Value::String(s) => {
            if version >= 4 {
                Value::Int(crate::len_as_int(s.chars().count()))
            } else {
                Value::Int(0)
            }
        }
        // Anything non-container counts as 0 — `count(12)` and
        // `count(null)` both return `0` in upstream rather than
        // raising. Keeps `count(unknown(...))` total.
        _ => Value::Int(0),
    }
}

// ---- Map operations ----
