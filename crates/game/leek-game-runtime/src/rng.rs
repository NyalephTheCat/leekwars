//! The official generator's fight RNG — a bit-exact port of the anonymous
//! `RandomGenerator` in the reference `State.java`.
//!
//! The reference uses a glibc-style LCG over a Java `long`:
//! `n = n * 1103515245 + 12345`, then `getDouble()` maps
//! `(n / 65536) % 32768 + 32768` onto `[0, 1)` by dividing by 65536. Java
//! `long` arithmetic overflows by wrapping and `/`/`%` truncate toward zero —
//! exactly Rust's `wrapping_*` and `i64` `/`/`%` — so the port is direct.
//! Golden sequences in the tests below were produced by running the Java
//! code verbatim.

/// Bit-exact port of the official fight RNG (`State.java`'s
/// `RandomGenerator`). Every random draw in a fight — map generation, start
/// order, critical hits, damage rolls — must come from this stream, in the
/// same order as the reference, for fights to replay identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfficialRng {
    n: i64,
}

impl OfficialRng {
    /// An RNG seeded like `state.seed(seed)`.
    #[must_use]
    pub fn new(seed: i64) -> Self {
        Self { n: seed }
    }

    /// Re-seed in place (`RandomGenerator.seed`).
    pub fn seed(&mut self, seed: i64) {
        self.n = seed;
    }

    /// `RandomGenerator.getDouble()`: advance the LCG and map to `(0, 1)`.
    #[allow(clippy::cast_precision_loss)]
    pub fn get_double(&mut self) -> f64 {
        self.n = self.n.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let r = (self.n / 65_536) % 32_768 + 32_768;
        r as f64 / 65_536.0
    }

    /// `RandomGenerator.getInt(min, max)`: uniform over `min..=max`
    /// (0 when the range is empty or overflows an `i32`, as in Java).
    #[allow(clippy::cast_possible_truncation)]
    pub fn get_int(&mut self, min: i32, max: i32) -> i32 {
        if max.wrapping_sub(min).wrapping_add(1) <= 0 {
            return 0;
        }
        let span = f64::from(max) - f64::from(min) + 1.0;
        min.wrapping_add((self.get_double() * span) as i32)
    }

    /// `RandomGenerator.getLong(min, max)`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub fn get_long(&mut self, min: i64, max: i64) -> i64 {
        if max.wrapping_sub(min).wrapping_add(1) <= 0 {
            return 0;
        }
        let span = (max - min + 1) as f64;
        min.wrapping_add((self.get_double() * span) as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::OfficialRng;

    /// `getDouble()` matches the Java reference bit-for-bit (golden values
    /// from running the Java LCG with `Double.toHexString`).
    #[test]
    #[allow(clippy::float_cmp)] // bit-exact equality is the contract
    fn get_double_matches_java_seed_42() {
        let mut rng = OfficialRng::new(42);
        let expected = [
            f64::from_bits(0x3FE9_5120_0000_0000), // 0x1.9512p-1
            f64::from_bits(0x3FD0_A280_0000_0000), // 0x1.0a28p-2
            f64::from_bits(0x3FE7_74A0_0000_0000), // 0x1.774ap-1
            f64::from_bits(0x3FEC_6EA0_0000_0000), // 0x1.c6eap-1
            f64::from_bits(0x3FCB_1080_0000_0000), // 0x1.b108p-3
            f64::from_bits(0x3FE0_88A0_0000_0000), // 0x1.088ap-1
            f64::from_bits(0x3FE6_ADA0_0000_0000), // 0x1.6adap-1
            f64::from_bits(0x3FD9_E140_0000_0000), // 0x1.9e14p-2
        ];
        for (i, want) in expected.into_iter().enumerate() {
            assert_eq!(rng.get_double(), want, "draw {i}");
        }
    }

    /// Negative LCG states (negative seed) also match Java's truncated
    /// division and remainder.
    #[test]
    #[allow(clippy::float_cmp)] // bit-exact equality is the contract
    fn get_double_matches_java_negative_seed() {
        let mut rng = OfficialRng::new(-7);
        let expected = [
            f64::from_bits(0x3FC9_CA80_0000_0000), // 0x1.9ca8p-3
            f64::from_bits(0x3F76_A000_0000_0000), // 0x1.6ap-8
            f64::from_bits(0x3FC1_CA00_0000_0000), // 0x1.1cap-3
            f64::from_bits(0x3FD5_0E00_0000_0000), // 0x1.50ep-2
        ];
        for (i, want) in expected.into_iter().enumerate() {
            assert_eq!(rng.get_double(), want, "draw {i}");
        }
    }

    /// `getInt` over the map-cell range matches the Java reference.
    #[test]
    fn get_int_matches_java() {
        let mut rng = OfficialRng::new(42);
        let expected = [484, 159, 449, 544, 129, 316, 434, 247];
        for (i, want) in expected.into_iter().enumerate() {
            assert_eq!(rng.get_int(0, 612), want, "draw {i}");
        }
    }

    /// Seeds wider than 32 bits behave like Java `long` seeding.
    #[test]
    fn get_int_matches_java_wide_seed() {
        let mut rng = OfficialRng::new(1_234_567_890_123);
        let expected = [5, 8, 8, 5];
        for (i, want) in expected.into_iter().enumerate() {
            assert_eq!(rng.get_int(1, 17), want, "draw {i}");
        }
    }

    /// Empty or inverted ranges return 0 without consuming a draw.
    #[test]
    fn degenerate_ranges_return_zero() {
        let mut rng = OfficialRng::new(42);
        assert_eq!(rng.get_int(5, 4), 0);
        assert_eq!(rng.get_int(i32::MIN, i32::MAX), 0); // span overflows
        assert_eq!(rng.get_long(9, 3), 0);
        // The stream was not advanced: the first real draw still matches.
        assert_eq!(rng.get_int(0, 612), 484);
    }
}
