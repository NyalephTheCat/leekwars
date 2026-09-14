//! `slice` and `set_index` — the two `eval.rs` entry points every backend
//! shares and nothing tested (#188).
//!
//! `set_index` is the interesting one: in v1–v3 an out-of-range write *morphs*
//! a dense array into a sparse map and returns the replacement for the
//! caller's slot, while v4 drops the write. Every backend has to honour the
//! returned value — native does it through `Statement::ApplyPromotion` — so
//! the promotion rules are pinned here at each version byte rather than
//! rediscovered per backend.

use std::cell::RefCell;
use std::rc::Rc;

use leek_runtime::{IntervalValue, MapData, Value, set_index, slice};

fn arr(items: &[i64]) -> Value {
    Value::Array(Rc::new(RefCell::new(
        items.iter().copied().map(Value::Int).collect(),
    )))
}

fn ints(v: &Value) -> Vec<i64> {
    match v {
        Value::Array(a) => a
            .borrow()
            .iter()
            .map(|e| e.as_int().unwrap_or(-1))
            .collect(),
        other => panic!("expected an array, got {other:?}"),
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.as_ref().clone(),
        other => panic!("expected a string, got {other:?}"),
    }
}

/// `[a..b]`, integer-typed and inclusive on both ends.
fn interval(a: f64, b: f64) -> Value {
    Value::Interval(Rc::new(IntervalValue {
        start: Some(a),
        end: Some(b),
        start_inclusive: true,
        end_inclusive: true,
        integer_typed: true,
        start_is_int: true,
        end_is_int: true,
        start_forces_real: false,
        end_forces_real: false,
    }))
}

// ---- slice ----

#[test]
fn array_slices_take_python_style_bounds() {
    let a = arr(&[0, 1, 2, 3, 4]);
    assert_eq!(ints(&slice(&a, Some(1), Some(3), None)), vec![1, 2]);
    // An omitted bound is the end of the sequence, in whichever direction the
    // step runs.
    assert_eq!(ints(&slice(&a, None, None, None)), vec![0, 1, 2, 3, 4]);
    assert_eq!(ints(&slice(&a, Some(2), None, None)), vec![2, 3, 4]);
    assert_eq!(ints(&slice(&a, None, Some(2), None)), vec![0, 1]);
}

#[test]
fn negative_slice_bounds_count_from_the_end() {
    let a = arr(&[0, 1, 2, 3, 4]);
    assert_eq!(ints(&slice(&a, Some(-2), None, None)), vec![3, 4]);
    assert_eq!(ints(&slice(&a, Some(1), Some(-1), None)), vec![1, 2, 3]);
    // Past the start, a negative bound clamps rather than wrapping twice.
    assert_eq!(
        ints(&slice(&a, Some(-99), Some(2), None)),
        vec![0, 1],
        "an over-negative start clamps to 0"
    );
}

#[test]
fn out_of_range_slice_bounds_clamp_instead_of_panicking() {
    let a = arr(&[0, 1, 2]);
    assert_eq!(ints(&slice(&a, Some(1), Some(99), None)), vec![1, 2]);
    assert!(ints(&slice(&a, Some(5), Some(9), None)).is_empty());
    // An inverted range is empty, not reversed: direction comes from `step`.
    assert!(ints(&slice(&a, Some(2), Some(1), None)).is_empty());
}

#[test]
fn a_step_selects_every_nth_and_a_negative_step_walks_backwards() {
    let a = arr(&[0, 1, 2, 3, 4, 5]);
    assert_eq!(ints(&slice(&a, None, None, Some(2.0))), vec![0, 2, 4]);
    assert_eq!(
        ints(&slice(&a, None, None, Some(-1.0))),
        vec![5, 4, 3, 2, 1, 0]
    );
    assert_eq!(ints(&slice(&a, Some(4), Some(1), Some(-2.0))), vec![4, 2]);
    // A zero step would never terminate; arrays answer with an empty slice.
    assert!(ints(&slice(&a, None, None, Some(0.0))).is_empty());
}

