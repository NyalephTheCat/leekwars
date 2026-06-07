//! Seeded point-buy build generation for randomized AI testing.
//!
//! Leek-wars has no point-buy formula in the engine, so this defines one:
//! distribute a `capital` of stat points across the chosen stats with random
//! weights, deterministically per seed (the same seed always yields the same
//! build, so a regression is reproducible).

use std::collections::HashMap;

use crate::schema::{EntitySpec, StatKind};

/// A splitmix64 mix of two values — turns a `(base_seed, run_index)` pair into
/// a well-distributed per-run seed so consecutive runs don't collide (and don't
/// land on [`gen_build`]'s zero-seed fallback).
#[must_use]
pub fn mix(a: u64, b: u64) -> u64 {
    let mut z = a
        .wrapping_add(b.wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A xorshift64 step — the same family the generator's `Fight` RNG uses, so we
/// stay dependency-free and reproducible.
fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Generate one build: give every stat `min_per_stat`, then split the remaining
/// capital across `stats` by random weights. Deterministic for a given `seed`.
#[must_use]
pub fn gen_build(
    capital: i64,
    stats: &[StatKind],
    min_per_stat: i64,
    seed: u64,
) -> HashMap<StatKind, i64> {
    let mut alloc: HashMap<StatKind, i64> = stats.iter().map(|s| (*s, min_per_stat)).collect();
    if stats.is_empty() {
        return alloc;
    }

    let n = i64::try_from(stats.len()).unwrap_or(1);
    let remaining = (capital - min_per_stat * n).max(0);

    let mut rng = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };
    let weights: Vec<u64> = stats.iter().map(|_| (xorshift(&mut rng) % 1000) + 1).collect();
    let total: u128 = weights.iter().map(|&w| u128::from(w)).sum();

    let rem = u128::from(u64::try_from(remaining).unwrap_or(0));
    let mut distributed = 0;
    for (s, &w) in stats.iter().zip(&weights) {
        let share = i64::try_from(rem * u128::from(w) / total).unwrap_or(0);
        *alloc.get_mut(s).unwrap() += share;
        distributed += share;
    }
    // Hand any rounding remainder to a pseudo-random stat so the total is exact.
    let leftover = remaining - distributed;
    if leftover > 0 {
        let idx = usize::try_from(xorshift(&mut rng) % (stats.len() as u64)).unwrap_or(0);
        *alloc.get_mut(&stats[idx]).unwrap() += leftover;
    }
    alloc
}

/// Write a generated build onto an entity spec (overwriting those stats).
pub fn apply_build(spec: &mut EntitySpec, build: &HashMap<StatKind, i64>) {
    for (stat, &value) in build {
        match stat {
            StatKind::Strength => spec.strength = Some(value),
            StatKind::Agility => spec.agility = Some(value),
            StatKind::Wisdom => spec.wisdom = Some(value),
            StatKind::Resistance => spec.resistance = Some(value),
            StatKind::Science => spec.science = Some(value),
            StatKind::Magic => spec.magic = Some(value),
            StatKind::Power => spec.power = Some(value),
        }
    }
}
