//! Well-formedness verification of MIR.
//!
//! MIR is consumed by the code-generation backends — `leek-backend-native`
//! (Cranelift) and `leek-backend-java`. Both *assume* a set of
//! structural invariants and index straight into `blocks` / terminator targets
//! without bounds checks. Historically those invariants were implicit: a
//! malformed function was only discovered when a backend panicked or
//! miscompiled. This module makes the contract explicit and checkable.
//!
//! ## Invariants checked (per [`MirFunction`])
//!
//! 1. **Entry in range** — `entry` indexes a real block.
//! 2. **Block-index consistency** — `blocks[i].id == BlockId(i)`, so
//!    `MirFunction::block(id)` (which indexes by `id.0`) is correct.
//! 3. **Terminator targets in range** — every `Goto` / `Branch` / `Switch`
//!    target (and the switch `default`) indexes a real block.
//!
//! These are the load-bearing CFG invariants both backends rely on. Operand /
//! place-level checks (valid `LocalId`s inside rvalues) are intentionally out
//! of scope for this first verifier; they can be layered on later.
//!
//! The verifier is pure and allocation-light; callers can run it behind a
//! `debug_assert!` at the lowering boundary to catch malformed IR at
//! construction rather than as a downstream backend crash.

use crate::ir::{BlockId, MirFunction, MirProgram, Terminator};

/// A well-formedness violation found by [`verify_function`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirError {
    /// Name of the offending function (for diagnostics).
    pub function: String,
    /// Human-readable description of the violation.
    pub message: String,
}

impl std::fmt::Display for MirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed MIR in `{}`: {}", self.function, self.message)
    }
}

/// Verify the structural CFG invariants of a single function.
pub fn verify_function(func: &MirFunction) -> Result<(), MirError> {
    let nblocks = func.blocks.len();
    let mk = |message: String| MirError {
        function: func.name.clone(),
        message,
    };

    if nblocks == 0 {
        return Err(mk("function has no basic blocks".to_string()));
    }
    if func.entry.0 as usize >= nblocks {
        return Err(mk(format!(
            "entry block bb{} is out of range (only {nblocks} blocks)",
            func.entry.0
        )));
    }

    for (i, block) in func.blocks.iter().enumerate() {
        if block.id.0 as usize != i {
            return Err(mk(format!(
                "block at index {i} has id bb{} (index/id mismatch)",
                block.id.0
            )));
        }
        let check = |target: BlockId| -> Result<(), MirError> {
            if (target.0 as usize) < nblocks {
                Ok(())
            } else {
                Err(mk(format!(
                    "block bb{i} jumps to out-of-range block bb{}",
                    target.0
                )))
            }
        };
        match &block.terminator {
            Terminator::Goto(b) => check(*b)?,
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                check(*then_block)?;
                check(*else_block)?;
            }
            Terminator::Switch { arms, default, .. } => {
                for (_, b) in arms {
                    check(*b)?;
                }
                check(*default)?;
            }
            Terminator::Return(_) | Terminator::Unreachable => {}
        }
    }
    Ok(())
}

/// Verify every function in a program. Returns the first violation found.
pub fn verify_program(program: &MirProgram) -> Result<(), MirError> {
    for func in &program.functions {
        verify_function(func)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, FunctionKind, MirFunction};
    use leek_span::Span;
    use leek_types::Type;

    fn func(blocks: Vec<BasicBlock>, entry: u32) -> MirFunction {
        MirFunction {
            def_id: None,
            kind: FunctionKind::Main,
            name: "test".to_string(),
            params: Vec::new(),
            return_ty: Type::Void,
            locals: Vec::new(),
            blocks,
            entry: BlockId(entry),
            owning_class: None,
            span: Span::synthetic(),
        }
    }

    fn block(id: u32, term: Terminator) -> BasicBlock {
        BasicBlock {
            id: BlockId(id),
            statements: Vec::new(),
            statement_spans: Vec::new(),
            terminator: term,
            terminator_span: Span::synthetic(),
        }
    }

    #[test]
    fn well_formed_function_verifies() {
        let f = func(
            vec![
                block(0, Terminator::Goto(BlockId(1))),
                block(1, Terminator::Return(None)),
            ],
            0,
        );
        assert!(verify_function(&f).is_ok());
    }

    #[test]
    fn out_of_range_jump_is_rejected() {
        let f = func(vec![block(0, Terminator::Goto(BlockId(9)))], 0);
        let err = verify_function(&f).unwrap_err();
        assert!(err.message.contains("out-of-range"), "{}", err.message);
    }

    #[test]
    fn out_of_range_entry_is_rejected() {
        let f = func(vec![block(0, Terminator::Return(None))], 5);
        assert!(verify_function(&f).is_err());
    }

    #[test]
    fn block_index_mismatch_is_rejected() {
        // block at index 0 claims id bb3.
        let f = func(vec![block(3, Terminator::Return(None))], 0);
        assert!(verify_function(&f).is_err());
    }

    #[test]
    fn switch_targets_are_checked() {
        let f = func(
            vec![block(
                0,
                Terminator::Switch {
                    discriminant: crate::ir::Operand::Const(crate::ir::Const::Int(0)),
                    arms: vec![(crate::ir::Const::Int(1), BlockId(2))],
                    default: BlockId(0),
                },
            )],
            0,
        );
        // arm targets bb2 which doesn't exist.
        assert!(verify_function(&f).is_err());
    }
}
