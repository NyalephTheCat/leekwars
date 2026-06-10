//! Deterministic PRNG shared by the backends' random builtins.
//!
//! A seeded xorshift64 — reproducible across runs so the corpus's
//! statistical tests are stable, while still giving real spread. Lives in
//! `leek-runtime` (not a backend) so the interpreter and the native
//! backend draw from the same generator.
//
// PRNG bit math reinterprets between signed/unsigned and builds floats from
// raw bits; the casts here are deliberate, not lossy coercions.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
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
        let span = (hi - lo) as u64;
        lo + (self.next_u64() % span) as i64
    }

    /// Uniform real in `[lo, hi)` (53-bit mantissa of randomness).
    pub fn real_in(&mut self, lo: f64, hi: f64) -> f64 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + unit * (hi - lo)
    }
}
