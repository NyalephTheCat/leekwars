//! Control-flow-graph view of a [`MirFunction`].
//!
//! Binds the per-function CFG to the generic [`leek_visit::cfg`]
//! framework, so the standard graph algorithms — [`postorder`],
//! [`reverse_postorder`], [`predecessors`] — are available without
//! re-implementing them per consumer (the native backend, future
//! dataflow passes). Successors are derived from each block's
//! [`Terminator`].
//!
//! [`postorder`]: leek_visit::cfg::postorder
//! [`reverse_postorder`]: leek_visit::cfg::reverse_postorder
//! [`predecessors`]: leek_visit::cfg::predecessors

use leek_visit::cfg::Cfg;

use crate::ir::{BlockId, MirFunction, Terminator};

impl Cfg for MirFunction {
    type BlockId = BlockId;

    fn entry(&self) -> BlockId {
        self.entry
    }

    fn block_ids(&self) -> Vec<BlockId> {
        self.blocks.iter().map(|b| b.id).collect()
    }

    fn successors(&self, block: BlockId) -> Vec<BlockId> {
        let Some(b) = self.blocks.iter().find(|b| b.id == block) else {
            return Vec::new();
        };
        match &b.terminator {
            Terminator::Goto(target) => vec![*target],
            // `then` before `else` — significant for deterministic RPO.
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
            // Arms in source order, then the default.
            Terminator::Switch { arms, default, .. } => {
                let mut succs: Vec<BlockId> = arms.iter().map(|(_, bb)| *bb).collect();
                succs.push(*default);
                succs
            }
            Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, FunctionKind};
    use leek_span::Span;
    use leek_types::Type;
    use leek_visit::cfg::{predecessors, reverse_postorder};

    fn block(id: u32, term: Terminator) -> BasicBlock {
        BasicBlock {
            id: BlockId(id),
            statements: Vec::new(),
            statement_spans: Vec::new(),
            terminator: term,
            terminator_span: leek_span::Span::synthetic(),
        }
    }

    /// Diamond: bb0 -Branch-> {bb1, bb2} -Goto-> bb3 -Return.
    fn diamond() -> MirFunction {
        MirFunction {
            def_id: None,
            kind: FunctionKind::Main,
            name: "main".into(),
            params: Vec::new(),
            return_ty: Type::Any,
            locals: Vec::new(),
            blocks: vec![
                block(
                    0,
                    Terminator::Branch {
                        cond: crate::ir::Operand::Const(crate::ir::Const::Bool(true)),
                        then_block: BlockId(1),
                        else_block: BlockId(2),
                    },
                ),
                block(1, Terminator::Goto(BlockId(3))),
                block(2, Terminator::Goto(BlockId(3))),
                block(3, Terminator::Return(None)),
            ],
            entry: BlockId(0),
            owning_class: None,
            span: Span::new(leek_span::SourceId::new(1).unwrap(), 0, 0),
        }
    }

    #[test]
    fn rpo_entry_first_join_last() {
        let f = diamond();
        let rpo = reverse_postorder(&f);
        assert_eq!(rpo.first(), Some(&BlockId(0)), "entry first");
        assert_eq!(rpo.last(), Some(&BlockId(3)), "join last");
        assert_eq!(rpo.len(), 4);
    }

    #[test]
    fn join_block_has_both_predecessors() {
        let f = diamond();
        let preds = predecessors(&f);
        let mut p = preds[&BlockId(3)].clone();
        p.sort_by_key(|b| b.0);
        assert_eq!(p, vec![BlockId(1), BlockId(2)]);
    }
}
