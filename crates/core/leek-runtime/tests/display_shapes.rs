//! How composite values render, at each display version (#188).
//!
//! `Display` is not cosmetic here: the corpus compares a program's rendered
//! result against upstream's, so every one of these shapes is an oracle. The
//! version-sensitive ones are the traps — a v1–v3 map with sequential integer
//! keys renders as a bare array, `string(v)` drops the quotes on nested
//! strings before v4 while `Display` keeps them at every version, and an
//! interval picks integer or real endpoints from flags rather than from its
//! stored `f64`s.
//!
//! `DISPLAY_VERSION` is a thread-local the whole process shares, so every
//! test here restores it, exactly as `numeric_edge_cases.rs` does.

use std::cell::RefCell;
use std::rc::Rc;

use leek_runtime::{
    DISPLAY_VERSION, IntervalValue, MapData, MapKey, SetData, Value, value_as_bare_string,
};

/// Render `v` at display version `version`, restoring the previous version
/// whatever happens.
fn at(version: u8, v: &Value) -> String {
    let prev = DISPLAY_VERSION.get();
    DISPLAY_VERSION.set(version);
    let out = v.to_string();
    DISPLAY_VERSION.set(prev);
    out
}

fn arr(items: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(items)))
}

fn map(entries: Vec<(Value, Value)>) -> Value {
    let mut m = MapData::new();
    for (k, v) in entries {
        m.insert_canonical(MapKey::of(&k), k, v);
    }
    Value::Map(Rc::new(RefCell::new(m)))
}

fn set(items: Vec<Value>) -> Value {
    let mut s = SetData::new();
    for it in items {
        s.insert(it);
    }
    Value::Set(Rc::new(RefCell::new(s)))
}

fn str_(s: &str) -> Value {
    Value::String(Rc::new(s.to_string()))
}

/// A bounded interval whose endpoints are both flagged integer.
fn int_interval(a: f64, b: f64) -> IntervalValue {
    IntervalValue {
        start: Some(a),
        end: Some(b),
        start_inclusive: true,
        end_inclusive: true,
        integer_typed: true,
        start_is_int: true,
        end_is_int: true,
        start_forces_real: false,
        end_forces_real: false,
    }
}

#[test]
fn an_integer_interval_renders_without_decimals_and_a_real_one_with() {
    let iv = Value::Interval(Rc::new(int_interval(1.0, 5.0)));
    assert_eq!(at(4, &iv), "[1..5]");
    assert_eq!(at(1, &iv), "[1..5]", "brackets don't move with the version");

    // One real endpoint widens *both* — an interval never renders half
    // integer, half real.
    let mixed = Value::Interval(Rc::new(IntervalValue {
        end: Some(5.5),
        end_is_int: false,
        ..int_interval(1.0, 5.5)
    }));
    assert_eq!(at(4, &mixed), "[1.0..5.5]");
}

#[test]
fn interval_brackets_follow_the_inclusive_flags() {
    let open = Value::Interval(Rc::new(IntervalValue {
        start_inclusive: false,
        end_inclusive: false,
        ..int_interval(1.0, 5.0)
    }));
    assert_eq!(at(4, &open), "]1..5[");
}

#[test]
fn an_unbounded_interval_renders_its_infinities() {
    // `]..[ ` — both ends exclusive and unbounded: the whole number line.
    let line = Value::Interval(Rc::new(IntervalValue {
        start: None,
        end: None,
        start_inclusive: false,
        end_inclusive: false,
        ..int_interval(0.0, 0.0)
    }));
    assert_eq!(at(4, &line), "]-∞..∞[");

    // `[..]` — both ends *inclusive* and unbounded is the empty interval, and
    // renders without endpoints at all.
    let empty = Value::Interval(Rc::new(IntervalValue {
        start: None,
        end: None,
        ..int_interval(0.0, 0.0)
    }));
    assert_eq!(at(4, &empty), "[..]");
}

