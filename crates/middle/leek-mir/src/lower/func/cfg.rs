//! Per-function CFG lowering.

use std::collections::HashMap;

use leek_diagnostics::Diagnostic;
use leek_hir::{DefId, HirFile};
use leek_span::Span;
use leek_types::Type;

use crate::ir::{
    BasicBlock, BlockId, FunctionKind, LocalDecl, LocalId, LocalKind, MirFunction, Statement,
    Terminator,
};

use super::{FnLowerer, PendingLambda};
// `FnLowerer::new` / `finish` and block-local helpers live in this module.

impl<'a> FnLowerer<'a> {
    pub(crate) fn new(
        hir: &'a HirFile,
        globals: &'a HashMap<DefId, String>,
        errors: &'a mut Vec<Diagnostic>,
        program_functions: &'a mut Vec<MirFunction>,
        pending_lambdas: &'a mut Vec<PendingLambda>,
        kind: FunctionKind,
        name: String,
        def_id: Option<DefId>,
        fn_span: Span,
        return_ty: Type,
    ) -> Self {
        let mut me = Self {
            hir,
            globals,
            errors,
            program_functions,
            pending_lambdas,
            kind,
            name,
            def_id,
            return_ty,
            fn_span,
            locals: Vec::new(),
            blocks: Vec::new(),
            params: Vec::new(),
            local_map: HashMap::new(),
            captures: Vec::new(),
            method_ctx: None,
            current: None,
            loop_stack: Vec::new(),
            cur_span: fn_span,
        };
        let entry = me.new_block();
        me.current = Some(entry);
        me
    }

    pub(crate) fn finish(self) -> MirFunction {
        let owning_class = self.method_ctx.as_ref().map(|m| m.class_def_id);
        MirFunction {
            def_id: self.def_id,
            kind: self.kind,
            name: self.name,
            params: self.params,
            return_ty: self.return_ty,
            locals: self.locals,
            blocks: self.blocks,
            entry: BlockId(0),
            owning_class,
            span: self.fn_span,
        }
    }

    // ---- Block / local management ----

    pub(crate) fn new_block(&mut self) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).expect("more than u32::MAX blocks"));
        self.blocks.push(BasicBlock {
            id,
            statements: Vec::new(),
            statement_spans: Vec::new(),
            terminator: Terminator::Unreachable,
            terminator_span: Span::synthetic(),
        });
        id
    }

    pub(crate) fn declare_local(
        &mut self,
        name: Option<String>,
        ty: Type,
        kind: LocalKind,
        span: Span,
    ) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).expect("more than u32::MAX locals"));
        self.locals.push(LocalDecl {
            name,
            ty,
            kind,
            span,
            default_init: None,
            inferred_ty: None,
            is_shared: false,
            is_by_ref: false,
        });
        id
    }

    pub(crate) fn fresh_temp(&mut self, ty: Type, span: Span) -> LocalId {
        self.declare_local(None, ty, LocalKind::Temp, span)
    }

    /// Get the block currently being built. Panics if there isn't
    /// one — callers must check `is_open` first when they care.
    pub(crate) fn cur(&mut self) -> BlockId {
        self.current
            .expect("attempted to append to a terminated block — open a new one first")
    }

    pub(crate) fn is_open(&self) -> bool {
        self.current.is_some()
    }

    pub(crate) fn push_stmt(&mut self, s: Statement) {
        let id = self.cur();
        let block = &mut self.blocks[id.0 as usize];
        block.statements.push(s);
        block.statement_spans.push(self.cur_span);
    }

    pub(crate) fn set_terminator(&mut self, t: Terminator) {
        let id = self.cur();
        let block = &mut self.blocks[id.0 as usize];
        block.terminator = t;
        block.terminator_span = self.cur_span;
        self.current = None;
    }

    /// Terminate the current block (if any) with a goto to `target`.
    /// No-op when the current block is already closed.
    pub(crate) fn goto(&mut self, target: BlockId) {
        if self.is_open() {
            self.set_terminator(Terminator::Goto(target));
        }
    }

    /// Resume work in `block`. Must be called after every
    /// terminator emission so subsequent statements know where to
    /// land.
    pub(crate) fn resume(&mut self, block: BlockId) {
        debug_assert!(self.current.is_none(), "resuming over an open block");
        self.current = Some(block);
    }

    /// Close the function with an implicit `return null` if the
    /// last block is still open. Functions that explicitly return
    /// (or `main` lowered from a file ending in a return) skip the
    /// implicit terminator.
    pub(crate) fn close_with_implicit_return(&mut self, _span: Span) {
        if self.is_open() {
            self.set_terminator(Terminator::Return(None));
        }
    }
}
