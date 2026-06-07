//! LeekScript numeric coercions.
//!
//! These conversions intentionally truncate or lose precision per the
//! language's defined `integer` / `real` semantics, so the `as` casts here
//! are deliberate, not accidental narrowings. Centralizing them gives one
//! audited home for that behavior (and one place the cast lints are allowed).

/// `real → integer`: truncates toward zero. Rust's `f64 as i64` saturates
/// out-of-range values and maps `NaN` to `0`, matching LeekScript clamping.
#[must_use]
#[inline]
#[allow(clippy::cast_possible_truncation)]
pub fn real_to_int(r: f64) -> i64 {
    r as i64
}

/// `integer → real`: may lose precision for magnitudes beyond 2^53, exactly
/// as LeekScript does when widening an `integer` to a `real`.
#[must_use]
#[inline]
#[allow(clippy::cast_precision_loss)]
pub fn int_to_real(i: i64) -> f64 {
    i as f64
}

/// A container length / `usize` as a LeekScript `integer`. Lengths never
/// approach `i64::MAX` in practice, so the widening is effectively exact.
#[must_use]
#[inline]
#[allow(clippy::cast_possible_wrap)]
pub fn len_as_int(n: usize) -> i64 {
    n as i64
}

/// Clamp a LeekScript `integer` to a non-negative array index. Negative
/// values clamp to `0`; the result is the `usize` position.
#[must_use]
#[inline]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn clamp_index(i: i64) -> usize {
    i.max(0) as usize
}
