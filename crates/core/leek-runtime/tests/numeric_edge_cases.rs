//! Numeric / formatting / map-key edge cases (Theme G testing gap, P2 #23).
//! Characterization tests pinning the runtime's behavior on the corners that
//! tend to differ between languages: integer overflow, real formatting per
//! language version, and NaN / signed-zero as map keys.

use leek_runtime::{DISPLAY_VERSION, Value, add, key_repr, mul, neg, sub};

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

#[test]
fn nan_and_signed_zero_map_keys() {
    // Two NaNs canonicalize to the same map key (so `m[NaN]` is addressable and
    // a second write to it overwrites rather than duplicating).
    assert_eq!(
        key_repr(&Value::Real(f64::NAN)),
        key_repr(&Value::Real(f64::NAN))
    );
    // Signed zero: `0.0` and `-0.0` are numerically equal but format
    // distinctly, so they are *distinct* map keys (pinned behavior).
    // Exact float equality is the point here: IEEE 754 defines 0.0 == -0.0.
    #[allow(clippy::float_cmp)]
    {
        assert_eq!(0.0_f64, -0.0_f64);
    }
    assert_ne!(key_repr(&Value::Real(0.0)), key_repr(&Value::Real(-0.0)));
    // An integer key and a real key with the same magnitude don't collide
    // (the key is type-tagged).
    assert_ne!(key_repr(&Value::Int(1)), key_repr(&Value::Real(1.0)));
}
