//! Numeric / formatting / map-key edge cases (Theme G testing gap, P2 #23).
//! Characterization tests pinning the runtime's behavior on the corners that
//! tend to differ between languages: integer overflow, real formatting per
//! language version, and NaN / signed-zero as map keys.

use leek_runtime::{DISPLAY_VERSION, MapKey, Value, add, mul, neg, sub};

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        other => panic!("expected Int, got {other:?}"),
    }
}

#[test]
fn integer_arithmetic_wraps_not_panics() {
    // LeekScript integers are 64-bit and wrap (matching the VM / the `neg`
    // wrapping pin), so an overflowing op must wrap, never panic.
    assert_eq!(int(&add(&Value::Int(i64::MAX), &Value::Int(1))), i64::MIN);
    assert_eq!(int(&sub(&Value::Int(i64::MIN), &Value::Int(1))), i64::MAX);
    assert_eq!(int(&mul(&Value::Int(i64::MAX), &Value::Int(2))), -2);
    // wrapping_neg(i64::MIN) == i64::MIN (no overflow panic).
    assert_eq!(int(&neg(&Value::Int(i64::MIN))), i64::MIN);
}

#[test]
fn real_formatting_is_version_specific() {
    let prev = DISPLAY_VERSION.get();
    // v1 renders reals with a comma decimal separator, and an *integer-valued*
    // real drops its fractional part entirely (`42.0` → `"42"`) — a v1 quirk.
    DISPLAY_VERSION.set(1);
    assert_eq!(Value::Real(2.5).to_string(), "2,5");
    assert_eq!(Value::Real(42.0).to_string(), "42");
    // v2+ uses a dot and keeps the trailing `.0` so the value stays a real.
    DISPLAY_VERSION.set(4);
    assert_eq!(Value::Real(2.5).to_string(), "2.5");
    assert_eq!(Value::Real(42.0).to_string(), "42.0");
    DISPLAY_VERSION.set(prev);
}

/// v1 real formatting, pinned against the Java backend.
///
/// Every expectation below was produced by running upstream's own v1 path —
/// `AI.doubleToString` for version < 2, which is
/// `new DecimalFormat()` + `setMinimumFractionDigits(0)` under the server's
/// French locale — on a JDK 25 and copying its output. The separator is a
/// narrow no-break space (U+202F), spelled out here so the expectations stay
/// readable.
#[test]
fn v1_real_formatting_matches_the_java_backend() {
    const NB: &str = "\u{202f}";
    let prev = DISPLAY_VERSION.get();
    DISPLAY_VERSION.set(1);
    let v1 = |r: f64| Value::Real(r).to_string();

    // The bug this pins: the integer part used to go through `real_to_int`,
    // which saturates, so anything past `i64::MAX` printed as
    // "9 223 372 036 854 775 807".
    assert_eq!(
        v1(1e20),
        format!("100{NB}000{NB}000{NB}000{NB}000{NB}000{NB}000")
    );
    assert_eq!(
        v1(-1e20),
        format!("-100{NB}000{NB}000{NB}000{NB}000{NB}000{NB}000")
    );
    assert_eq!(
        v1(1e21),
        format!("1{NB}000{NB}000{NB}000{NB}000{NB}000{NB}000{NB}000")
    );
    // Above ~9e15 `(abs * 1000.0).round()` lost the low digits; 2^53 and its
    // neighbour are the exact integers that used to disappear.
    assert_eq!(
        v1(9_007_199_254_740_992.0),
        format!("9{NB}007{NB}199{NB}254{NB}740{NB}992")
    );
    assert_eq!(
        v1(9_007_199_254_740_994.0),
        format!("9{NB}007{NB}199{NB}254{NB}740{NB}994")
    );
    // Grouping and rounding on an ordinary value: `1234567.8915` is really
    // `…8914999…`, so it rounds *down* to 3 digits.
    assert_eq!(v1(1_234_567.891_5), format!("1{NB}234{NB}567,891"));
    assert_eq!(v1(-1_234_567.891_5), format!("-1{NB}234{NB}567,891"));
    // Ties resolve on the true binary value, not on the literal: `0.0005` is
    // just under a half, `2.0005` just over.
    assert_eq!(v1(0.0005), "0");
    assert_eq!(v1(-0.0005), "-0");
    assert_eq!(v1(2.0005), "2,001");
    assert_eq!(v1(1.0005), "1");
    // Rounding happens before formatting, so 0.9999 carries into the integer
    // part rather than truncating to 0.
    assert_eq!(v1(0.9999), "1");
    assert_eq!(v1(999_999.999_9), format!("1{NB}000{NB}000"));
    // Trailing zeros are trimmed, and an integral real drops its `,0`.
    assert_eq!(v1(0.1), "0,1");
    assert_eq!(v1(1.25), "1,25");
    assert_eq!(v1(100.0), "100");
    assert_eq!(v1(1.0 / 3.0), "0,333");
    assert_eq!(v1(2.0 / 3.0), "0,667");
    // The sign comes from the sign bit, so `-0.0` and values that round to
    // zero from below keep their `-`.
    assert_eq!(v1(0.0), "0");
    assert_eq!(v1(-0.0), "-0");
    assert_eq!(v1(-1e-5), "-0");
    // Non-finite values short-circuit before the French formatting.
    assert_eq!(v1(f64::NAN), "NaN");
    assert_eq!(v1(f64::INFINITY), "∞");
    assert_eq!(v1(f64::NEG_INFINITY), "-∞");

    DISPLAY_VERSION.set(prev);
}

#[test]
fn nan_and_signed_zero_map_keys() {
    // `MapKey::of` is the canonicalisation maps and sets actually key on.
    // Two NaNs canonicalize to the same map key (so `m[NaN]` is addressable and
    // a second write to it overwrites rather than duplicating).
    assert_eq!(
        MapKey::of(&Value::Real(f64::NAN)),
        MapKey::of(&Value::Real(f64::NAN))
    );
    // Signed zero: `0.0` and `-0.0` are numerically equal but keep their sign
    // bit, so they are *distinct* map keys (pinned behavior).
    // Exact float equality is the point here: IEEE 754 defines 0.0 == -0.0.
    #[allow(clippy::float_cmp)]
    {
        assert_eq!(0.0_f64, -0.0_f64);
    }
    assert_ne!(
        MapKey::of(&Value::Real(0.0)),
        MapKey::of(&Value::Real(-0.0))
    );
    // An integer key and a real key with the same magnitude don't collide
    // (the key is type-tagged).
    assert_ne!(MapKey::of(&Value::Int(1)), MapKey::of(&Value::Real(1.0)));
}
