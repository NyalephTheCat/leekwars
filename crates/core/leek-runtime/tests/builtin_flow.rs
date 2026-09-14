//! The builtin error channel, at the `leek-runtime` end.
//!
//! Every higher-order builtin invokes its callback through
//! [`BuiltinHost::call_value`], which returns `Result<Value, BuiltinFlow>`.
//! When a callback reports an error the builtin must propagate it *and stop*
//! — not swallow it and keep calling back for the remaining elements. The
//! channel had zero producers for a long time (#189), so nothing exercised
//! either half; these tests pin both, so re-swallowing a `?` anywhere in
//! `builtins/{array,map,misc}.rs` turns a test red instead of going unnoticed.

use std::cell::RefCell;
use std::rc::Rc;

use leek_runtime::{
    BuiltinFlow, BuiltinHost, Function, MapData, SetData, Value, call_builtin, is_known_builtin,
};

/// A host that counts callback invocations and reports an error on the
/// `fail_at`-th one. `reply` is what a *successful* callback returns — it has
/// to keep the builtin under test looping (`arrayEvery` stops on a falsy
/// verdict, `arraySome`/`arrayFind` on a truthy one).
struct CountingHost {
    calls: usize,
    fail_at: usize,
    reply: Value,
}

impl CountingHost {
    fn new(fail_at: usize, reply: Value) -> Self {
        Self {
            calls: 0,
            fail_at,
            reply,
        }
    }
}

impl BuiltinHost for CountingHost {
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
        Some(1)
    }
    fn param_byref_mask(&self, _callee: &Value) -> Option<Vec<bool>> {
        None
    }
    fn call_value(&mut self, _callee: &Value, _args: Vec<Value>) -> Result<Value, BuiltinFlow> {
        self.calls += 1;
        if self.calls == self.fail_at {
            return Err(BuiltinFlow::Error("BOOM".into()));
        }
        Ok(self.reply.clone())
    }
}

/// Five elements, so "stopped at the third callback" is distinguishable from
/// "ran to the end".
const N: i64 = 5;
const FAIL_AT: usize = 3;

fn array_of(n: i64) -> Value {
    Value::Array(Rc::new(RefCell::new((0..n).map(Value::Int).collect())))
}

fn map_of(n: i64) -> Value {
    let mut m = MapData::new();
    for i in 0..n {
        m.insert(Value::Int(i), Value::Int(i * 10));
    }
    Value::Map(Rc::new(RefCell::new(m)))
}

fn set_of(n: i64) -> Value {
    let mut s = SetData::new();
    for i in 0..n {
        s.insert(Value::Int(i));
    }
    Value::Set(Rc::new(RefCell::new(s)))
}

/// A callback value. `arraySort` dispatches its second argument on the value
/// being callable, so a plain null would be read as a `SORT_*` mode flag.
fn callback() -> Value {
    Value::Function(Function::Builtin("abs".into()))
}

/// Run `name(args)` against a `CountingHost` that fails on the third
/// callback, and assert the error came out and the builtin stopped there.
fn assert_stops_at_first_error(name: &str, args: &[Value], reply: Value) {
    assert!(
        is_known_builtin(name),
        "{name} is not in KNOWN_BUILTIN_NAMES — the test is calling a name nothing dispatches"
    );
    let mut host = CountingHost::new(FAIL_AT, reply);
    let r = call_builtin(&mut host, name, args);
    match r {
        Err(BuiltinFlow::Error(code)) => assert_eq!(code, "BOOM", "{name} reported the wrong code"),
        Ok(v) => panic!("{name} swallowed the callback error and returned {v:?}"),
        Err(other) => panic!("{name} reported {other:?} instead of the callback's error"),
    }
    assert_eq!(
        host.calls, FAIL_AT,
        "{name} kept calling its callback after the error (ran {} of {N} elements)",
        host.calls
    );
}

#[test]
fn array_higher_order_builtins_stop_at_the_first_callback_error() {
    let f = callback();
    let falsy = Value::Bool(false);
    let truthy = Value::Bool(true);
    for (name, args, reply) in [
        ("arrayMap", vec![array_of(N), f.clone()], Value::Null),
        ("arrayFilter", vec![array_of(N), f.clone()], truthy.clone()),
        (
            "arrayFoldLeft",
            vec![array_of(N), f.clone(), Value::Int(0)],
            Value::Int(0),
        ),
        (
            "arrayFoldRight",
            vec![array_of(N), f.clone(), Value::Int(0)],
            Value::Int(0),
        ),
        ("arrayIter", vec![array_of(N), f.clone()], Value::Null),
        // `arrayEvery` short-circuits on a falsy verdict and `arraySome` /
        // `arrayFind` on a truthy one — each gets the reply that keeps it going.
        ("arrayEvery", vec![array_of(N), f.clone()], truthy.clone()),
        ("arraySome", vec![array_of(N), f.clone()], falsy.clone()),
        ("arrayFind", vec![array_of(N), f.clone()], falsy.clone()),
        ("arrayPartition", vec![array_of(N), f], truthy),
    ] {
        assert_stops_at_first_error(name, &args, reply);
    }
}

#[test]
fn array_sort_stops_at_the_first_comparator_error() {
    // The comparator sort is an O(n²) bubble sort: without the `?` reaching
    // the caller it would run every remaining comparison after the fault.
    assert_stops_at_first_error("arraySort", &[array_of(N), callback()], Value::Int(0));
}

#[test]
fn map_higher_order_builtins_stop_at_the_first_callback_error() {
    let f = callback();
    for (name, args, reply) in [
        ("mapMap", vec![map_of(N), f.clone()], Value::Null),
        ("mapFilter", vec![map_of(N), f.clone()], Value::Bool(true)),
        ("mapIter", vec![map_of(N), f.clone()], Value::Null),
        (
            "mapFold",
            vec![map_of(N), f.clone(), Value::Int(0)],
            Value::Int(0),
        ),
        ("mapEvery", vec![map_of(N), f.clone()], Value::Bool(true)),
        ("mapSome", vec![map_of(N), f.clone()], Value::Bool(false)),
    ] {
        assert_stops_at_first_error(name, &args, reply);
    }
}

#[test]
fn set_higher_order_builtins_stop_at_the_first_callback_error() {
    let f = callback();
    for (name, args, reply) in [
        ("setFilter", vec![set_of(N), f.clone()], Value::Bool(true)),
        ("setMap", vec![set_of(N), f.clone()], Value::Int(1)),
        ("setIter", vec![set_of(N), f.clone()], Value::Null),
        ("setForEach", vec![set_of(N), f.clone()], Value::Null),
    ] {
        assert_stops_at_first_error(name, &args, reply);
    }
}

/// A builtin that never calls back is untouched by the channel: no error
/// appears out of nowhere, and the result is the plain value.
#[test]
fn a_first_order_builtin_is_unaffected() {
    let mut host = CountingHost::new(1, Value::Null);
    let r = call_builtin(&mut host, "abs", &[Value::Int(-7)]);
    assert!(matches!(r, Ok(Value::Int(7))), "abs(-7) should be 7");
    assert_eq!(host.calls, 0);
}
