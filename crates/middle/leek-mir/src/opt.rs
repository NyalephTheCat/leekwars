//! Backend-agnostic MIR optimization passes.
//!
//! These run after lowering, gated on the recipe's
//! [`OptLevel`](leek_pipeline::OptLevel), and only ever shrink a function's
//! CFG — they never change its observable behavior. Two passes, run per
//! function:
//!
//! 1. **Constant-branch simplification** — a [`Terminator::Branch`] on a
//!    constant boolean (or a [`Terminator::Switch`] on a constant that exactly
//!    matches an arm) becomes an unconditional [`Terminator::Goto`]. This drops
//!    the branch operation the interpreter would charge for, and exposes the
//!    not-taken successor as dead code. It composes with HIR constant folding:
//!    `if (DEBUG)` where `DEBUG` folded to a literal becomes straight-line flow.
//!
//! 2. **Unreachable-block elimination** — blocks no longer reachable from the
//!    entry (nor from any parameter default-initializer block) are removed, and
//!    the survivors are renumbered so the positional `blocks[i].id == BlockId(i)`
//!    invariant [`verify`](crate::verify) checks still holds.
//!
//! The passes preserve [`MirFunction`] well-formedness; callers run
//! [`verify_program`](crate::verify::verify_program) after to assert it.

use std::collections::{HashMap, HashSet};

use leek_pipeline::{Fuel, OptConfig, OptLevel};

use crate::ir::{BlockId, Const, MirFunction, MirProgram, Operand, Terminator};

/// Optimize every function in `program` in place at the default
/// [`OptLevel::O1`] pass set. Thin wrapper over [`optimize_program_with`].
pub fn optimize_program(program: &mut MirProgram) {
    optimize_program_with(program, &OptConfig::for_level(OptLevel::O1));
}

/// Optimize every function in `program` per `cfg`. A single fuel budget is
/// shared across all functions, so a finite budget bounds total work.
pub fn optimize_program_with(program: &mut MirProgram, cfg: &OptConfig) {
    let mut fuel = cfg.fuel;
    for f in &mut program.functions {
        optimize_function_with(f, cfg, &mut fuel);
    }
}

/// Run the per-function passes once at [`OptLevel::O1`]. Thin wrapper kept for
/// callers/tests that don't carry an [`OptConfig`].
pub fn optimize_function(f: &mut MirFunction) {
    let mut fuel = Fuel::Unlimited;
    optimize_function_with(f, &OptConfig::for_level(OptLevel::O1), &mut fuel);
}

/// Run the enabled MIR passes on `f` to a fixpoint (or until `fuel` runs out):
/// constant-terminator simplification, optional jump-threading, then
/// unreachable-block pruning. Returns the number of rewrites.
///
/// At [`OptLevel::O1`] only `simplify_const_terminators` + `remove_unreachable_blocks`
/// run; the loop reaches its fixpoint after one effective pass (block removal
/// exposes no new constant terminator), so O1 output matches the previous
/// single-pass behavior.
pub fn optimize_function_with(f: &mut MirFunction, cfg: &OptConfig, fuel: &mut Fuel) -> usize {
    const MAX_ROUNDS: usize = 16;
    let mut total = 0;
    for _ in 0..MAX_ROUNDS {
        if !fuel.available() {
            break;
        }
        let mut changed = 0;
        if cfg.mir_const_branch && fuel.available() {
            let c = simplify_const_terminators(f);
            fuel.spend_n(c);
            changed += c;
        }
        if cfg.mir_jump_thread && fuel.available() {
            let c = thread_jumps(f);
            fuel.spend_n(c);
            changed += c;
        }
        // Structural cleanup — not fuel-charged; run last so it prunes blocks the
        // other passes just made unreachable.
        if cfg.mir_unreachable {
            changed += remove_unreachable_blocks(f);
        }
        total += changed;
        if changed == 0 {
            break;
        }
    }
    total
}

