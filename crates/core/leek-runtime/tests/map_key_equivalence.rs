//! `MapKey` vs `key_repr` equivalence (RT-02, #239).
//!
//! Maps and sets key on [`MapKey`] instead of on the canonical string
//! [`key_repr`] builds, so the two must agree *exactly*: for every
//! pair of values, `MapKey::of(a) == MapKey::of(b)` iff
//! `key_repr(a) == key_repr(b)`. A single disagreement silently
//! merges or splits map entries, and the damage is data-dependent
//! rather than a compile error — hence the exhaustive pairwise sweep
//! over an adversarial corpus, with `key_repr` kept as the oracle.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use leek_runtime::{
    Function, IntervalValue, MapData, MapKey, ObjectData, SetData, SuperValue, Value,
    big_from_decimal, key_repr,
};

fn s(t: &str) -> Value {
    Value::String(Rc::new(t.to_string()))
}

fn arr(items: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(items)))
}

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

/// Values chosen to attack every way the two canonicalisations could
/// drift apart: each primitive arm's boundary values, strings that
/// mimic another arm's `key_repr` prefix, `big_integer` against the
/// same integer, NaN payloads, signed zero, two structurally-equal
/// composites built independently, and the cell/`super` wrappers
/// whose `Display` peels to the value inside.
fn corpus() -> Vec<(&'static str, Value)> {
    let mut m = MapData::new();
    m.insert(Value::Int(1), s("a"));

    let mut set = SetData::new();
    set.insert(Value::Int(1));

    let mut obj = ObjectData::new();
    obj.set("a", Value::Int(1));

    vec![
        ("null", Value::Null),
        ("true", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("int 0", Value::Int(0)),
        ("int 1", Value::Int(1)),
        ("int -1", Value::Int(-1)),
        ("int 5", Value::Int(5)),
        ("int MIN", Value::Int(i64::MIN)),
        ("int MAX", Value::Int(i64::MAX)),
        ("real 0.0", Value::Real(0.0)),
        ("real -0.0", Value::Real(-0.0)),
        ("real 1.0", Value::Real(1.0)),
        ("real 0.5", Value::Real(0.5)),
        ("real 5.0", Value::Real(5.0)),
        ("real 1e300", Value::Real(1e300)),
        ("real NaN", Value::Real(f64::NAN)),
        ("real -NaN", Value::Real(-f64::NAN)),
        // A NaN with a non-default payload: distinct bits, same
        // `key_repr` ("r:NaN"), so it must be the same key.
        (
            "real NaN payload",
            Value::Real(f64::from_bits(0x7ff8_0000_0000_0001)),
        ),
        ("real inf", Value::Real(f64::INFINITY)),
        ("real -inf", Value::Real(f64::NEG_INFINITY)),
        ("str empty", s("")),
        ("str 1", s("1")),
        ("str 5", s("5")),
        // Prefix-collision adversaries — a string spelled like
        // another arm's canonical form.
        ("str i:5", s("i:5")),
        ("str r:0", s("r:0")),
        ("str b:true", s("b:true")),
        ("str s:x", s("s:x")),
        ("str I:5", s("I:5")),
        ("str null", s("null")),
        ("str true", s("true")),
        ("str accent", s("é")),
        ("big 0", Value::BigInt(Rc::new(big_from_decimal("0")))),
        ("big 5", Value::BigInt(Rc::new(big_from_decimal("5")))),
        // Same numeric value, independently built: one key.
        ("big 5 again", Value::BigInt(Rc::new(big_from_decimal("5")))),
        (
            "big huge",
            Value::BigInt(Rc::new(big_from_decimal("123456789012345678901234567890"))),
        ),
        ("array empty", arr(vec![])),
        ("array [1,2]", arr(vec![Value::Int(1), Value::Int(2)])),
        // Structurally equal but a different `Rc`: still one key,
        // because canonicalisation is by rendered content.
        ("array [1,2] again", arr(vec![Value::Int(1), Value::Int(2)])),
        ("array [1]", arr(vec![Value::Int(1)])),
        ("map {1:a}", Value::Map(Rc::new(RefCell::new(m)))),
        ("set {1}", Value::Set(Rc::new(RefCell::new(set)))),
        ("object {a:1}", Value::Object(Rc::new(RefCell::new(obj)))),
        ("interval 1..2", interval(1.0, 2.0)),
        ("interval 1..3", interval(1.0, 3.0)),
        ("builtin class Array", Value::BuiltinClass("Array")),
        ("builtin class Map", Value::BuiltinClass("Map")),
        (
            "function sum",
            Value::Function(Function::Builtin("sum".to_string())),
        ),
        // Cells and `super` render as the value inside, so they key
        // as that value's *bare* form — `Cell(5)` is "5", not "i:5".
        ("cell null", Value::Cell(Rc::new(RefCell::new(Value::Null)))),
        ("cell 5", Value::Cell(Rc::new(RefCell::new(Value::Int(5))))),
        (
            "cell 5.0",
            Value::Cell(Rc::new(RefCell::new(Value::Real(5.0)))),
        ),
        ("cell \"x\"", Value::Cell(Rc::new(RefCell::new(s("x"))))),
        (
            "super null",
            Value::Super(Box::new(SuperValue {
                parent_class: "A".to_string(),
                receiver: Rc::new(Value::Null),
            })),
        ),
        (
            "super 5",
            Value::Super(Box::new(SuperValue {
                parent_class: "A".to_string(),
                receiver: Rc::new(Value::Int(5)),
            })),
        ),
    ]
}

#[test]
fn map_key_matches_key_repr_on_every_pair() {
    let corpus = corpus();
    for (na, a) in &corpus {
        for (nb, b) in &corpus {
            let typed = MapKey::of(a) == MapKey::of(b);
            let string = key_repr(a) == key_repr(b);
            assert_eq!(
                typed,
                string,
                "{na} vs {nb}: MapKey says {typed}, key_repr says {string} \
                 ({:?} / {:?} vs {:?} / {:?})",
                MapKey::of(a),
                key_repr(a),
                MapKey::of(b),
                key_repr(b),
            );
        }
    }
}

#[test]
fn map_key_hashes_agree_with_key_repr_classes() {
    // `HashMap` needs `Hash` to be consistent with `Eq`: equal keys
    // must land in the same bucket. Checked by round-tripping the
    // whole corpus through a `HashSet` and comparing the number of
    // distinct buckets against the number of distinct `key_repr`
    // strings.
    let corpus = corpus();
    let typed: HashSet<MapKey> = corpus.iter().map(|(_, v)| MapKey::of(v)).collect();
    let strings: HashSet<String> = corpus.iter().map(|(_, v)| key_repr(v)).collect();
    assert_eq!(typed.len(), strings.len());

    // And every value must find its own class back through a hash
    // lookup, not merely compare equal.
    let mut by_key: HashMap<MapKey, String> = HashMap::new();
    for (_, v) in &corpus {
        by_key.insert(MapKey::of(v), key_repr(v));
    }
    for (name, v) in &corpus {
        assert_eq!(
            by_key.get(&MapKey::of(v)).map(String::as_str),
            Some(key_repr(v).as_str()),
            "{name} did not hash back to its own key class"
        );
    }
}

#[test]
fn known_upstream_distinctions_survive() {
    // The identities upstream depends on, spelled out so a future
    // refactor of `MapKey` trips on the specific pairs rather than on
    // a corpus diff.
    let distinct = |a: Value, b: Value| {
        assert_ne!(MapKey::of(&a), MapKey::of(&b), "{a:?} vs {b:?}");
    };
    // `5`, `5.0`, `"5"`, `5L` and `true` are five different keys.
    distinct(Value::Int(5), Value::Real(5.0));
    distinct(Value::Int(5), s("5"));
    distinct(Value::Int(5), Value::BigInt(Rc::new(big_from_decimal("5"))));
    distinct(Value::Int(1), Value::Bool(true));
    distinct(Value::Null, s("null"));
    // Signed zero stays split; every NaN collapses to one key.
    distinct(Value::Real(0.0), Value::Real(-0.0));
    assert_eq!(
        MapKey::of(&Value::Real(f64::NAN)),
        MapKey::of(&Value::Real(-f64::NAN))
    );
    assert_eq!(
        MapKey::of(&Value::Real(f64::NAN)),
        MapKey::of(&Value::Real(f64::from_bits(0x7ff8_0000_0000_0001)))
    );
    // A string spelled like a canonical prefix is still a string.
    distinct(s("i:5"), Value::Int(5));
    distinct(s("b:true"), Value::Bool(true));
}
