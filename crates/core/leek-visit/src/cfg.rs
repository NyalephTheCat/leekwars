//! Generic control-flow-graph traversal.
//!
//! An IR provides a [`Cfg`] view — an entry block, the set of blocks, and
//! the successors of each block — and gets the standard graph algorithms
//! for free: [`postorder`], [`reverse_postorder`], and [`predecessors`].
//! MIR implements [`Cfg`] for its per-function CFG; the native backend
//! and any future dataflow pass consume these.
//!
//! The walks are iterative (no recursion) so deeply-nested control flow
//! can't blow the stack, and visit only blocks reachable from the entry.

use std::collections::HashMap;
use std::hash::Hash;

/// A control-flow graph view over some IR. `BlockId` is a cheap handle
/// (typically a newtype around `u32`).
pub trait Cfg {
    type BlockId: Copy + Eq + Hash;

    /// The entry block — where execution starts.
    fn entry(&self) -> Self::BlockId;

    /// Every block in the graph, in the IR's own storage order. Used for
    /// allocating per-block state; traversal order comes from the
    /// algorithms below.
    fn block_ids(&self) -> Vec<Self::BlockId>;

    /// The blocks control may transfer to when leaving `block`. Order is
    /// significant for deterministic traversal: list `then` before
    /// `else`, switch arms in source order before the default, etc.
    fn successors(&self, block: Self::BlockId) -> Vec<Self::BlockId>;
}

/// Depth-first post-order of the blocks reachable from the entry: a block
/// appears only after all of its successors' subtrees. Unreachable blocks
/// are omitted.
pub fn postorder<C: Cfg>(cfg: &C) -> Vec<C::BlockId> {
    let mut visited: Vec<C::BlockId> = Vec::new();
    let mut order: Vec<C::BlockId> = Vec::new();
    // Explicit stack of (block, successors-iterator-as-index).
    let mut stack: Vec<(C::BlockId, Vec<C::BlockId>, usize)> = Vec::new();

    let mark = |v: &mut Vec<C::BlockId>, b: C::BlockId| {
        if v.contains(&b) {
            false
        } else {
            v.push(b);
            true
        }
    };

    let entry = cfg.entry();
    if mark(&mut visited, entry) {
        stack.push((entry, cfg.successors(entry), 0));
    }
    while let Some((block, succs, idx)) = stack.last_mut() {
        if *idx < succs.len() {
            let next = succs[*idx];
            *idx += 1;
            if mark(&mut visited, next) {
                stack.push((next, cfg.successors(next), 0));
            }
        } else {
            order.push(*block);
            stack.pop();
        }
    }
    order
}

/// Reverse post-order — the canonical order for forward dataflow and for
/// emitting blocks so a block precedes everything it dominates. Equal to
/// [`postorder`] reversed.
pub fn reverse_postorder<C: Cfg>(cfg: &C) -> Vec<C::BlockId> {
    let mut po = postorder(cfg);
    po.reverse();
    po
}

/// Predecessor map: for each reachable block, the blocks that jump to it.
/// Built from the reachable sub-graph (so it agrees with [`postorder`]).
pub fn predecessors<C: Cfg>(cfg: &C) -> HashMap<C::BlockId, Vec<C::BlockId>> {
    let mut preds: HashMap<C::BlockId, Vec<C::BlockId>> = HashMap::new();
    for b in postorder(cfg) {
        for s in cfg.successors(b) {
            preds.entry(s).or_default().push(b);
        }
    }
    preds
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny diamond CFG: 0 → {1, 2} → 3.
    struct Diamond;
    impl Cfg for Diamond {
        type BlockId = u32;
        fn entry(&self) -> u32 {
            0
        }
        fn block_ids(&self) -> Vec<u32> {
            vec![0, 1, 2, 3]
        }
        fn successors(&self, b: u32) -> Vec<u32> {
            match b {
                0 => vec![1, 2],
                1 | 2 => vec![3],
                _ => vec![],
            }
        }
    }

    #[test]
    fn rpo_entry_first_join_last() {
        let rpo = reverse_postorder(&Diamond);
        assert_eq!(rpo[0], 0, "entry first");
        assert_eq!(rpo[3], 3, "join block last");
        // Every block reachable exactly once.
        let mut sorted = rpo.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3]);
    }

    #[test]
    fn preds_of_join_are_both_arms() {
        let preds = predecessors(&Diamond);
        let mut p = preds[&3].clone();
        p.sort_unstable();
        assert_eq!(p, vec![1, 2]);
    }

    /// Unreachable block 9 must be skipped.
    struct WithDead;
    impl Cfg for WithDead {
        type BlockId = u32;
        fn entry(&self) -> u32 {
            0
        }
        fn block_ids(&self) -> Vec<u32> {
            vec![0, 1, 9]
        }
        fn successors(&self, b: u32) -> Vec<u32> {
            match b {
                0 => vec![1],
                _ => vec![],
            }
        }
    }

    #[test]
    fn unreachable_blocks_omitted() {
        let po = postorder(&WithDead);
        assert!(!po.contains(&9));
        assert_eq!(po.len(), 2);
    }
}