/// Rewrite terminators whose control flow is statically determined to an
/// unconditional [`Terminator::Goto`]. Returns the number of terminators
/// rewritten.
fn simplify_const_terminators(f: &mut MirFunction) -> usize {
    let mut changed = 0;
    for block in &mut f.blocks {
        let new_term = match &block.terminator {
            // `if (true)` / `if (false)` — only a *boolean* constant is folded;
            // other constants would need the interpreter's truthiness coercion,
            // which we deliberately don't replicate here.
            Terminator::Branch {
                cond: Operand::Const(Const::Bool(b)),
                then_block,
                else_block,
            } => Some(Terminator::Goto(if *b { *then_block } else { *else_block })),
            // Both arms go to the same block — the condition is irrelevant (and
            // its operand is side-effect-free, already in a temp), so drop the
            // branch. Saves the runtime branch op the interpreter charges.
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } if then_block == else_block => Some(Terminator::Goto(*then_block)),
            // `switch (k)` on a constant: jump straight to the arm whose key is
            // exactly equal. We only fold an exact match — if none matches we
            // leave the switch alone rather than assume the default, since arm
            // matching may use looser equality than `Const`'s structural `Eq`.
            Terminator::Switch {
                discriminant: Operand::Const(disc),
                arms,
                ..
            } => arms
                .iter()
                .find(|(key, _)| key == disc)
                .map(|(_, target)| Terminator::Goto(*target)),
            _ => None,
        };
        if let Some(term) = new_term {
            block.terminator = term;
            changed += 1;
        }
    }
    changed
}

/// Remove blocks unreachable from the entry (or from any parameter
/// default-initializer block) and renumber the survivors. Returns the number of
/// blocks removed.
fn remove_unreachable_blocks(f: &mut MirFunction) -> usize {
    let n = f.blocks.len();
    if n == 0 {
        return 0;
    }

    // Roots: the entry, plus every parameter default-init block. A default-init
    // block is entered when a caller omits that argument, so it is reachable
    // independently of the CFG edges from `entry`.
    let mut reachable = vec![false; n];
    let mut stack: Vec<BlockId> = vec![f.entry];
    for local in &f.locals {
        if let Some(b) = local.default_init {
            stack.push(b);
        }
    }

    while let Some(b) = stack.pop() {
        let idx = b.0 as usize;
        if idx >= n || reachable[idx] {
            continue;
        }
        reachable[idx] = true;
        for succ in successors(&f.blocks[idx].terminator) {
            stack.push(succ);
        }
    }

    if reachable.iter().all(|&r| r) {
        return 0; // nothing to remove — avoid the rebuild + remap.
    }

    // old BlockId index → new sequential index, keeping surviving blocks in
    // their original relative order for deterministic output.
    let mut remap: HashMap<u32, u32> = HashMap::new();
    let mut next = 0u32;
    for (i, &keep) in (0u32..).zip(&reachable) {
        if keep {
            remap.insert(i, next);
            next += 1;
        }
    }

    let removed = n - next as usize;

    // Rebuild the block list with remapped ids + terminator targets.
    let old_blocks = std::mem::take(&mut f.blocks);
    for (i, mut block) in (0u32..).zip(old_blocks) {
        if !reachable[i as usize] {
            continue;
        }
        block.id = BlockId(remap[&i]);
        block.terminator = remap_terminator(block.terminator, &remap);
        f.blocks.push(block);
    }

    f.entry = BlockId(remap[&f.entry.0]);
    for local in &mut f.locals {
        if let Some(b) = local.default_init {
            // A default-init root is always reachable, so the remap has it.
            local.default_init = Some(BlockId(remap[&b.0]));
        }
    }

    removed
}

/// The successor block ids of a terminator (mirrors the [`Cfg`](leek_visit::cfg::Cfg)
/// impl, but operates directly on a borrowed terminator).
fn successors(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Goto(b) => vec![*b],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        Terminator::Switch { arms, default, .. } => {
            let mut s: Vec<BlockId> = arms.iter().map(|(_, b)| *b).collect();
            s.push(*default);
            s
        }
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}