#[test]
fn an_infinite_endpoint_forces_the_other_side_to_real_only_when_it_came_from_infinity() {
    // The `Infinity` builtin sets `*_forces_real`; the bare `∞` symbol does
    // not, and that is the whole difference between these two renderings.
    let from_builtin = Value::Interval(Rc::new(IntervalValue {
        end: Some(f64::INFINITY),
        end_is_int: false,
        end_forces_real: true,
        ..int_interval(1.0, 0.0)
    }));
    assert_eq!(at(4, &from_builtin), "[1.0..∞]");

    let from_symbol = Value::Interval(Rc::new(IntervalValue {
        end: Some(f64::INFINITY),
        end_is_int: false,
        ..int_interval(1.0, 0.0)
    }));
    assert_eq!(at(4, &from_symbol), "[1..∞]");
}

/// The quote rule lives in `string(v)`, not in `Display` — a distinction
/// worth pinning, because it is the one that decides what a *program* sees.
#[test]
fn strings_inside_a_composite_keep_their_quotes_in_display_at_every_version() {
    let a = arr(vec![str_("x"), Value::Int(1)]);
    assert_eq!(at(4, &a), "[\"x\", 1]");
    assert_eq!(at(3, &a), "[\"x\", 1]");
    assert_eq!(at(1, &a), "[\"x\", 1]");
}

#[test]
fn string_of_a_composite_drops_the_quotes_before_v4() {
    let a = arr(vec![str_("x"), Value::Int(1)]);
    let bare = |version: u8| -> String {
        let prev = DISPLAY_VERSION.get();
        DISPLAY_VERSION.set(version);
        let out = value_as_bare_string(&a);
        DISPLAY_VERSION.set(prev);
        out
    };
    assert_eq!(bare(4), "[\"x\", 1]");
    assert_eq!(bare(3), "[x, 1]");
    assert_eq!(bare(1), "[x, 1]");
}

#[test]
fn a_v1_v3_map_with_sequential_keys_renders_as_a_bare_array() {
    let sequential = map(vec![
        (Value::Int(0), Value::Int(7)),
        (Value::Int(1), Value::Int(8)),
    ]);
    assert_eq!(
        at(3, &sequential),
        "[7, 8]",
        "the LegacyArray in-order path"
    );
    assert_eq!(at(4, &sequential), "[0 : 7, 1 : 8]");

    // One key out of sequence and even v1-v3 show the keys.
    let sparse = map(vec![
        (Value::Int(0), Value::Int(7)),
        (Value::Int(5), Value::Int(8)),
    ]);
    assert_eq!(at(3, &sparse), "[0 : 7, 5 : 8]");
}

#[test]
fn an_empty_map_renders_as_a_map_only_from_v4() {
    let empty = map(vec![]);
    assert_eq!(at(4, &empty), "[:]");
    assert_eq!(at(3, &empty), "[]", "v1-v3 share the empty-array form");
    assert_eq!(at(4, &arr(vec![])), "[]");
}

#[test]
fn sets_render_in_angle_brackets_and_nest_like_everything_else() {
    assert_eq!(at(4, &set(vec![Value::Int(1), Value::Int(2)])), "<1, 2>");
    let nested = arr(vec![
        set(vec![Value::Int(1)]),
        map(vec![(str_("k"), arr(vec![Value::Int(2)]))]),
        Value::Interval(Rc::new(int_interval(0.0, 2.0))),
    ]);
    assert_eq!(at(4, &nested), "[<1>, [\"k\" : [2]], [0..2]]");
    // v1-v3 differ only in the map's own shape, not in the nesting: the keys
    // here are strings, so the LegacyArray in-order path doesn't apply.
    assert_eq!(at(3, &nested), "[<1>, [\"k\" : [2]], [0..2]]");
}

#[test]
fn a_self_referential_array_renders_once_instead_of_recursing_forever() {
    let a = arr(vec![Value::Int(1)]);
    if let Value::Array(inner) = &a {
        inner.borrow_mut().push(a.clone());
    }
    // The exact marker is the runtime's business; not looping is the
    // assertion worth making.
    let rendered = at(4, &a);
    assert!(rendered.starts_with("[1, "), "{rendered}");
    assert!(rendered.len() < 100, "cycle guard held: {rendered}");
}
