//! Builtin map dispatch.
//!
//! The Leekscript stdlib has hundreds of free functions; this is a
//! best-effort subset focused on what the upstream test corpus
//! exercises. Anything unrecognised returns `null`, matching
//! upstream's "missing builtin" runtime behavior.

use std::cell::RefCell;
use std::rc::Rc;

use crate::{BuiltinFlow, BuiltinHost};
use crate::value::{MapData, Value};

use super::array::{min_max_array, stash_promotion, sum_array};

pub(crate) fn dispatch_map(
    host: &mut dyn BuiltinHost,
    name: &str,
    args: &[Value],
) -> Result<Option<Value>, BuiltinFlow> {
    Ok(Some(match (name, args.len()) {
        ("mapGet", 2) => match &args[0] {
            Value::Map(m) => m.borrow().get(&args[1]).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        },
        // `mapGet(map, key, default)` — return `default` when the
        // key is absent OR when `map` isn't actually a map.
        ("mapGet", 3) => match &args[0] {
            Value::Map(m) => m
                .borrow()
                .get(&args[1])
                .cloned()
                .unwrap_or_else(|| args[2].clone()),
            _ => args[2].clone(),
        },
        ("mapPut", 3) => {
            if let Value::Map(m) = &args[0] {
                let canon = crate::value::key_repr(&args[1]);
                m.borrow_mut()
                    .insert_canonical(canon, args[1].clone(), args[2].clone());
            }
            args[0].clone()
        }
        // Upstream split these: `mapContains` checks for a
        // matching VALUE (loose-equality), `mapContainsKey` checks
        // for a matching KEY.
        ("mapContains" | "mapContainsValue", 2) => match &args[0] {
            Value::Map(m) => {
                Value::Bool(m.borrow().entries.iter().any(|(_, v)| v.loose_eq(&args[1])))
            }
            _ => Value::Bool(false),
        },
        ("mapContainsKey", 2) => match &args[0] {
            Value::Map(m) => Value::Bool(m.borrow().get(&args[1]).is_some()),
            _ => Value::Bool(false),
        },
        ("mapRemove", 2) => match &args[0] {
            Value::Map(m) => {
                let canon = crate::value::key_repr(&args[1]);
                let mut mm = m.borrow_mut();
                if let Some(&idx) = mm.index.get(&canon) {
                    let removed = mm.entries.remove(idx).1;
                    // Reindex.
                    mm.index.clear();
                    let canons: Vec<String> = mm
                        .entries
                        .iter()
                        .map(|(k, _)| crate::value::key_repr(k))
                        .collect();
                    for (i, c) in canons.into_iter().enumerate() {
                        mm.index.insert(c, i);
                    }
                    removed
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        },
        // `keys(x)` / `values(x)` extract the keys / values of a
        // map, set, or object. Mirror upstream's `keys()` and
        // `values()` overloads on each type.
        ("mapKeys", 1) => match &args[0] {
            Value::Map(m) => {
                let keys: Vec<Value> = m.borrow().entries.iter().map(|(k, _)| k.clone()).collect();
                Value::Array(Rc::new(RefCell::new(keys)))
            }
            Value::Object(o) => {
                let keys: Vec<Value> = o
                    .borrow()
                    .iter()
                    .map(|(k, _)| Value::String(Rc::new(k.clone())))
                    .collect();
                Value::Array(Rc::new(RefCell::new(keys)))
            }
            Value::Instance(inst) => {
                let keys: Vec<Value> = inst
                    .borrow()
                    .fields
                    .iter()
                    .map(|(k, _)| Value::String(Rc::new(k.clone())))
                    .collect();
                Value::Array(Rc::new(RefCell::new(keys)))
            }
            Value::Set(s) => {
                let keys: Vec<Value> = s.borrow().iter().cloned().collect();
                Value::Array(Rc::new(RefCell::new(keys)))
            }
            _ => Value::Null,
        },
        ("mapValues", 1) => match &args[0] {
            Value::Map(m) => {
                let vals: Vec<Value> = m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                Value::Array(Rc::new(RefCell::new(vals)))
            }
            Value::Object(o) => {
                let vals: Vec<Value> = o.borrow().iter().map(|(_, v)| v.clone()).collect();
                Value::Array(Rc::new(RefCell::new(vals)))
            }
            Value::Instance(inst) => {
                let vals: Vec<Value> = inst
                    .borrow()
                    .fields
                    .iter()
                    .map(|(_, v)| v.clone())
                    .collect();
                Value::Array(Rc::new(RefCell::new(vals)))
            }
            Value::Set(s) => {
                let vals: Vec<Value> = s.borrow().iter().cloned().collect();
                Value::Array(Rc::new(RefCell::new(vals)))
            }
            _ => Value::Null,
        },
        ("mapSize", 1) => match &args[0] {
            Value::Map(m) => Value::Int(crate::len_as_int(m.borrow().len())),
            _ => Value::Null,
        },
        ("mapClear", 1) => match &args[0] {
            Value::Map(m) => {
                let mut mm = m.borrow_mut();
                mm.entries.clear();
                mm.index.clear();
                args[0].clone()
            }
            _ => args[0].clone(),
        },
        ("mapIsEmpty", 1) => match &args[0] {
            Value::Map(m) => Value::Bool(m.borrow().is_empty()),
            _ => Value::Bool(true),
        },
        ("mapMap", 2) => map_map(host, &args[0], &args[1])?,
        ("mapFilter", 2) => map_filter(host, &args[0], &args[1])?,
        ("mapIter", 2) => map_iter(host, &args[0], &args[1])?,
        ("mapFold", 3) => map_fold(host, &args[0], &args[1], args[2].clone())?,
        ("mapSome", 2) => map_quantify(host, &args[0], &args[1], false)?,
        ("mapEvery", 2) => map_quantify(host, &args[0], &args[1], true)?,
        // `mapSearch(map, value)` → first key whose value equals
        // `value`, else null.
        ("mapSearch", 2) => match &args[0] {
            Value::Map(m) => m
                .borrow()
                .entries
                .iter()
                .find(|(_, v)| v.loose_eq(&args[1]))
                .map_or(Value::Null, |(k, _)| k.clone()),
            _ => Value::Null,
        },
        // `mapMerge(a, b)` — new map with a's entries plus
        // any entries from b whose key isn't already present.
        // Matches `MapLeekValue.mapMerge` (`putIfAbsent`).
        ("mapMerge", 2) => match (&args[0], &args[1]) {
            (Value::Map(a), Value::Map(b)) => {
                let mut out = a.borrow().entries.clone();
                let mut idx = std::collections::HashMap::new();
                for (i, (k, _)) in out.iter().enumerate() {
                    idx.insert(crate::value::key_repr(k), i);
                }
                for (k, v) in &b.borrow().entries {
                    let canon = crate::value::key_repr(k);
                    if let std::collections::hash_map::Entry::Vacant(e) = idx.entry(canon) {
                        e.insert(out.len());
                        out.push((k.clone(), v.clone()));
                    }
                }
                Value::Map(Rc::new(RefCell::new(MapData {
                    entries: out,
                    index: idx,
                })))
            }
            _ => Value::Null,
        },
        // `mapPutAll(target, source)` — copy every entry from
        // source into target, overwriting on key collisions.
        // Mutates target; returns null (Java return is void).
        ("mapPutAll", 2) => {
            if let (Value::Map(t), Value::Map(s)) = (&args[0], &args[1]) {
                let entries = s.borrow().entries.clone();
                let mut tb = t.borrow_mut();
                for (k, v) in entries {
                    let canon = crate::value::key_repr(&k);
                    tb.insert_canonical(canon, k, v);
                }
            }
            Value::Null
        }
        // `mapFill(map, value)` — overwrite every entry's value.
        // Mutates target; returns null.
        ("mapFill", 2) => {
            if let Value::Map(m) = &args[0] {
                for (_, v) in &mut m.borrow_mut().entries {
                    *v = args[1].clone();
                }
            }
            Value::Null
        }
        // `mapReplace(map, key, value)` — write `value` only when
        // `key` is already present; returns the previous value
        // (or null if absent).
        ("mapReplace", 3) => match &args[0] {
            Value::Map(m) => {
                let canon = crate::value::key_repr(&args[1]);
                let mut mm = m.borrow_mut();
                if let Some(&idx) = mm.index.get(&canon) {
                    
                    std::mem::replace(&mut mm.entries[idx].1, args[2].clone())
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        },
        // `mapReplaceAll(target, source)` — write every key/value
        // from source over target, but only for keys target
        // already has. Matches Java's `MapLeekValue.replaceAll`.
        ("mapReplaceAll", 2) => {
            if let (Value::Map(t), Value::Map(s)) = (&args[0], &args[1]) {
                let entries = s.borrow().entries.clone();
                let mut tb = t.borrow_mut();
                for (k, v) in entries {
                    let canon = crate::value::key_repr(&k);
                    if let Some(&idx) = tb.index.get(&canon) {
                        tb.entries[idx].1 = v;
                    }
                }
            }
            Value::Null
        }
        // `mapRemoveAll(map, value)` — drop every entry whose
        // *value* loose-equals `value`. Java's
        // `MapLeekValue.removeAll(value)` iterates entries and
        // removes matching values; returns void.
        ("mapRemoveAll", 2) => {
            if let Value::Map(t) = &args[0] {
                let mut tb = t.borrow_mut();
                tb.entries.retain(|(_, v)| !v.loose_eq(&args[1]));
                tb.index.clear();
                let canons: Vec<String> = tb
                    .entries
                    .iter()
                    .map(|(k, _)| crate::value::key_repr(k))
                    .collect();
                for (i, c) in canons.into_iter().enumerate() {
                    tb.index.insert(c, i);
                }
            }
            Value::Null
        }
        // `assocSort(map, [SORT_ASC|SORT_DESC])` — sort the map's
        // entries by their *values* in place, preserving keys.
        // `keySort(map, ...)` sorts by *keys* instead. Both return
        // void. Mirrors upstream's `assocSort` / `keySort`.
        ("assocSort" | "keySort", 1 | 2) => {
            let desc = args
                .get(1)
                .and_then(super::super::value::types::Value::as_int)
                .is_some_and(|n| n == 1);
            let by_keys = matches!(name, "keySort");
            // v1-v3 LegacyArray promotion: an Array gets turned
            // into a sparse map keyed by original indices, then
            // sorted (stable) by value. The post-call
            // `ApplyPromotion` writes the morphed map back to the
            // caller's slot.
            if let Value::Array(a) = &args[0]
                && host.version() <= 3 {
                    let snap: Vec<Value> = a.borrow().clone();
                    let mut indexed: Vec<(i64, Value)> = snap
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| (crate::len_as_int(i), v))
                        .collect();
                    let null_greater = host.version() <= 1;
                    indexed.sort_by(|a, b| {
                        let oa = if by_keys {
                            // keys are i64 indices; never null
                            a.0.cmp(&b.0)
                        } else {
                            let la = &a.1;
                            let lb = &b.1;
                            let la_null = matches!(la, Value::Null);
                            let lb_null = matches!(lb, Value::Null);
                            match (la_null, lb_null) {
                                (true, true) => std::cmp::Ordering::Equal,
                                (true, false) => {
                                    if null_greater {
                                        std::cmp::Ordering::Greater
                                    } else {
                                        std::cmp::Ordering::Less
                                    }
                                }
                                (false, true) => {
                                    if null_greater {
                                        std::cmp::Ordering::Less
                                    } else {
                                        std::cmp::Ordering::Greater
                                    }
                                }
                                _ => la.cmp_partial(lb).unwrap_or(std::cmp::Ordering::Equal),
                            }
                        };
                        if desc { oa.reverse() } else { oa }
                    });
                    let mut map = MapData::new();
                    for (k, v) in indexed {
                        let key = Value::Int(k);
                        let ck = crate::value::key_repr(&key);
                        map.insert_canonical(ck, key, v);
                    }
                    stash_promotion(Value::Map(Rc::new(RefCell::new(map))));
                    return Ok(Some(Value::Null));
                }
            if let Value::Map(m) = &args[0] {
                let mut mm = m.borrow_mut();
                // Stable sort with version-aware null placement —
                // v1 treats null as GREATER than any value (so it
                // collates to the end in ASC, start in DESC). v2+
                // treats null as LESS than any value (start in
                // ASC, end in DESC). The direction itself flips
                // the comparison after.
                let null_greater = host.version() <= 1;
                mm.entries.sort_by(|a, b| {
                    let (la, lb) = if by_keys { (&a.0, &b.0) } else { (&a.1, &b.1) };
                    let la_null = matches!(la, Value::Null);
                    let lb_null = matches!(lb, Value::Null);
                    let oa = match (la_null, lb_null) {
                        (true, true) => std::cmp::Ordering::Equal,
                        (true, false) => {
                            if null_greater {
                                std::cmp::Ordering::Greater
                            } else {
                                std::cmp::Ordering::Less
                            }
                        }
                        (false, true) => {
                            if null_greater {
                                std::cmp::Ordering::Less
                            } else {
                                std::cmp::Ordering::Greater
                            }
                        }
                        _ => la.cmp_partial(lb).unwrap_or(std::cmp::Ordering::Equal),
                    };
                    if desc { oa.reverse() } else { oa }
                });
                mm.index.clear();
                let canons: Vec<String> = mm
                    .entries
                    .iter()
                    .map(|(k, _)| crate::value::key_repr(k))
                    .collect();
                for (i, c) in canons.into_iter().enumerate() {
                    mm.index.insert(c, i);
                }
            }
            Value::Null
        }
        // `assocReverse(map)` — reverse the entry order. v1-v3
        // on an Array first promotes to a sparse map keyed by the
        // original indices, then reverses the entry order; the
        // promoted value is stashed for `ApplyPromotion`.
        ("assocReverse", 1) => {
            match &args[0] {
                Value::Map(m) => {
                    let mut mm = m.borrow_mut();
                    mm.entries.reverse();
                    mm.index.clear();
                    let canons: Vec<String> = mm
                        .entries
                        .iter()
                        .map(|(k, _)| crate::value::key_repr(k))
                        .collect();
                    for (i, c) in canons.into_iter().enumerate() {
                        mm.index.insert(c, i);
                    }
                }
                Value::Array(a) if host.version() <= 3 => {
                    let snap: Vec<Value> = a.borrow().clone();
                    let mut map = MapData::new();
                    for (j, v) in snap.into_iter().enumerate().rev() {
                        let k = Value::Int(crate::len_as_int(j));
                        let ck = crate::value::key_repr(&k);
                        map.insert_canonical(ck, k, v);
                    }
                    stash_promotion(Value::Map(Rc::new(RefCell::new(map))));
                }
                _ => {}
            }
            Value::Null
        }
        // `mapSum(map)` / `mapAverage(map)` / `mapMin(map)` /
        // `mapMax(map)` — reductions over the value side.
        ("mapSum", 1) => match &args[0] {
            Value::Map(m) => {
                // Mirrors `sum_array`: always returns Real, even
                // when every value is an Int.
                let vs: Vec<Value> = m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                sum_array(&vs)
            }
            _ => Value::Real(0.0),
        },
        ("mapAverage", 1) => match &args[0] {
            Value::Map(m) => {
                let entries = m.borrow();
                let n = entries.entries.len();
                if n == 0 {
                    Value::Int(0)
                } else {
                    let mut acc = 0.0;
                    for (_, v) in &entries.entries {
                        acc += v.as_real().unwrap_or(0.0);
                    }
                    Value::Real(acc / crate::int_to_real(crate::len_as_int(n)))
                }
            }
            _ => Value::Int(0),
        },
        ("mapMin", 1) => match &args[0] {
            Value::Map(m) => {
                let entries: Vec<Value> =
                    m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                min_max_array(&entries, true)
            }
            _ => Value::Null,
        },
        ("mapMax", 1) => match &args[0] {
            Value::Map(m) => {
                let entries: Vec<Value> =
                    m.borrow().entries.iter().map(|(_, v)| v.clone()).collect();
                min_max_array(&entries, false)
            }
            _ => Value::Null,
        },
        _ => return Ok(None),
    }))
}

/// Build the call arg list for a map-callback. 1-arg gets the
/// value alone; 2+-arg gets `(value, key)` — matches upstream's
/// `mapMap` / `mapFilter` / `mapIter` arity dispatch.
pub(crate) fn map_call_args(host: &dyn BuiltinHost, fun: &Value, k: &Value, v: &Value) -> Vec<Value> {
    match host.callback_arity(fun) {
        Some(0) => vec![],
        None | Some(1) => vec![v.clone()],
        _ => vec![v.clone(), k.clone()],
    }
}

pub(crate) fn map_map(host: &mut dyn BuiltinHost, m: &Value, fun: &Value) -> Result<Value, BuiltinFlow> {
    let items = match m {
        Value::Map(mm) => mm.borrow().entries.clone(),
        _ => return Ok(Value::Null),
    };
    let mut out = MapData::new();
    for (k, v) in items {
        let new_v = host.call_value(fun, map_call_args(host, fun, &k, &v))?;
        let canon = crate::value::key_repr(&k);
        out.insert_canonical(canon, k, new_v);
    }
    Ok(Value::Map(Rc::new(RefCell::new(out))))
}

pub(crate) fn map_filter(
    host: &mut dyn BuiltinHost,
    m: &Value,
    fun: &Value,
) -> Result<Value, BuiltinFlow> {
    let items = match m {
        Value::Map(mm) => mm.borrow().entries.clone(),
        _ => return Ok(Value::Null),
    };
    let mut out = MapData::new();
    for (k, v) in items {
        let keep = host.call_value(fun, map_call_args(host, fun, &k, &v))?;
        if keep.is_truthy() {
            let canon = crate::value::key_repr(&k);
            out.insert_canonical(canon, k, v);
        }
    }
    Ok(Value::Map(Rc::new(RefCell::new(out))))
}

pub(crate) fn map_iter(host: &mut dyn BuiltinHost, m: &Value, fun: &Value) -> Result<Value, BuiltinFlow> {
    let items = match m {
        Value::Map(mm) => mm.borrow().entries.clone(),
        _ => return Ok(Value::Null),
    };
    for (k, v) in items {
        host.call_value(fun, map_call_args(host, fun, &k, &v))?;
    }
    Ok(m.clone())
}

pub(crate) fn map_fold(
    host: &mut dyn BuiltinHost,
    m: &Value,
    fun: &Value,
    init: Value,
) -> Result<Value, BuiltinFlow> {
    let items = match m {
        Value::Map(mm) => mm.borrow().entries.clone(),
        _ => return Ok(Value::Null),
    };
    let mut acc = init;
    for (k, v) in items {
        acc = host.call_value(fun, vec![acc, v, k])?;
    }
    Ok(acc)
}

pub(crate) fn map_quantify(
    host: &mut dyn BuiltinHost,
    m: &Value,
    fun: &Value,
    want_all: bool,
) -> Result<Value, BuiltinFlow> {
    let items = match m {
        Value::Map(mm) => mm.borrow().entries.clone(),
        _ => return Ok(Value::Null),
    };
    for (k, v) in items {
        let r = host.call_value(fun, vec![v, k])?;
        if want_all && !r.is_truthy() {
            return Ok(Value::Bool(false));
        }
        if !want_all && r.is_truthy() {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(want_all))
}

// ---- String operations ----