#[test]
fn string_slices_index_by_utf16_code_unit() {
    let ascii = Value::String(Rc::new("abcdef".to_string()));
    assert_eq!(text(&slice(&ascii, Some(1), Some(4), None)), "bcd");
    assert_eq!(text(&slice(&ascii, Some(-2), None, None)), "ef");
    assert_eq!(text(&slice(&ascii, None, None, Some(2.0))), "ace");

    // Non-ASCII takes the UTF-16 path (`String.charAt` upstream), so a BMP
    // character is one unit and an astral one is two.
    let accented = Value::String(Rc::new("héllo".to_string()));
    assert_eq!(text(&slice(&accented, Some(1), Some(3), None)), "él");
    let astral = Value::String(Rc::new("a😀b".to_string()));
    assert_eq!(
        text(&slice(&astral, Some(0), Some(2), None))
            .chars()
            .count(),
        2,
        "the surrogate pair is two code units, so [0:2] cuts it in half"
    );

    // Unlike arrays, a zero stride on a string is treated as 1 (upstream
    // `stringSlice`).
    assert_eq!(text(&slice(&ascii, Some(1), Some(3), Some(0.0))), "bc");
}

#[test]
fn interval_slices_materialize_the_selected_steps() {
    let iv = interval(1.0, 5.0);
    let all = slice(&iv, None, None, None);
    assert_eq!(
        all.to_string(),
        "[1.0, 2.0, 3.0, 4.0, 5.0]",
        "an interval slices into an array of reals"
    );
    assert_eq!(slice(&iv, Some(1), Some(3), None).to_string(), "[2.0, 3.0]");
    // A negative step walks down from the interval's upper bound.
    assert_eq!(
        slice(&iv, Some(0), Some(2), Some(-1.0)).to_string(),
        "[5.0, 4.0]"
    );
    // An unbounded interval has no first element to count from.
    let unbounded = Value::Interval(Rc::new(IntervalValue {
        start: None,
        ..match interval(0.0, 1.0) {
            Value::Interval(iv) => (*iv).clone(),
            _ => unreachable!(),
        }
    }));
    assert!(matches!(slice(&unbounded, None, None, None), Value::Null));
}

#[test]
fn slicing_anything_else_is_null() {
    assert!(matches!(
        slice(&Value::Int(7), Some(0), Some(1), None),
        Value::Null
    ));
    assert!(matches!(slice(&Value::Null, None, None, None), Value::Null));
}

// ---- set_index ----

#[test]
fn an_in_range_array_write_mutates_in_place_at_every_version() {
    for version in 1..=4 {
        let a = arr(&[1, 2, 3]);
        assert!(set_index(&a, &Value::Int(1), Value::Int(9), version).is_none());
        assert_eq!(ints(&a), vec![1, 9, 3], "v{version}");
        // A negative index that lands inside the array writes there.
        assert!(set_index(&a, &Value::Int(-1), Value::Int(7), version).is_none());
        assert_eq!(ints(&a), vec![1, 9, 7], "v{version}");
    }
}

#[test]
fn writing_one_past_the_end_appends_in_v1_v3_and_is_dropped_in_v4() {
    for version in 1..=3 {
        let a = arr(&[1, 2]);
        assert!(set_index(&a, &Value::Int(2), Value::Int(3), version).is_none());
        assert_eq!(ints(&a), vec![1, 2, 3], "v{version} appends");
    }
    let a = arr(&[1, 2]);
    assert!(set_index(&a, &Value::Int(2), Value::Int(3), 4).is_none());
    assert_eq!(ints(&a), vec![1, 2], "v4 drops the write");
}

