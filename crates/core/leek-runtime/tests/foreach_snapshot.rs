//! The `foreach` snapshot (#111). `make_foreach_iter` captures the iteration
//! *flat* — one `Value` per element, no `[key, value]` pair — so its accessors
//! are the whole contract its shape has. These tests pin what each source
//! kind yields, that keys stay aligned with values, and that a positional
//! source keys by position. A string is not one of the source kinds: it is
//! not iterable at all (#268).

use std::cell::RefCell;
use std::rc::Rc;

use leek_runtime::{
    IntervalValue, MapData, ObjectData, SetData, Value, foreach_key_at, foreach_len,
    foreach_value_at, make_foreach_iter,
};

/// Every (key, value) pair a snapshot of `v` yields, rendered for comparison.
fn walk(v: &Value) -> Vec<(String, String)> {
    let iter = make_foreach_iter(v);
    (0..foreach_len(&iter))
        .map(|i| {
            (
                foreach_key_at(&iter, i).to_string(),
                foreach_value_at(&iter, i).to_string(),
            )
        })
        .collect()
}

fn array(items: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(items)))
}

#[test]
fn an_array_iterates_values_keyed_by_position() {
    let a = array(vec![Value::Int(10), Value::Int(20), Value::Int(30)]);
    assert_eq!(foreach_len(&make_foreach_iter(&a)), 3);
    assert_eq!(
        walk(&a),
        [
            ("0".into(), "10".into()),
            ("1".into(), "20".into()),
            ("2".into(), "30".into())
        ]
    );
}

#[test]
fn a_map_iterates_entries_in_insertion_order_with_its_own_keys() {
    let m = Value::Map(Rc::new(RefCell::new(MapData::from_pairs(vec![
        (Value::String(Rc::new("b".into())), Value::Int(2)),
        (Value::String(Rc::new("a".into())), Value::Int(1)),
    ]))));
    assert_eq!(
        walk(&m),
        [("\"b\"".into(), "2".into()), ("\"a\"".into(), "1".into())]
    );
}

#[test]
fn a_set_iterates_elements_keyed_by_position() {
    let mut s = SetData::new();
    s.insert(Value::Int(7));
    s.insert(Value::Int(8));
    let s = Value::Set(Rc::new(RefCell::new(s)));
    assert_eq!(
        walk(&s),
        [("0".into(), "7".into()), ("1".into(), "8".into())]
    );
}

#[test]
fn an_object_iterates_fields_keyed_by_name() {
    let mut o = ObjectData::new();
    o.set("x", Value::Int(1));
    o.set("y", Value::Int(2));
    let o = Value::Object(Rc::new(RefCell::new(o)));
    assert_eq!(
        walk(&o),
        [("\"x\"".into(), "1".into()), ("\"y\"".into(), "2".into())]
    );
}

/// Upstream's `AI.isIterable` (`AI.java:1801-1807`) lists only the array /
/// map / set / interval shapes, and `ForeachBlock.java:148` skips the whole
/// walk when it says no — so a `foreach` over a string runs its body zero
/// times (#268). The non-ASCII case guards the regression specifically: the
/// old arm walked `as_bytes()`, so `"a😀b"` used to yield six mojibake
/// one-char strings.
#[test]
fn a_string_is_not_iterable() {
    for src in ["abc", "a\u{1F600}b"] {
        let s = Value::String(Rc::new(src.into()));
        assert_eq!(foreach_len(&make_foreach_iter(&s)), 0, "{src:?}");
        assert!(walk(&s).is_empty(), "{src:?}");
    }
}

#[test]
fn an_interval_walks_in_unit_steps_keyed_by_position() {
    let iv = Value::Interval(Rc::new(IntervalValue {
        start: Some(2.0),
        end: Some(5.0),
        start_inclusive: true,
        end_inclusive: true,
        integer_typed: true,
        start_is_int: true,
        end_is_int: true,
        start_forces_real: false,
        end_forces_real: false,
    }));
    assert_eq!(
        walk(&iv),
        [
            ("0".into(), "2".into()),
            ("1".into(), "3".into()),
            ("2".into(), "4".into()),
            ("3".into(), "5".into())
        ]
    );
}

#[test]
fn a_non_iterable_yields_no_iterations() {
    for v in [Value::Int(5), Value::Null, Value::Bool(true)] {
        assert_eq!(foreach_len(&make_foreach_iter(&v)), 0, "{v:?}");
    }
}

#[test]
fn reading_past_the_end_yields_null() {
    let iter = make_foreach_iter(&array(vec![Value::Int(1)]));
    assert!(matches!(foreach_value_at(&iter, 1), Value::Null));
    assert!(matches!(foreach_value_at(&iter, -1), Value::Null));
}

#[test]
fn the_snapshot_is_taken_once_and_survives_mutation_of_the_source() {
    let items = Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)]));
    let iter = make_foreach_iter(&Value::Array(Rc::clone(&items)));
    items.borrow_mut().push(Value::Int(3));
    assert_eq!(foreach_len(&iter), 2, "the snapshot must not grow");
}
