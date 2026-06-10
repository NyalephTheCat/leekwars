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

use std::collections::HashMap;

use crate::ir::{BlockId, Const, MirFunction, MirProgram, Operand, Terminator};

/// Optimize every function in `program` in place.
pub fn optimize_program(program: &mut MirProgram) {
    for f in &mut program.functions {
        optimize_function(f);
    }
}

/// Run the per-function passes: simplify constant terminators, then prune the
/// blocks that became unreachable.
pub fn optimize_function(f: &mut MirFunction) {
    simplify_const_terminators(f);
    remove_unreachable_blocks(f);
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
}