/// Apply the old→new block-id remap to a terminator's targets.
fn remap_terminator(term: Terminator, remap: &HashMap<u32, u32>) -> Terminator {
    let m = |b: BlockId| BlockId(remap[&b.0]);
    match term {
        Terminator::Goto(b) => Terminator::Goto(m(b)),
        Terminator::Branch {
            cond,
            then_block,
            else_block,
        } => Terminator::Branch {
            cond,
            then_block: m(then_block),
            else_block: m(else_block),
        },
        Terminator::Switch {
            discriminant,
            arms,
            default,
        } => Terminator::Switch {
            discriminant,
            arms: arms.into_iter().map(|(k, b)| (k, m(b))).collect(),
            default: m(default),
        },
        Terminator::Return(op) => Terminator::Return(op),
        Terminator::Unreachable => Terminator::Unreachable,
    }
}

/// Jump-threading / empty-block merging (O3): redirect every terminator target
/// that points at an **empty** block whose terminator is an unconditional
/// `Goto` straight to that goto's destination, following chains to their end.
/// The bypassed blocks become unreachable and are pruned by the
/// [`remove_unreachable_blocks`] pass that runs after. Returns the number of
/// targets rewritten.
///
/// Only *empty* blocks are threaded, so `Charge` / `ChargeVersioned` statements
/// (the op-budget ticks) are never dropped. Parameter default-init blocks are
/// left as roots (a caller may enter them directly), and self-loops are skipped
/// so the chain walk terminates.
fn thread_jumps(f: &mut MirFunction) -> usize {
    let init_roots: HashSet<BlockId> = f.locals.iter().filter_map(|l| l.default_init).collect();
    let mut redirect: HashMap<BlockId, BlockId> = HashMap::new();
    for b in &f.blocks {
        if b.statements.is_empty()
            && !init_roots.contains(&b.id)
            && let Terminator::Goto(t) = &b.terminator
            && *t != b.id
        {
            redirect.insert(b.id, *t);
        }
    }
    if redirect.is_empty() {
        return 0;
    }
    // Follow a chain of empty-goto blocks to its final destination, bounded by
    // the map size so a cycle of empty blocks can't loop forever.
    let resolve = |start: BlockId| -> BlockId {
        let mut cur = start;
        for _ in 0..=redirect.len() {
            match redirect.get(&cur) {
                Some(&next) if next != cur => cur = next,
                _ => break,
            }
        }
        cur
    };

    let mut changed = 0;
    for b in &mut f.blocks {
        let (new_term, n) = resolve_terminator(&b.terminator, &resolve);
        if n > 0 {
            b.terminator = new_term;
            changed += n;
        }
    }
    let new_entry = resolve(f.entry);
    if new_entry != f.entry {
        f.entry = new_entry;
        changed += 1;
    }
    changed
}

