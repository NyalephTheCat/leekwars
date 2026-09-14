//! `trim` and `number` follow the upstream methods they implement, not the
//! Rust standard-library functions that look like them (#335, #356).
//!
//! Both builtins used to be written as the obvious one-liner and both were
//! subtly wrong for it:
//!
//! - `trim` called `str::trim`, which strips every character carrying the
//!   Unicode `White_Space` property. `StringClass.trim` is
//!   `return string.trim();`, and `java.lang.String.trim()` strips only
//!   characters `<= U+0020` — so a no-break space survives upstream and did
//!   not survive here.
//! - `number` folded `Boolean` to `0`/`1`, dropped a big integer to `0`, and
//!   answered `Real(0.0)` when a dotted string failed to parse.
//!   `ValueClass.number` does none of those: `Boolean` is neither
//!   `instanceof Number` nor `instanceof String` so it falls to the trailing
//!   `return 0l;`, `BigIntegerValue extends Number` so it is returned as it
//!   stands, and every parse failure leaves the `try` through that same
//!   `return 0l` — an **integer** zero.
//!
//! The corpus pins `number('12')` and `number('12.55')` and nothing else
//! here, so these are the rows that keep the rest honest. The end-to-end
//! twins live in `crates/testing/leek-builtin-suite/suite.toml`.

use std::rc::Rc;

use leek_runtime::{
    BuiltinHost, BuiltinResult, Value, big_from_decimal, big_full_decimal, call_builtin,
};

/// The minimum host `call_builtin` needs. Neither conversion calls back or
/// draws random numbers, so every hook here is unreachable in this file.
struct PureHost;

impl BuiltinHost for PureHost {
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
        unreachable!("neither `trim` nor `number` invokes a callback")
    }
}

fn s(text: &str) -> Value {
    Value::String(Rc::new(text.to_string()))
}

fn call(name: &str, args: &[Value]) -> Value {
    call_builtin(&mut PureHost, name, args).expect("neither conversion can trap")
}

fn trim(text: &str) -> String {
    match call("trim", &[s(text)]) {
        Value::String(out) => out.as_ref().clone(),
        other => panic!("trim({text:?}) should answer a string, got {other:?}"),
    }
}

// ---- trim ----

/// `java.lang.String.trim()` defines space as "any character whose codepoint
/// is less than or equal to `'U+0020'`". A no-break space is U+00A0, so it is
/// not space to Java and upstream keeps it — where `str::trim`, which goes by
/// the Unicode `White_Space` property, ate it.
#[test]
fn trim_keeps_the_unicode_spaces_java_does_not_strip() {
    assert_eq!(
        trim("\u{00A0}x\u{00A0}"),
        "\u{00A0}x\u{00A0}",
        "U+00A0 NO-BREAK SPACE is > U+0020, so `String.trim()` leaves it"
    );
    // The rest of the block `str::trim` strips and Java does not.
    for space in ['\u{0085}', '\u{2000}', '\u{2028}', '\u{202F}', '\u{3000}'] {
        let text = format!("{space}x{space}");
        assert_eq!(trim(&text), text, "U+{:04X} is > U+0020", space as u32);
    }
}

/// Everything at or below U+0020 still goes, from both ends — that half of
/// `String.trim()` is the half `str::trim` already agreed with.
#[test]
fn trim_strips_every_character_at_or_below_u0020() {
    assert_eq!(trim(" \t\nx "), "x");
    assert_eq!(trim("\r\n\u{000B}\u{000C}x\u{0000}\u{001F}"), "x");
    assert_eq!(trim("   "), "", "an all-space string trims away entirely");
    assert_eq!(trim(""), "");
    assert_eq!(trim("a b"), "a b", "interior space is never touched");
}

// ---- number ----

fn number(v: Value) -> Value {
    call("number", &[v])
}

