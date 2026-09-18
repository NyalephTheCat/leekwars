//! Deterministic PRNG shared by the backends' random builtins.
//!
//! A seeded xorshift64 — reproducible across runs so the corpus's
//! statistical tests are stable, while still giving real spread. Lives in
//! `leek-runtime` (not a backend) so every execution backend draws from
//! the same generator.
//
// PRNG bit math reinterprets between signed/unsigned, truncates a 128-bit
// product back to its halves and builds floats from raw bits; the casts here
// are deliberate, not lossy coercions.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

/// A seeded xorshift64 generator.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Default for Rng {
    fn default() -> Self {
        Self::new()
    }
}

impl Rng {
    /// A generator seeded with the fixed default seed.
    pub fn new() -> Self {
        Self {
            state: 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// A generator seeded with `seed` (a zero seed falls back to the
    /// default, since xorshift can't escape an all-zero state).
    pub fn with_seed(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// Advance the generator and return the next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Uniform integer in `[lo, hi)` (exclusive upper, matching upstream
    /// `randInt`). Returns `lo` when the range is empty.
    pub fn int_in(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        // `hi - lo` overflows `i64` as soon as the range is wider than
        // `i64::MAX` (`randInt(-9e18, 9e18)`) — a panic under
        // `overflow-checks`, a negative span in release. The *unsigned*
        // difference is always exact, though: the widest legal range,
        // `i64::MIN..i64::MAX`, is `u64::MAX` wide.
        let span = hi.wrapping_sub(lo) as u64;
        lo.wrapping_add(self.below(span) as i64)
    }

    /// Uniform `u64` in `[0, span)` (`span > 0`) by Lemire's
    /// multiply-and-shift, with the rejection step that makes it exactly
    /// unbiased: plain `next_u64() % span` over-represents the low
    /// `2^64 % span` values whenever `span` doesn't divide `2^64`.
    fn below(&mut self, span: u64) -> u64 {
        debug_assert!(span > 0, "below() needs a non-empty range");
        let mut product = u128::from(self.next_u64()) * u128::from(span);
        let mut low = product as u64;
        if low < span {
            // `(2^64 - span) % span` — the number of low halves that would
            // make one bucket wider than the others.
            let threshold = span.wrapping_neg() % span;
            while low < threshold {
                product = u128::from(self.next_u64()) * u128::from(span);
                low = product as u64;
            }
        }
        (product >> 64) as u64
    }

    /// Uniform real in `[lo, hi)` (53-bit mantissa of randomness).
    ///
    /// Degenerate bounds are handled rather than propagated: a `NaN` bound
    /// gives `NaN`, an empty or reversed range gives `lo` (as `int_in`
    /// does), and infinite bounds are pulled in to the representable
    /// extremes so the draw stays a finite real instead of `NaN`/`inf`.
    pub fn real_in(&mut self, lo: f64, hi: f64) -> f64 {
        // Draw first, whatever the bounds: the generator advances exactly
        // once per call, so a degenerate range can't shift the sequence the
        // following calls see.
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        if lo.is_nan() || hi.is_nan() {
            return f64::NAN;
        }
        if hi <= lo {
            return lo;
        }
        // `f64::MIN`/`f64::MAX` are the finite extremes, so `max`/`min` here
        // only ever replace an infinity.
        let lo = lo.max(f64::MIN);
        let hi = hi.min(f64::MAX);
        let span = hi - lo;
        if span.is_finite() {
            lo + unit * span
        } else {
            // The span itself overflows (a range spanning most of the f64
            // line): interpolate without ever forming it. The two terms have
            // opposite signs here, so the sum can't overflow either.
            lo * (1.0 - unit) + hi * unit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn the_widest_int_range_does_not_overflow() {
        // `hi - lo` would overflow `i64` here (the whole line minus one), so
        // this panics under `overflow-checks` unless the span is computed
        // unsigned. Every draw must still land inside `[lo, hi)`.
        let mut rng = Rng::new();
        for _ in 0..10_000 {
            // (`v >= i64::MIN` is vacuous, so only the open end is asserted.)
            let v = rng.int_in(i64::MIN, i64::MAX);
            assert!(v < i64::MAX);
        }
        // The other wide ranges the issue names: `randInt(-9e18, 9e18)` and
        // the two halves of the line.
        for (lo, hi) in [
            (
                -9_000_000_000_000_000_000_i64,
                9_000_000_000_000_000_000_i64,
            ),
            (i64::MIN, 0),
            (0, i64::MAX),
            (i64::MIN + 1, i64::MAX),
        ] {
            for _ in 0..1_000 {
                let v = rng.int_in(lo, hi);
                assert!(v >= lo && v < hi, "{v} out of [{lo}, {hi})");
            }
        }
    }

    #[test]
    fn empty_and_reversed_int_ranges_give_the_low_bound() {
        let mut rng = Rng::new();
        assert_eq!(rng.int_in(7, 7), 7);
        assert_eq!(rng.int_in(7, 3), 7);
        assert_eq!(rng.int_in(i64::MAX, i64::MIN), i64::MAX);
        assert_eq!(rng.int_in(i64::MIN, i64::MIN), i64::MIN);
    }

    #[test]
    fn a_one_wide_int_range_is_the_low_bound() {
        let mut rng = Rng::new();
        assert_eq!(rng.int_in(4, 5), 4);
        assert_eq!(rng.int_in(i64::MIN, i64::MIN + 1), i64::MIN);
        assert_eq!(rng.int_in(i64::MAX - 1, i64::MAX), i64::MAX - 1);
    }

    #[test]
    fn small_int_ranges_cover_every_value_without_modulo_bias() {
        // Lemire's mapping is a partition of the 64-bit output into equal
        // buckets, so a small range must stay roughly flat. A `% span` over a
        // biased stream would skew the low buckets; this is a coarse guard
        // (±25% of the expected count) that still catches a broken mapping.
        let mut rng = Rng::new();
        let mut counts = [0_u32; 6];
        for _ in 0..60_000 {
            let v = rng.int_in(1, 7);
            assert!((1..7).contains(&v));
            counts[usize::try_from(v - 1).unwrap()] += 1;
        }
        for c in counts {
            assert!(c > 7_500 && c < 12_500, "bucket counts skewed: {counts:?}");
        }
    }

    #[test]
    fn reals_stay_inside_their_bounds() {
        let mut rng = Rng::new();
        for _ in 0..10_000 {
            let v = rng.real_in(-1.5, 2.5);
            assert!((-1.5..2.5).contains(&v), "{v} out of [-1.5, 2.5)");
        }
    }

    #[test]
    fn degenerate_real_bounds_do_not_leak_nan_or_infinity() {
        let mut rng = Rng::new();
        // Empty and reversed ranges collapse to the low bound.
        assert!((rng.real_in(1.5, 1.5) - 1.5).abs() < f64::EPSILON);
        assert!((rng.real_in(2.5, 1.0) - 2.5).abs() < f64::EPSILON);
        // A NaN bound is the one case that stays NaN (there is no sane
        // value to invent), but it must not panic.
        assert!(rng.real_in(f64::NAN, 1.0).is_nan());
        assert!(rng.real_in(0.0, f64::NAN).is_nan());
        // Infinite bounds used to produce NaN (`inf - inf`) or `inf`.
        for (lo, hi) in [
            (f64::NEG_INFINITY, f64::INFINITY),
            (0.0, f64::INFINITY),
            (f64::NEG_INFINITY, 0.0),
            (f64::MIN, f64::MAX),
        ] {
            for _ in 0..1_000 {
                let v = rng.real_in(lo, hi);
                assert!(v.is_finite(), "real_in({lo}, {hi}) gave {v}");
            }
        }
        // `inf..inf` is empty, so it takes the reversed-range path.
        assert!(rng.real_in(f64::INFINITY, f64::INFINITY).is_infinite());
    }
}
