//! Turn-order computation — a bit-exact port of the reference
//! `state/StartOrder.java`.
//!
//! Each team's entities are first sorted by frequency (descending, stable).
//! Each team then gets an Elo-like probability from its *lead* entity's
//! frequency, `1 / (1 + 10^((sum - f) / 100))` with `sum` the total of all
//! lead frequencies, normalized to 1. A **team order** is drawn — one
//! `getDouble()` per team, walking the remaining teams and subtracting
//! probabilities until `v <= p`; after each pick every probability is
//! divided by `1 - p`. Finally entities are interleaved round-robin over the
//! team order, skipping exhausted teams. Golden orderings in the tests were
//! produced by running the Java algorithm verbatim with the official LCG.

use crate::fight::Fight;
use crate::host::GameHost;
use crate::rng::OfficialRng;

/// The frequency every fighter is given when computing the engine-native
/// fight's start order. The reference draws each team's probability from its
/// lead's `frequency` stat, which the scenario schema doesn't carry yet
/// (GAME-09); until then every team weighs the same and the draw alone
/// decides who opens.
pub const DEFAULT_FREQUENCY: i64 = 100;

/// Compute the global turn order for a fight (`StartOrder.compute`).
///
/// `teams` holds, per team, the `(entity id, frequency)` pairs of its
/// fighters; every team must be non-empty. Returns entity ids in play order,
/// consuming exactly `teams.len()` `getDouble()` draws like the reference.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn compute_start_order(teams: &[Vec<(i64, i64)>], rng: &mut OfficialRng) -> Vec<i64> {
    // Sort entities inside each team on their frequency (descending; Rust's
    // sort is stable like Collections.sort).
    let queues: Vec<Vec<(i64, i64)>> = teams
        .iter()
        .map(|team| {
            let mut q = team.clone();
            q.sort_by_key(|b| std::cmp::Reverse(b.1));
            q
        })
        .collect();
    let total: usize = queues.iter().map(Vec::len).sum();

    // Probability for each team from its lead entity's frequency.
    let frequencies: Vec<f64> = queues.iter().map(|q| q[0].1 as f64).collect();
    let sum: f64 = frequencies.iter().sum();
    let mut probas: Vec<f64> = frequencies
        .iter()
        .map(|&f| 1.0 / (1.0 + 10f64.powf((sum - f) / 100.0)))
        .collect();
    let psum: f64 = probas.iter().sum();
    for p in &mut probas {
        *p /= psum;
    }

    // Compute the team order: one draw per team, walking the remaining
    // teams; renormalize all probabilities by `1 - p` after each pick.
    let mut team_order: Vec<usize> = Vec::with_capacity(queues.len());
    let mut remaining: Vec<usize> = (0..queues.len()).collect();
    for _ in 0..queues.len() {
        let mut v = rng.get_double();
        let mut psum = 1.0;
        for i in 0..remaining.len() {
            let team = remaining[i];
            let p = probas[team];
            if v <= p {
                team_order.push(team);
                remaining.remove(i);
                psum -= p;
                break;
            }
            v -= p;
        }
        for p in &mut probas {
            *p /= psum;
        }
    }

    // Interleave entities round-robin over the team order, skipping
    // exhausted teams.
    let mut order = Vec::with_capacity(total);
    let mut cursors = vec![0usize; queues.len()];
    let mut current = 0usize;
    while order.len() != total {
        let team = team_order[current];
        if cursors[team] < queues[team].len() {
            order.push(queues[team][cursors[team]].0);
            cursors[team] += 1;
        }
        current = (current + 1) % queues.len();
    }
    order
}

/// Stir a fight seed into the LCG state the start-order draw starts from
/// (splitmix64, then the bits as a Java `long`).
///
/// The reference reaches `StartOrder.compute` with a well-mixed state: map
/// generation has already drawn from the same stream. Here the start order is
/// the first draw, and scenario seeds are small counters (1, 2, 3, …), whose
/// LCG state stays positive for the first steps — `getDouble()` would then
/// land in `[0.5, 1)` for *every* seed and the same team would open every
/// fight. Mixing first gives the draw its full range.
fn start_order_seed(seed: u64) -> i64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // The state is a Java `long`: keep the bits, wrapping into the sign.
    i64::from_ne_bytes(z.to_ne_bytes())
}