#[test]
fn a_gap_write_promotes_a_v1_v3_array_to_a_sparse_map() {
    for version in 1..=3 {
        let a = arr(&[1, 2]);
        let promoted = set_index(&a, &Value::Int(5), Value::Int(9), version)
            .expect("v1-v3 promote rather than drop the write");
        // The dense elements keep their integer keys and the written index
        // becomes another key — this is the value the caller must store back.
        match &promoted {
            Value::Map(m) => {
                let m = m.borrow();
                assert_eq!(m.len(), 3, "v{version}: two elements plus the new key");
                assert_eq!(m.get(&Value::Int(5)).and_then(Value::as_int), Some(9),);
            }
            other => panic!("v{version}: expected a map, got {other:?}"),
        }
        // …and the original array is left alone, so a caller that ignores the
        // return value silently loses the write.
        assert_eq!(ints(&a), vec![1, 2], "v{version}");
    }
}

#[test]
fn a_negative_out_of_range_write_promotes_in_v1_v3_and_is_dropped_in_v4() {
    for version in 1..=3 {
        let a = arr(&[1, 2]);
        let promoted = set_index(&a, &Value::Int(-5), Value::Int(9), version)
            .expect("v1-v3 promote rather than drop the write");
        match &promoted {
            // The *raw* index is the key, not the wrapped one.
            Value::Map(m) => assert_eq!(
                m.borrow().get(&Value::Int(-5)).and_then(Value::as_int),
                Some(9),
                "v{version}",
            ),
            other => panic!("v{version}: expected a map, got {other:?}"),
        }
    }
    let a = arr(&[1, 2]);
    assert!(set_index(&a, &Value::Int(-5), Value::Int(9), 4).is_none());
    assert_eq!(ints(&a), vec![1, 2], "v4 drops the write");
}

#[test]
fn a_map_write_uses_legacy_keys_in_v1_v3_and_the_value_itself_in_v4() {
    let key = arr(&[1, 2, 3]);
    // v1-v3 collapse a collection key to its size (`transformKey`), so an
    // array key and the integer 3 are the same slot.
    let m = Value::Map(Rc::new(RefCell::new(MapData::new())));
    assert!(set_index(&m, &key, Value::Int(1), 3).is_none());
    match &m {
        Value::Map(m) => assert_eq!(
            m.borrow().get(&Value::Int(3)).and_then(Value::as_int),
            Some(1),
            "the array key collapsed to its size",
        ),
        _ => unreachable!(),
    }
    // v4 keys by the value itself, so the integer slot stays empty.
    let m4 = Value::Map(Rc::new(RefCell::new(MapData::new())));
    assert!(set_index(&m4, &key, Value::Int(1), 4).is_none());
    match &m4 {
        Value::Map(m) => {
            assert!(m.borrow().get(&Value::Int(3)).is_none());
            assert_eq!(m.borrow().len(), 1);
        }
        _ => unreachable!(),
    }
}

#[test]
fn a_v1_map_write_stores_a_deep_copy() {
    let stored = arr(&[1, 2]);
    let m = Value::Map(Rc::new(RefCell::new(MapData::new())));
    set_index(&m, &Value::Int(0), stored.clone(), 1);
    // v1 has copy-on-assign semantics: mutating the source afterwards must
    // not be visible through the map.
    if let Value::Array(a) = &stored {
        a.borrow_mut().push(Value::Int(3));
    }
    match &m {
        Value::Map(m) => match m.borrow().get(&Value::Int(0)) {
            Some(Value::Array(a)) => assert_eq!(a.borrow().len(), 2, "v1 stored a deep copy"),
            other => panic!("expected an array, got {other:?}"),
        },
        _ => unreachable!(),
    }
}

#[test]
fn writing_an_index_on_a_scalar_does_nothing() {
    assert!(set_index(&Value::Int(1), &Value::Int(0), Value::Int(2), 4).is_none());
    assert!(set_index(&Value::Null, &Value::Int(0), Value::Int(2), 1).is_none());
}
