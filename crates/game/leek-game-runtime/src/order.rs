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

use crate::rng::OfficialRng;

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
            q.sort_by(|a, b| b.1.cmp(&a.1));
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

#[cfg(test)]
mod tests {
    use super::compute_start_order;
    use crate::rng::OfficialRng;

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
}
