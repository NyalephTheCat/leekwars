//! Map/set key semantics at the collection level (RT-02 #239,
//! RT-01 #267).
//!
//! `tests/map_key_equivalence.rs` pins the canonicalisation itself;
//! this pins what that means for `MapData` and `SetData` — how many
//! entries a mixed-type key set produces, that insertion order
//! survives a removal now that removal no longer rebuilds the index
//! from scratch, and that composite keys behave like the object
//! identities upstream keys them by: distinct lambdas and distinct
//! instances stay distinct, and a key that is mutated (or read back
//! under a different `DISPLAY_VERSION`) still finds its entry.

use std::cell::RefCell;
use std::rc::Rc;

use leek_runtime::{
    ClassId, DISPLAY_VERSION, Function, Instance, LambdaCapture, MapData, ObjectData, SetData,
    Value, big_from_decimal,
};

fn s(t: &str) -> Value {
    Value::String(Rc::new(t.to_string()))
}

fn arr(items: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(items)))
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
fn a_composite_key_is_keyed_by_identity() {
    // Two independently built but structurally equal arrays are two
    // keys — upstream's `ArrayLeekValue.equals` is `object == this`
    // (`ArrayLeekValue.java:1093-1101`).
    let a = arr(vec![Value::Int(1), Value::Int(2)]);
    let b = arr(vec![Value::Int(1), Value::Int(2)]);
    let c = arr(vec![Value::Int(1)]);
    let mut m = MapData::new();
    m.insert(a.clone(), Value::Int(1));
    m.insert(b.clone(), Value::Int(2));
    m.insert(c.clone(), Value::Int(3));
    assert_eq!(m.len(), 3);
    assert!(matches!(m.get(&a), Some(Value::Int(1))));
    assert!(matches!(m.get(&b), Some(Value::Int(2))));
    assert!(matches!(m.get(&c), Some(Value::Int(3))));

    // A second handle on the same `Rc` is the same key.
    assert!(matches!(m.get(&b.clone()), Some(Value::Int(2))));

    // Writing through that handle overwrites rather than adding.
    m.insert(b.clone(), Value::Int(20));
    assert_eq!(m.len(), 3);
    assert!(matches!(m.get(&b), Some(Value::Int(20))));
}

#[test]
fn mutating_a_key_array_does_not_strand_its_entry() {
    // The stale-key bug: with a rendered key, `m[k]` after `push`
    // looked up a string nothing in the index spelled any more, so
    // the entry became unreachable and a re-insert grew the map.
    let k = arr(vec![Value::Int(1), Value::Int(2)]);
    let mut m = MapData::new();
    m.insert(k.clone(), s("payload"));

    if let Value::Array(inner) = &k {
        inner.borrow_mut().push(Value::Int(3));
        inner.borrow_mut().remove(0);
    }

    assert_eq!(m.len(), 1);
    assert_eq!(
        m.get(&k).map(ToString::to_string).as_deref(),
        Some("\"payload\"")
    );
    // …and the entry's own stored key is that same, now-mutated
    // array, so `reindex` lands it in the same place.
    m.reindex();
    assert_eq!(
        m.get(&k).map(ToString::to_string).as_deref(),
        Some("\"payload\"")
    );
    m.insert(k.clone(), s("second"));
    assert_eq!(m.len(), 1);
}

#[test]
fn two_lambdas_are_two_set_elements() {
    // Every anonymous function stringifies to `#Anonymous Function`,
    // so a rendered key collapsed a whole program's lambdas into one
    // entry. Identity keeps them apart.
    let lambda = |idx: usize| {
        Value::Function(Function::Lambda(Rc::new(LambdaCapture {
            function_idx: idx,
            captured: RefCell::new(vec![]),
        })))
    };
    let a = lambda(0);
    let b = lambda(0);
    assert_eq!(
        a.to_string(),
        b.to_string(),
        "the two should still render alike"
    );

    let mut set = SetData::new();
    assert!(set.insert(a.clone()));
    assert!(set.insert(b.clone()));
    assert_eq!(set.len(), 2);
    // The same handle is not a third element.
    assert!(!set.insert(a.clone()));
    assert_eq!(set.len(), 2);
    assert!(set.contains(&a));
    assert!(set.contains(&b));

    // A bound method keys on the method *and* the receiver.
    let r1 = arr(vec![]);
    let r2 = arr(vec![]);
    let bound = |idx: usize, recv: &Value| {
        Value::Function(Function::BoundMethod {
            function_idx: idx,
            receiver: Box::new(recv.clone()),
        })
    };
    let mut set = SetData::new();
    assert!(set.insert(bound(3, &r1)));
    assert!(!set.insert(bound(3, &r1)));
    assert!(set.insert(bound(3, &r2)));
    assert!(set.insert(bound(4, &r1)));
    assert_eq!(set.len(), 3);
}

#[test]
fn two_instances_of_one_class_are_two_map_entries() {
    // `==` on instances is `Rc::ptr_eq`, so a map that merged two
    // equal-field instances disagreed with the language itself.
    let instance = || {
        let mut fields = ObjectData::new();
        fields.set("x", Value::Int(1));
        Value::Instance(Rc::new(RefCell::new(Instance {
            class: ClassId(1),
            class_name: "A".to_string(),
            fields,
        })))
    };
    let a = instance();
    let b = instance();
    assert_eq!(
        a.to_string(),
        b.to_string(),
        "the two should still render alike"
    );

    let mut m = MapData::new();
    m.insert(a.clone(), s("first"));
    m.insert(b.clone(), s("second"));
    assert_eq!(m.len(), 2);
    assert_eq!(
        m.get(&a).map(ToString::to_string).as_deref(),
        Some("\"first\"")
    );
    assert_eq!(
        m.get(&b).map(ToString::to_string).as_deref(),
        Some("\"second\"")
    );

    // Mutating one instance's fields moves neither entry.
    if let Value::Instance(inst) = &a {
        inst.borrow_mut().fields.set("x", Value::Int(99));
    }
    assert_eq!(m.len(), 2);
    assert_eq!(
        m.get(&a).map(ToString::to_string).as_deref(),
        Some("\"first\"")
    );
}

#[test]
fn a_composite_key_survives_a_display_version_change() {
    // Rendered keys went through the `DISPLAY_VERSION` thread-local,
    // so an entry written under one language version could not be
    // read back under another. Identity keys never consult it.
    let k = arr(vec![Value::Real(1234.5)]);
    let mut m = MapData::new();

    DISPLAY_VERSION.with(|v| v.set(1));
    m.insert(k.clone(), s("written under v1"));
    let v1_text = k.to_string();
    assert!(m.contains_key(&k));

    DISPLAY_VERSION.with(|v| v.set(4));
    let v4_text = k.to_string();
    assert_ne!(
        v1_text, v4_text,
        "pick a key whose rendering actually depends on the version"
    );
    assert_eq!(m.len(), 1);
    assert_eq!(
        m.get(&k).map(ToString::to_string).as_deref(),
        Some("\"written under v1\"")
    );
    m.insert(k.clone(), s("written under v4"));
    assert_eq!(m.len(), 1);

    DISPLAY_VERSION.with(|v| v.set(1));
    assert_eq!(
        m.get(&k).map(ToString::to_string).as_deref(),
        Some("\"written under v4\"")
    );

    DISPLAY_VERSION.with(|v| v.set(4));
}
