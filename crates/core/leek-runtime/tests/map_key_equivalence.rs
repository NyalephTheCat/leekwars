//! `MapKey` canonicalisation (RT-02 #239, RT-01 #267).
//!
//! Maps and sets key on [`MapKey`], and the relation it defines has
//! two halves that need different oracles.
//!
//! For **primitives** it is the same relation the canonical string
//! [`key_repr`] builds, so the two must agree *exactly*: for every
//! pair of primitive values, `MapKey::of(a) == MapKey::of(b)` iff
//! `key_repr(a) == key_repr(b)`. A single disagreement silently
//! merges or splits map entries, and the damage is data-dependent
//! rather than a compile error — hence the exhaustive pairwise sweep
//! over an adversarial corpus, with `key_repr` kept as the oracle.
//!
//! For **composites** there is no string oracle any more, and that is
//! the fix rather than a gap: upstream keys arrays, maps, sets,
//! objects, instances, intervals and functions by object identity
//! (`ArrayLeekValue.java:1093-1101`), so two structurally-equal
//! arrays are two keys while `key_repr` renders them the same. Those
//! are asserted directly below.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use leek_runtime::{MapKey, Value, big_from_decimal, key_repr};

fn s(t: &str) -> Value {
    Value::String(Rc::new(t.to_string()))
}

fn arr(items: Vec<Value>) -> Value {
    Value::Array(Rc::new(RefCell::new(items)))
}

/// Primitive values chosen to attack every way the two
/// canonicalisations could drift apart: each primitive arm's boundary
/// values, strings that mimic another arm's `key_repr` prefix,
/// `big_integer` against the same integer, NaN payloads and signed
/// zero.
fn primitive_corpus() -> Vec<(&'static str, Value)> {
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
    ]
}

#[test]
fn map_key_matches_key_repr_on_every_primitive_pair() {
    let corpus = primitive_corpus();
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
    let corpus = primitive_corpus();
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

#[test]
fn structurally_equal_composites_are_two_keys() {
    // The half `key_repr` can no longer arbitrate: upstream's
    // `equals` on a composite is `object == this`, so two arrays that
    // render identically are still two keys.
    let a = arr(vec![Value::Int(1), Value::Int(2)]);
    let b = arr(vec![Value::Int(1), Value::Int(2)]);
    assert_eq!(key_repr(&a), key_repr(&b), "the two should render alike");
    assert_ne!(MapKey::of(&a), MapKey::of(&b));

    // …and the same `Rc`, however many `Value` handles point at it,
    // is one key.
    let alias = a.clone();
    assert_eq!(MapKey::of(&a), MapKey::of(&alias));

    // Which also means the key survives a mutation of the contents,
    // where a rendered key would have moved.
    let before_key = MapKey::of(&a);
    let before_text = key_repr(&a);
    if let Value::Array(inner) = &a {
        inner.borrow_mut().push(Value::Int(3));
    }
    assert_ne!(
        before_text,
        key_repr(&a),
        "the mutation should have moved the rendered form"
    );
    assert_eq!(before_key, MapKey::of(&a));
    assert_eq!(MapKey::of(&a), MapKey::of(&alias));
}

#[test]
fn a_cell_keys_as_the_value_inside() {
    // Cells are pure storage, not a value the language hands out, so
    // they carry no identity of their own.
    let cell = Value::Cell(Rc::new(RefCell::new(Value::Int(5))));
    assert_eq!(MapKey::of(&cell), MapKey::of(&Value::Int(5)));
    assert_ne!(MapKey::of(&cell), MapKey::of(&s("5")));

    let null_cell = Value::Cell(Rc::new(RefCell::new(Value::Null)));
    assert_eq!(MapKey::of(&null_cell), MapKey::of(&Value::Null));

    // A cell wrapping a composite peels to that composite's identity,
    // not to a second one.
    let a = arr(vec![Value::Int(1)]);
    let boxed = Value::Cell(Rc::new(RefCell::new(a.clone())));
    assert_eq!(MapKey::of(&boxed), MapKey::of(&a));
}

#[test]
fn super_keys_as_its_receiver() {
    let receiver = arr(vec![Value::Int(1)]);
    let sup = Value::Super(Box::new(leek_runtime::SuperValue {
        parent_class: "A".to_string(),
        receiver: Rc::new(receiver.clone()),
    }));
    assert_eq!(MapKey::of(&sup), MapKey::of(&receiver));
}

#[test]
fn class_references_key_per_class_not_per_value() {
    // A `ClassRef` is rebuilt at every evaluation site, while
    // upstream has one `ClassLeekValue` per class — so the id, not
    // the `Rc`, is the identity.
    let id = leek_runtime::ClassId(7);
    let a = Value::ClassRef(id, Rc::new("A".to_string()));
    let b = Value::ClassRef(id, Rc::new("A".to_string()));
    assert_eq!(MapKey::of(&a), MapKey::of(&b));
    assert_ne!(
        MapKey::of(&a),
        MapKey::of(&Value::ClassRef(
            leek_runtime::ClassId(8),
            Rc::new("B".to_string())
        ))
    );

    assert_eq!(
        MapKey::of(&Value::BuiltinClass("Array")),
        MapKey::of(&Value::BuiltinClass("Array"))
    );
    assert_ne!(
        MapKey::of(&Value::BuiltinClass("Array")),
        MapKey::of(&Value::BuiltinClass("Map"))
    );
    // A class reference never collides with the string of its name.
    assert_ne!(MapKey::of(&a), MapKey::of(&s("A")));
}
