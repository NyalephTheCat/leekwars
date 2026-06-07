//! Lower HIR into MIR.
//!
//! Walks a [`HirFile`], producing a [`MirProgram`] with one
//! [`MirFunction`] per user-defined function plus a synthetic
//! `main` function for the file's top-level statements.
//!
//! Three responsibilities live in this pass:
//!
//! 1. **Flatten expressions.** Every sub-expression becomes either a
//!    constant operand or a local temp assigned via `Statement::Assign`.
//!    `Rvalue` therefore only ever sees flat `Operand`s.
//!
//! 2. **Build the CFG.** `if` / `while` / `for` / `do-while` /
//!    `foreach` / `switch` / `break` / `continue` / `return` become
//!    explicit basic blocks and terminators. There is no
//!    fall-through between blocks — every block ends in a
//!    [`Terminator`].
//!
//! 3. **Lower short-circuit operators.** `&&`, `||`, `??`, and
//!    `?:` ternary all turn into branch terminators with both arms
//!    assigning into the same temp before joining. We are not in
//!    SSA, so two writes into the same local at a join point is
//!    fine.
//!
//! Compound assignments (`+=`, `??=`, etc.) and `++` / `--` are
//! desugared here into a read-modify-write sequence. This frees the
//! interpreter and native backend from having to model them.
//!
//! ## Not yet lowered
//!
//! Classes, lambdas with captures, and `super` dispatch are
//! recognised but emit [`Rvalue::Unsupported`] markers. They have
//! their own design pass coming.

use std::collections::HashMap;

use leek_diagnostics::Diagnostic;
use leek_hir::{DefId, HirFile};
use leek_span::Span;
use leek_types::Type;

use crate::ir::{BasicBlock, BlockId, FunctionKind, LocalDecl, LocalId, MirFunction, MirProgram};

mod func;
mod program;
mod util;

/// Lower a HIR file into a MIR program.
///
/// The result also contains lowering [`Diagnostic`]s for shapes that
/// couldn't be fully modeled. The program is still returned — unsupported
/// sites are marked with [`Rvalue::Unsupported`] where applicable.
pub fn lower_file(hir: &HirFile) -> (MirProgram, Vec<Diagnostic>) {
    let mut ctx = ProgramCtx::new(hir);
    ctx.lower();
    // Catch malformed IR (bad block ids / out-of-range jumps) at construction
    // in debug/test builds, instead of as a downstream backend panic or
    // miscompile. Compiled out in release. See `crate::verify`.
    #[cfg(debug_assertions)]
    if let Err(e) = crate::verify::verify_program(&ctx.program) {
        panic!("{e}");
    }
    (ctx.program, ctx.errors)
}

// ---- Program-level context ----

pub(crate) struct ProgramCtx<'a> {
    pub(crate) hir: &'a HirFile,
    pub(crate) program: MirProgram,
    pub(crate) errors: Vec<Diagnostic>,
    /// `DefId` of every top-level global we've registered, mapped to
    /// the global's source name (used when building `Place::Global`
    /// / `Rvalue::GlobalRef`).
    pub(crate) globals: HashMap<DefId, String>,
    /// Lambdas reserved during lowering but not yet processed.
    /// Drained after main is lowered; each task lowers a closure
    /// body into the function slot it reserved.
    pub(crate) pending_lambdas: Vec<PendingLambda>,
}

/// One closure body waiting to be lowered. Created when a parent
/// FnLowerer encounters `ExprKind::Lambda`. The slot at
/// `function_idx` is already reserved in `program.functions`
/// (filled with a placeholder) so MakeLambda can reference it
/// immediately; `lower_pending_lambda` later replaces the
/// placeholder with the real MirFunction.
#[derive(Clone)]
pub(crate) struct MethodCtx {
    /// `None` for static methods. For instance methods, this is
    /// the LocalId of the synthetic first parameter holding
    /// `this`.
    this_local: Option<LocalId>,
    class_def_id: DefId,
    class_name: String,
    parent_class: Option<String>,
}

pub(crate) struct PendingLambda {
    function_idx: usize,
    lambda: leek_hir::LambdaExpr,
    /// Captured `DefId`s in slot order — the first `captures.len()`
    /// params of the lambda's MirFunction are these, in the same
    /// order they appear in the parent's `MakeLambda` operands.
    captures: Vec<DefId>,
    /// When the lambda is lowered inside a method body, the outer
    /// method's `MethodCtx` carries through so `this` / `Class_` /
    /// `super` references inside the lambda resolve correctly.
    /// `this` is captured implicitly as an additional first capture
    /// slot (before the by-DefId captures), and `this_local` in the
    /// rebuilt `MethodCtx` points at that slot.
    method_ctx: Option<MethodCtx>,
    /// True iff the lambda body references `this`/`super`/`Class_`
    /// (or rewrote field/method names to use `this`). When true and
    /// `method_ctx` is also set, the lambda gets an implicit `this`
    /// capture as its first slot.
    needs_this: bool,
    span: Span,
}

// ---- Per-function lowering ----

pub(crate) struct FnLowerer<'a> {
    pub(crate) hir: &'a HirFile,
    pub(crate) globals: &'a HashMap<DefId, String>,
    pub(crate) errors: &'a mut Vec<Diagnostic>,
    pub(crate) program_functions: &'a mut Vec<MirFunction>,
    pub(crate) pending_lambdas: &'a mut Vec<PendingLambda>,
    pub(crate) kind: FunctionKind,
    pub(crate) name: String,
    pub(crate) def_id: Option<DefId>,
    pub(crate) return_ty: Type,
    pub(crate) fn_span: Span,
    pub(crate) locals: Vec<LocalDecl>,
    pub(crate) blocks: Vec<BasicBlock>,
    pub(crate) params: Vec<LocalId>,
    pub(crate) local_map: HashMap<DefId, LocalId>,
    pub(crate) captures: Vec<DefId>,
    pub(crate) method_ctx: Option<MethodCtx>,
    pub(crate) current: Option<BlockId>,
    pub(crate) loop_stack: Vec<LoopCtx>,
    /// Source span of the HIR statement currently being lowered.
    /// `push_stmt` stamps each emitted MIR statement with it, giving the
    /// native debug backend a source line per statement.
    pub(crate) cur_span: Span,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LoopCtx {
    pub(crate) continue_target: BlockId,
    pub(crate) break_target: BlockId,
}