/// The start order of an engine-native [`Fight`]: group its living entities
/// into teams (teams and their members in setup order) and run
/// [`compute_start_order`] over a fresh [`OfficialRng`] seeded from
/// [`Fight::seed`] (through [`start_order_seed`]).
///
/// This is the fight's order of play for every turn, drawn once like the
/// reference's `StartOrder.compute` — so who opens follows the seed rather
/// than the entity ids. The draws come from their own stream, so the combat
/// RNG (damage rolls) is untouched.
#[must_use]
pub fn fight_start_order(fight: &Fight) -> Vec<i64> {
    let mut teams: Vec<(i64, Vec<(i64, i64)>)> = Vec::new();
    for id in fight.entities(true) {
        let team = fight.team(id).unwrap_or_default();
        if let Some((_, members)) = teams.iter_mut().find(|(t, _)| *t == team) {
            members.push((id, DEFAULT_FREQUENCY));
        } else {
            teams.push((team, vec![(id, DEFAULT_FREQUENCY)]));
        }
    }
    if teams.is_empty() {
        return Vec::new();
    }
    let queues: Vec<Vec<(i64, i64)>> = teams.into_iter().map(|(_, members)| members).collect();
    let mut rng = OfficialRng::new(start_order_seed(fight.seed()));
    compute_start_order(&queues, &mut rng)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_FREQUENCY, compute_start_order, fight_start_order, start_order_seed};
    use crate::rng::OfficialRng;
    use crate::{Entity, Fight};

    // Golden orderings from running the Java `StartOrder` algorithm verbatim
    // with the official LCG.

    #[test]
    fn one_v_one_equal_frequency() {
        let teams = vec![vec![(1, 100)], vec![(2, 100)]];
        let mut rng = OfficialRng::new(42);
        assert_eq!(compute_start_order(&teams, &mut rng), vec![2, 1]);
        let mut rng = OfficialRng::new(1);
        assert_eq!(compute_start_order(&teams, &mut rng), vec![2, 1]);
    }

    #[test]
    fn two_v_two_mixed_frequencies() {
        let teams = vec![vec![(1, 100), (2, 300)], vec![(3, 200), (4, 50)]];
        let mut rng = OfficialRng::new(7);
        assert_eq!(compute_start_order(&teams, &mut rng), vec![2, 3, 1, 4]);
    }

    #[test]
    fn three_teams_uneven_sizes() {
        let teams = vec![vec![(1, 150)], vec![(2, 150), (3, 150)], vec![(4, 400)]];
        let mut rng = OfficialRng::new(12_345);
        assert_eq!(compute_start_order(&teams, &mut rng), vec![4, 1, 2, 3]);
    }

    #[test]
    fn three_v_two_equal_frequency() {
        let teams = vec![
            vec![(10, 100), (11, 100), (12, 100)],
            vec![(20, 100), (21, 100)],
        ];
        let mut rng = OfficialRng::new(99);
        assert_eq!(
            compute_start_order(&teams, &mut rng),
            vec![20, 10, 21, 11, 12]
        );
    }

    /// A 1v1 arena laid out the usual way: the lower id leads team 0.
    fn duel(seed: u64) -> Fight {
        Fight::new(10, 10, 1)
            .with_seed(seed)
            .with_entity(Entity::new(1, "Bot", 0, 0))
            .with_entity(Entity::new(2, "Foe", 33, 1))
    }

    /// Regression (#39): the engine-native order used to be the entity ids
    /// sorted, so id 1 opened every fight on every seed. It is drawn from the
    /// seed now, so both sides get to open.
    #[test]
    fn fight_start_order_follows_the_seed() {
        let seeds = 1..=64;
        let opened_by_1 = seeds
            .clone()
            .filter(|&seed| fight_start_order(&duel(seed)).first() == Some(&1))
            .count();
        // Equal frequencies: the draw is a coin flip, so neither side runs
        // away with the openings (it was 64/64 for id 1 before #39).
        assert!(
            (16..=48).contains(&opened_by_1),
            "id 1 opened {opened_by_1} of 64 seeds"
        );
        // Every fighter still gets exactly one slot.
        let mut order = fight_start_order(&duel(7));
        order.sort_unstable();
        assert_eq!(order, vec![1, 2]);
    }

    /// The fight-level helper is `compute_start_order` over uniform
    /// frequencies, drawn from the fight's own seed.
    #[test]
    fn fight_start_order_matches_the_reference_draw() {
        let teams = vec![vec![(1, DEFAULT_FREQUENCY)], vec![(2, DEFAULT_FREQUENCY)]];
        for seed in [1_u64, 42, 1_000_003] {
            let mut rng = OfficialRng::new(start_order_seed(seed));
            assert_eq!(
                fight_start_order(&duel(seed)),
                compute_start_order(&teams, &mut rng),
                "seed {seed}"
            );
        }
    }

    /// Teams and their members keep setup order, and the dead take no slot.
    #[test]
    fn fight_start_order_groups_by_team_and_skips_the_dead() {
        let fight = Fight::new(10, 10, 1)
            .with_seed(3)
            .with_entity(Entity::new(7, "Lead", 0, 0))
            .with_entity(Entity::new(3, "Ally", 1, 0))
            .with_entity(Entity::new(5, "Foe", 33, 1))
            .with_entity(Entity::new(4, "Corpse", 34, 1).with_life(0));
        let order = fight_start_order(&fight);
        assert_eq!(order.len(), 3);
        assert!(
            !order.contains(&4),
            "a dead fighter takes no slot: {order:?}"
        );
        let team0: Vec<i64> = order.iter().copied().filter(|id| *id != 5).collect();
        assert_eq!(team0, vec![7, 3], "team members keep setup order");
    }
}