/// A `Boolean` matches neither `instanceof Number` nor `instanceof String`,
/// so `ValueClass.number` runs off the end into `return 0l;`. Both booleans
/// answer the same integer zero — `number(true)` is **not** `1`.
#[test]
fn number_of_a_boolean_is_integer_zero() {
    for b in [true, false] {
        assert!(
            matches!(number(Value::Bool(b)), Value::Int(0)),
            "number({b}) should be integer 0, got {:?}",
            number(Value::Bool(b))
        );
    }
}

/// `BigIntegerValue extends Number`
/// (`leekscript/runner/values/BigIntegerValue.java`), so the first
/// `instanceof Number` test catches it and hands it straight back.
#[test]
fn number_of_a_big_integer_returns_it_unchanged() {
    let digits = "123456789012345678901234567890";
    let got = number(Value::BigInt(Rc::new(big_from_decimal(digits))));
    match got {
        Value::BigInt(b) => assert_eq!(
            big_full_decimal(&b),
            digits,
            "a big integer must round-trip, not be truncated or zeroed"
        ),
        other => panic!("number(bigint) should stay a big integer, got {other:?}"),
    }
}

/// The two shapes the upstream corpus pins (`reference.tsv`, `return
/// number('12')` → `12` and `return number('12.55')` → `12.55`): the dot
/// decides the branch, and with it the result type.
#[test]
fn number_of_a_parsable_string_follows_the_dot() {
    assert!(
        matches!(number(s("12")), Value::Int(12)),
        "no dot → `Long.parseLong`, so an integer"
    );
    match number(s("12.55")) {
        Value::Real(r) => assert!((r - 12.55).abs() < f64::EPSILON, "got {r}"),
        other => panic!("a dot → `Double.parseDouble`, so a real; got {other:?}"),
    }
}

/// Every failure exits `ValueClass.number` through the trailing
/// `return 0l;`, so the zero it answers is an **integer**, whichever branch
/// threw. `Real(0.0)` would be a different value and would display as `0.0`.
#[test]
fn a_string_that_fails_to_parse_is_integer_zero_not_real_zero() {
    for text in ["1.5x", ".", "1.2.3", "..0", "nonsense.", "1.0e"] {
        let got = number(s(text));
        assert!(
            matches!(got, Value::Int(0)),
            "number({text:?}) should be integer 0, got {got:?}"
        );
    }
}

/// `number("1e5")` is `0`, and that is upstream-correct rather than a bug:
/// the string holds no `.`, so `ValueClass.number` takes the
/// `Long.parseLong` branch, which rejects the exponent and throws.
/// `Double.parseDouble` — the only parser that would accept `1e5` — is
/// reachable only for a string that contains a dot.
#[test]
fn number_never_reads_an_exponent_without_a_dot() {
    for text in ["1e5", "1E5", "-2e3", "0x10"] {
        let got = number(s(text));
        assert!(
            matches!(got, Value::Int(0)),
            "number({text:?}) should be integer 0, got {got:?}"
        );
    }
    // With a dot the exponent *is* read, because `Double.parseDouble` is
    // what parses it.
    match number(s("1.0e5")) {
        Value::Real(r) => assert!((r - 100_000.0).abs() < f64::EPSILON, "got {r}"),
        other => panic!("number(\"1.0e5\") should be real 100000.0, got {other:?}"),
    }
}

/// A number argument is returned as it stands, and everything that is
/// neither a number nor a string falls to `return 0l;`.
#[test]
fn number_passes_numbers_through_and_zeroes_the_rest() {
    assert!(matches!(number(Value::Int(-7)), Value::Int(-7)));
    match number(Value::Real(1.5)) {
        Value::Real(r) => assert!((r - 1.5).abs() < f64::EPSILON, "got {r}"),
        other => panic!("number(1.5) should stay real, got {other:?}"),
    }
    assert!(
        matches!(number(Value::Null), Value::Int(0)),
        "null is neither a Number nor a String"
    );
}
