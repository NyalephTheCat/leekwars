//! `hash` / `hashCode` are a local extension, and this pins their shape
//! (RT-01 #267, #335).
//!
//! Neither name exists upstream: `LeekFunctions.java` is the generator's
//! whole function table and declares no `hash`, and no other file in the
//! generator tree mentions one either — the `hashCode`s in that source are
//! Java's own `Object.hashCode` overrides on the value classes, which the
//! language never hands out. So there is no reference implementation to
//! match and no oracle to consult: whatever these return is a decision this
//! crate made, and the only way it stays a decision rather than an accident
//! is for the exact numbers to be written down.
//!
//! What is pinned: a djb2 digest (`h = h * 33 + c` from 5381, wrapped into
//! the signed `integer` domain) over the value's `Display` rendering — which
//! is the language's `string()` form, *including* the quotes a string gets.

use leek_runtime::{
    BuiltinError, BuiltinHost, BuiltinResult, Value, call_builtin, is_known_builtin,
};
use std::rc::Rc;

/// `hash` needs none of the host's capabilities, so every method is a stub.
/// `call_value` reports an error rather than returning a value: nothing
/// should ever reach it, and a silent `null` would hide a misdispatch.
struct NoHost;

impl BuiltinHost for NoHost {
    fn version(&self) -> u8 {
        4
    }
    fn rng_int(&mut self, lo: i64, _hi: i64) -> i64 {
        lo
    }
    fn rng_real(&mut self, lo: f64, _hi: f64) -> f64 {
        lo
    }
    fn callback_arity(&self, _callee: &Value) -> Option<usize> {
        None
    }
    fn param_byref_mask(&self, _callee: &Value) -> Option<Vec<bool>> {
        None
    }
    fn call_value(&mut self, _callee: &Value, _args: Vec<Value>) -> BuiltinResult {
        Err(BuiltinError::new("hash never calls back"))
    }
}

fn hash_of(name: &str, v: Value) -> i64 {
    assert!(is_known_builtin(name), "{name} is not a dispatched builtin");
    match call_builtin(&mut NoHost, name, &[v]) {
        Ok(Value::Int(i)) => i,
        other => panic!("{name} returned {other:?}, expected an integer"),
    }
}

fn s(t: &str) -> Value {
    Value::String(Rc::new(t.to_string()))
}

#[test]
fn hash_pins_two_distinct_stable_values() {
    // `"a"` renders as `"a"` — three characters, the quotes included — so
    // this is djb2 over 34, 97, 34.
    assert_eq!(hash_of("hash", s("a")), 193_417_258);
    // `97` renders as the two digits `9` and `7`, so it is djb2 over 57, 55.
    assert_eq!(hash_of("hash", Value::Int(97)), 5_861_845);
    // 97 is the ASCII code point of `a`; digesting the rendered form
    // keeps the two apart anyway.
    assert_ne!(
        hash_of("hash", s("a")),
        hash_of("hash", Value::Int(97)),
        "a string and its code point must not collide"
    );
}

#[test]
fn hash_digests_the_quotes_around_a_string() {
    // The empty string is not the empty digest: the two quote characters
    // are in the input, so the seed has been stirred twice.
    assert_eq!(hash_of("hash", s("")), 5_861_065);
    assert_ne!(
        hash_of("hash", s("")),
        5381,
        "the seed must not leak through"
    );
}

#[test]
fn hash_code_is_the_same_function_under_a_second_name() {
    for v in [
        s("a"),
        s(""),
        Value::Int(97),
        Value::Null,
        Value::Bool(true),
    ] {
        assert_eq!(
            hash_of("hash", v.clone()),
            hash_of("hashCode", v.clone()),
            "the two names disagreed on {v:?}"
        );
    }
}
