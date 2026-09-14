//! Map/set key semantics at the collection level (RT-02, #239).
//!
//! `tests/map_key_equivalence.rs` pins the canonicalisation itself
//! against `key_repr`; this pins what that means for `MapData` and
//! `SetData` — how many entries a mixed-type key set produces, and
//! that insertion order survives a removal now that removal no
//! longer rebuilds the index from scratch.

use std::cell::RefCell;
use std::rc::Rc;

use leek_runtime::{MapData, SetData, Value, big_from_decimal};

fn s(t: &str) -> Value {
    Value::String(Rc::new(t.to_string()))
}

fn keys(m: &MapData) -> Vec<String> {
    m.entries.iter().map(|(k, _)| k.to_string()).collect()
}

#[test]
fn distinct_types_are_distinct_keys() {
    // `m[1]`, `m[1.0]`, `m["1"]`, `m[true]`, `m[null]` and `m[1L]`
    // are six separate entries — upstream never coerces a map key.
    let mut m = MapData::new();
    m.insert(Value::Int(1), s("int"));
    m.insert(Value::Real(1.0), s("real"));
    m.insert(s("1"), s("string"));
    m.insert(Value::Bool(true), s("bool"));
    m.insert(Value::Null, s("null"));
    m.insert(Value::BigInt(Rc::new(big_from_decimal("1"))), s("big"));
    assert_eq!(m.len(), 6);

    assert_eq!(
        m.get(&Value::Int(1)).map(ToString::to_string).as_deref(),
        Some("\"int\"")
    );
    assert_eq!(
        m.get(&Value::Real(1.0)).map(ToString::to_string).as_deref(),
        Some("\"real\"")
    );
    assert_eq!(
        m.get(&s("1")).map(ToString::to_string).as_deref(),
        Some("\"string\"")
    );
    assert_eq!(
        m.get(&Value::Bool(true))
            .map(ToString::to_string)
            .as_deref(),
        Some("\"bool\"")
    );
    assert_eq!(
        m.get(&Value::Null).map(ToString::to_string).as_deref(),
        Some("\"null\"")
    );
    assert_eq!(
        m.get(&Value::BigInt(Rc::new(big_from_decimal("1"))))
            .map(ToString::to_string)
            .as_deref(),
        Some("\"big\"")
    );
}

#[test]
fn nan_is_one_key_and_signed_zero_is_two() {
    let mut m = MapData::new();
    m.insert(Value::Real(f64::NAN), Value::Int(1));
    m.insert(Value::Real(f64::NAN), Value::Int(2));
    // Two writes of NaN are one entry, last write wins — even though
    // `NaN != NaN` under ordinary comparison.
    assert_eq!(m.len(), 1);
    assert!(matches!(m.get(&Value::Real(f64::NAN)), Some(Value::Int(2))));

    m.insert(Value::Real(0.0), Value::Int(3));
    m.insert(Value::Real(-0.0), Value::Int(4));
    assert_eq!(m.len(), 3);
    assert!(matches!(m.get(&Value::Real(0.0)), Some(Value::Int(3))));
    assert!(matches!(m.get(&Value::Real(-0.0)), Some(Value::Int(4))));
}

#[test]
fn a_string_spelled_like_a_canonical_prefix_is_still_a_string() {
    let mut m = MapData::new();
    m.insert(Value::Int(5), s("int five"));
    m.insert(s("i:5"), s("string i colon five"));
    m.insert(Value::Null, s("real null"));
    m.insert(s("null"), s("string null"));
    m.insert(Value::Bool(true), s("bool"));
    m.insert(s("b:true"), s("string b colon true"));
    assert_eq!(m.len(), 6);
}

#[test]
fn removing_a_middle_key_preserves_insertion_order() {
    let mut m = MapData::new();
    for i in 0..5 {
        m.insert(Value::Int(i), Value::Int(i * 10));
    }
    assert!(matches!(m.remove(&Value::Int(2)), Some(Value::Int(20))));
    assert_eq!(keys(&m), vec!["0", "1", "3", "4"]);
    assert_eq!(m.len(), 4);

    // The index has to follow the shift, or the survivors read back
    // as the wrong entry.
    for i in [0, 1, 3, 4] {
        assert!(
            matches!(m.get(&Value::Int(i)), Some(Value::Int(v)) if *v == i * 10),
            "entry {i} reads back wrong after the removal"
        );
    }
    assert!(m.get(&Value::Int(2)).is_none());
    assert!(m.remove(&Value::Int(2)).is_none());

    // …and a later insert still lands at the end.
    m.insert(Value::Int(9), Value::Int(90));
    assert_eq!(keys(&m), vec!["0", "1", "3", "4", "9"]);
    assert!(matches!(m.get(&Value::Int(9)), Some(Value::Int(90))));
}

#[test]
fn reindex_follows_a_permutation() {
    let mut m = MapData::new();
    for i in 0..5 {
        m.insert(Value::Int(i), Value::Int(i * 10));
    }
    m.entries.reverse();
    m.reindex();
    assert_eq!(keys(&m), vec!["4", "3", "2", "1", "0"]);
    for i in 0..5 {
        assert!(matches!(m.get(&Value::Int(i)), Some(Value::Int(v)) if *v == i * 10));
    }
}

#[test]
fn set_remove_preserves_order_of_the_rest() {
    let mut set = SetData::new();
    for i in 0..5 {
        assert!(set.insert(Value::Int(i)));
    }
    // Duplicates are dropped, first occurrence wins.
    assert!(!set.insert(Value::Int(3)));
    assert_eq!(set.len(), 5);

    assert!(set.remove(&Value::Int(1)));
    assert!(!set.remove(&Value::Int(1)));
    let order: Vec<String> = set.iter().map(ToString::to_string).collect();
    assert_eq!(order, vec!["0", "2", "3", "4"]);
    for i in [0, 2, 3, 4] {
        assert!(set.contains(&Value::Int(i)), "lost element {i}");
    }
    assert!(!set.contains(&Value::Int(1)));

    // Re-inserting the removed element appends it.
    assert!(set.insert(Value::Int(1)));
    let order: Vec<String> = set.iter().map(ToString::to_string).collect();
    assert_eq!(order, vec!["0", "2", "3", "4", "1"]);
}

#[test]
fn set_keeps_the_same_type_distinctions_as_a_map() {
    let mut set = SetData::new();
    for v in [
        Value::Int(1),
        Value::Real(1.0),
        s("1"),
        Value::Bool(true),
        Value::Null,
        Value::BigInt(Rc::new(big_from_decimal("1"))),
    ] {
        assert!(set.insert(v));
    }
    assert_eq!(set.len(), 6);
    assert!(!set.insert(Value::Int(1)));
    assert_eq!(set.len(), 6);
}

#[test]
fn a_composite_key_is_canonicalised_by_content() {
    // Two independently built but structurally equal arrays are one
    // key (upstream canonicalises composite keys by their rendered
    // form, not by identity).
    let a = Value::Array(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
    let b = Value::Array(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
    let c = Value::Array(Rc::new(RefCell::new(vec![Value::Int(1)])));
    let mut m = MapData::new();
    m.insert(a, Value::Int(1));
    m.insert(b.clone(), Value::Int(2));
    m.insert(c, Value::Int(3));
    assert_eq!(m.len(), 2);
    assert!(matches!(m.get(&b), Some(Value::Int(2))));

    // Mutating the array changes which key it canonicalises to —
    // same as before the typed-key change, since both forms render
    // the contents.
    if let Value::Array(inner) = &b {
        inner.borrow_mut().pop();
    }
    assert!(matches!(m.get(&b), Some(Value::Int(3))));
}