/// Apply `resolve` to each target of `term`, returning the rewritten terminator
/// and how many targets actually changed.
fn resolve_terminator(
    term: &Terminator,
    resolve: &impl Fn(BlockId) -> BlockId,
) -> (Terminator, usize) {
    let mut n = 0;
    let mut m = |b: BlockId| {
        let r = resolve(b);
        if r != b {
            n += 1;
        }
        r
    };
    let new = match term {
        Terminator::Goto(b) => Terminator::Goto(m(*b)),
        Terminator::Branch {
            cond,
            then_block,
            else_block,
        } => Terminator::Branch {
            cond: cond.clone(),
            then_block: m(*then_block),
            else_block: m(*else_block),
        },
        Terminator::Switch {
            discriminant,
            arms,
            default,
        } => Terminator::Switch {
            discriminant: discriminant.clone(),
            arms: arms.iter().map(|(k, b)| (k.clone(), m(*b))).collect(),
            default: m(*default),
        },
        Terminator::Return(op) => Terminator::Return(op.clone()),
        Terminator::Unreachable => Terminator::Unreachable,
    };
    (new, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, FunctionKind};
    use crate::verify::verify_function;
    use leek_span::Span;
    use leek_types::Type;

    fn block(id: u32, term: Terminator) -> BasicBlock {
        BasicBlock {
            id: BlockId(id),
            statements: Vec::new(),
            statement_spans: Vec::new(),
            terminator: term,
            terminator_span: Span::synthetic(),
        }
    }

    fn func(blocks: Vec<BasicBlock>, entry: u32) -> MirFunction {
        MirFunction {
            def_id: None,
            kind: FunctionKind::Main,
            name: "test".into(),
            params: Vec::new(),
            return_ty: Type::Void,
            locals: Vec::new(),
            blocks,
            entry: BlockId(entry),
            owning_class: None,
            span: Span::synthetic(),
        }
    }

    #[test]
    fn const_true_branch_becomes_goto_then() {
        let mut f = func(
            vec![
                block(
                    0,
                    Terminator::Branch {
                        cond: Operand::Const(Const::Bool(true)),
                        then_block: BlockId(1),
                        else_block: BlockId(2),
                    },
                ),
                block(1, Terminator::Return(None)),
                block(2, Terminator::Return(None)),
            ],
            0,
        );
        optimize_function(&mut f);
        verify_function(&f).expect("well-formed after opt");
        // bb2 (the else) is now unreachable and removed → 2 blocks remain.
        assert_eq!(f.blocks.len(), 2);
        // The entry now gotos the (renumbered) then-block.
        assert!(matches!(f.blocks[0].terminator, Terminator::Goto(_)));
    }

    #[test]
    fn const_false_branch_drops_then_block() {
        let mut f = func(
            vec![
                block(
                    0,
                    Terminator::Branch {
                        cond: Operand::Const(Const::Bool(false)),
                        then_block: BlockId(1),
                        else_block: BlockId(2),
                    },
                ),
                block(1, Terminator::Return(None)), // dead `then`
                block(2, Terminator::Return(None)),
            ],
            0,
        );
        optimize_function(&mut f);
        verify_function(&f).expect("well-formed");
        assert_eq!(f.blocks.len(), 2, "dead then-block removed");
    }

    #[test]
    fn switch_on_const_jumps_to_matching_arm() {
        let mut f = func(
            vec![
                block(
                    0,
                    Terminator::Switch {
                        discriminant: Operand::Const(Const::Int(2)),
                        arms: vec![(Const::Int(1), BlockId(1)), (Const::Int(2), BlockId(2))],
                        default: BlockId(3),
                    },
                ),
                block(1, Terminator::Return(None)),
                block(2, Terminator::Return(None)),
                block(3, Terminator::Return(None)),
            ],
            0,
        );
        optimize_function(&mut f);
        verify_function(&f).expect("well-formed");
        // Only the entry and the matching arm survive.
        assert_eq!(f.blocks.len(), 2);
        assert!(matches!(f.blocks[0].terminator, Terminator::Goto(_)));
    }

    #[test]
    fn branch_with_identical_targets_becomes_goto() {
        // A non-constant condition whose arms both go to bb1 still collapses.
        let mut f = func(
            vec![
                block(
                    0,
                    Terminator::Branch {
                        cond: Operand::Local(crate::ir::LocalId(0)),
                        then_block: BlockId(1),
                        else_block: BlockId(1),
                    },
                ),
                block(1, Terminator::Return(None)),
            ],
            0,
        );
        optimize_function(&mut f);
        verify_function(&f).expect("well-formed");
        assert_eq!(f.blocks.len(), 2);
        assert!(
            matches!(f.blocks[0].terminator, Terminator::Goto(_)),
            "identical-target branch collapsed to goto"
        );
    }

    #[test]
    fn non_constant_branch_is_left_alone() {
        let mut f = func(
            vec![
                block(
                    0,
                    Terminator::Branch {
                        cond: Operand::Local(crate::ir::LocalId(0)),
                        then_block: BlockId(1),
                        else_block: BlockId(2),
                    },
                ),
                block(1, Terminator::Return(None)),
                block(2, Terminator::Return(None)),
            ],
            0,
        );
        optimize_function(&mut f);
        verify_function(&f).expect("well-formed");
        assert_eq!(f.blocks.len(), 3, "no block removed");
        assert!(matches!(f.blocks[0].terminator, Terminator::Branch { .. }));
    }

    #[test]
    fn loop_back_edge_keeps_blocks_reachable() {
        // bb0 -> bb1 -> bb1 (self-loop via const-true branch back edge)
        let mut f = func(
            vec![
                block(0, Terminator::Goto(BlockId(1))),
                block(
                    1,
                    Terminator::Branch {
                        cond: Operand::Const(Const::Bool(true)),
                        then_block: BlockId(1),
                        else_block: BlockId(2),
                    },
                ),
                block(2, Terminator::Return(None)),
            ],
            0,
        );
        optimize_function(&mut f);
        verify_function(&f).expect("well-formed");
        // bb2 becomes unreachable (branch always loops); bb0 + bb1 remain.
        assert_eq!(f.blocks.len(), 2);
    }

    #[test]
    fn jump_threading_bypasses_empty_blocks_at_o3() {
        // bb0 branches to two empty blocks that both `Goto` the return block.
        let mut f = func(
            vec![
                block(
                    0,
                    Terminator::Branch {
                        cond: Operand::Local(crate::ir::LocalId(0)),
                        then_block: BlockId(1),
                        else_block: BlockId(2),
                    },
                ),
                block(1, Terminator::Goto(BlockId(3))),
                block(2, Terminator::Goto(BlockId(3))),
                block(3, Terminator::Return(None)),
            ],
            0,
        );
        let mut fuel = Fuel::Unlimited;
        optimize_function_with(&mut f, &OptConfig::for_level(OptLevel::O3), &mut fuel);
        verify_function(&f).expect("well-formed after opt");
        // bb1/bb2 (empty gotos) are bypassed; the branch's arms then coincide and
        // collapse to a goto, which is itself an empty entry block that threads
        // straight to the return — the whole function reduces to one block.
        assert_eq!(f.blocks.len(), 1);
        assert!(matches!(f.blocks[0].terminator, Terminator::Return(_)));
    }

    #[test]
    fn jump_threading_is_off_below_o3() {
        // An empty-goto chain that O1 leaves intact (jump-threading is O3-only).
        let mut f = func(
            vec![
                block(0, Terminator::Goto(BlockId(1))),
                block(1, Terminator::Goto(BlockId(2))),
                block(2, Terminator::Return(None)),
            ],
            0,
        );
        let mut fuel = Fuel::Unlimited;
        optimize_function_with(&mut f, &OptConfig::for_level(OptLevel::O1), &mut fuel);
        verify_function(&f).expect("well-formed");
        assert_eq!(f.blocks.len(), 3, "no jump-threading at O1");
    }

    #[test]
    fn fuel_zero_blocks_mir_rewrites() {
        // With an exhausted budget, even a constant branch is left untouched.
        let mut f = func(
            vec![
                block(
                    0,
                    Terminator::Branch {
                        cond: Operand::Const(Const::Bool(true)),
                        then_block: BlockId(1),
                        else_block: BlockId(2),
                    },
                ),
                block(1, Terminator::Return(None)),
                block(2, Terminator::Return(None)),
            ],
            0,
        );
        let mut fuel = Fuel::Limited(0);
        let n = optimize_function_with(&mut f, &OptConfig::for_level(OptLevel::O3), &mut fuel);
        assert_eq!(n, 0, "no fuel → no rewrites");
        assert_eq!(f.blocks.len(), 3, "branch and blocks untouched");
    }
}
