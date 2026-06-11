//! `big_integer` helpers — conversions and the upstream display crop.
//!
//! Mirrors upstream `BigIntegerValue` (an immutable wrapper over Java's
//! `BigInteger`). The value itself lives in [`super::Value::BigInt`] as an
//! `Rc<BigInt>` (immutable, so sharing is safe).

use num_bigint::BigInt;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};

// Number of digits kept at each end when displaying a very large number
// (upstream `STRING_CROP_LIMIT`).
const STRING_CROP_LIMIT: usize = 10;

/// Java `BigInteger.bitLength()` — length of the minimal two's-complement
/// representation, excluding the sign bit. For negatives this is one less
/// than the magnitude's bit count when the magnitude is a power of two.
pub fn java_bit_length(b: &BigInt) -> u64 {
    let bits = b.magnitude().bits();
    if b.is_negative() && b.magnitude().count_ones() == 1 {
        bits - 1
    } else {
        bits
    }
}

/// Java `BigInteger.longValue()` — the low 64 bits, two's complement
/// (wrapping, not saturating). Used by upstream `longint()` coercion.
pub fn big_to_i64_wrapping(b: &BigInt) -> i64 {
    let low = b.iter_u64_digits().next().unwrap_or(0);
    // `n mod 2^64` for a sign-magnitude value: negate the low magnitude
    // word (wrapping) when the value is negative.
    #[allow(clippy::cast_possible_wrap)]
    let low = low as i64;
    if b.is_negative() {
        low.wrapping_neg()
    } else {
        low
    }
}

/// Java `BigInteger.doubleValue()` — nearest double, `±inf` on overflow.
pub fn big_to_f64(b: &BigInt) -> f64 {
    b.to_f64().unwrap_or(f64::NAN)
}

/// Upstream `BigIntegerValue(AI, double)` — `BigDecimal.valueOf(val)`
/// truncation for finite reals (exact integer part, no i64 saturation),
/// `(long)` saturation for `±inf` / `NaN`.
pub fn f64_to_bigint(f: f64) -> BigInt {
    if f.is_finite() {
        BigInt::from_f64(f.trunc()).unwrap_or_default()
    } else {
        BigInt::from(crate::real_to_int(f))
    }
}

/// Upstream `BigIntegerValue.toString()` — full decimal up to ~20 digits
/// (`bitLength <= STRING_CROP_LIMIT * 2 / log10(2)`), then cropped to the
/// first and last 10 digits: `1234567890...1234567890`. A negative keeps
/// its sign plus 10 leading digits.
pub fn big_display(b: &BigInt) -> String {
    let bits = java_bit_length(b);
    // BIT_LIMIT = STRING_CROP_LIMIT * 2 / log10(2) ≈ 66.44
    #[allow(clippy::cast_precision_loss)]
    let over = (bits as f64) > (STRING_CROP_LIMIT * 2) as f64 / std::f64::consts::LOG10_2;
    if !over {
        return b.to_string();
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let digit_length = (bits as f64 * std::f64::consts::LOG10_2).floor() as u64;
    let ten = BigInt::from(10);
    let first_digits = (b / ten
        .pow(u32::try_from(digit_length - STRING_CROP_LIMIT as u64).unwrap_or(u32::MAX)))
    .to_string();
    // `substring(0, CROP_LIMIT - min(0, signum))` — one extra char for `-`.
    let keep = STRING_CROP_LIMIT + usize::from(b.is_negative());
    let first: String = first_digits.chars().take(keep).collect();
    let last = b.abs() % ten.pow(u32::try_from(STRING_CROP_LIMIT).unwrap_or(u32::MAX));
    format!("{first}...{last:0>width$}", width = STRING_CROP_LIMIT)
}

/// Full (uncropped) decimal form — used by `jsonEncode`, which bypasses
/// the display crop upstream.
pub fn big_full_decimal(b: &BigInt) -> String {
    b.to_string()
}

/// Parse canonical decimal digits (a MIR `Const::BigInt` / HIR
/// `Literal::BigInt`) back into a value. Malformed text falls back to 0
/// — lexer diagnostics already cover those literals.
pub fn big_from_decimal(s: &str) -> BigInt {
    s.parse().unwrap_or_default()
}

/// True when the value is zero (truthiness, upstream `isZero`).
pub fn big_is_zero(b: &BigInt) -> bool {
    b.is_zero()
}
