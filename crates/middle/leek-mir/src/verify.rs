//! Well-formedness verification of MIR.
//!
//! MIR is consumed by the code-generation backends — `leek-backend-native`
//! (Cranelift) and `leek-backend-java`. Both *assume* a set of
//! structural invariants and index straight into `blocks`, `locals`,
//! `program.functions` and terminator targets without bounds checks.
//! Historically those invariants were implicit: a malformed function was
//! only discovered when a backend panicked or miscompiled. This module
//! makes the contract explicit and checkable.
//!
//! ## Invariants checked (per [`MirFunction`])
//!
//! 1. **Entry in range** — `entry` indexes a real block.
//! 2. **Block-index consistency** — `blocks[i].id == BlockId(i)`, so
//!    [`MirFunction::block`] (which indexes by `id.0`) is correct.
//! 3. **Terminator targets in range** — every `Goto` / `Branch` / `Switch`
//!    target (and the switch `default`) indexes a real block.
//! 4. **Locals in range** — every [`LocalId`] reachable from a statement,
//!    a place, an rvalue (including through [`Rvalue::Synthetic`]), a
//!    callee or a terminator indexes a real slot in `locals`, so
//!    [`MirFunction::local`] and the backends' `locals[id.0]` are correct.
//! 5. **Local metadata** — every id in `params` names a
//!    [`LocalKind::Param`] slot, and every `default_init` names a real
//!    block.
//! 6. **Statement-span parity** — `statement_spans` is either empty or
//!    exactly as long as `statements`. *Partial* drift would silently
//!    misattribute source lines in the native debug backend.
//! 7. **No reachable `Unreachable`** — no block reachable from the entry
//!    (or from a parameter's `default_init`) still carries the
//!    placeholder terminator `new_block` seeds every block with.
//!
//! ## Invariants checked per [`MirProgram`] (see [`verify_program_refs`])
//!
//! 8. **Function indices in range** — `MakeLambda.function_idx`,
//!    `MirMethod::function_idx` (methods and constructors),
//!    `MirField::init_fn`, `FieldSlot::init_fn` and
//!    `VtableSlot::function_idx` all index real functions.
//! 9. **No unpatched lambda placeholders** — every reserved slot was
//!    overwritten with a real body (see
//!    [`placeholder_function`](crate::lower::util::placeholder_function)).
//! 10. **Layout slots are positions** — `field_layout[i].slot == i` and
//!     `vtable[i].slot == i`, which is what `compute_class_layouts`
//!     promises and what every `field_slot` caller assumes.
//!
//! The verifier is pure and allocation-light — one reusable scratch
//! buffer per function, not one per statement. Callers run it behind a
//! `debug_assert!` at the lowering boundary to catch malformed IR at
//! construction rather than as a downstream backend crash; the
//! [`VerifyMir`](crate::pipeline::VerifyMir) pipeline step runs it in
//! release builds and reports a [`Diagnostic`](leek_diagnostics::Diagnostic)
//! instead.

use leek_span::Span;

use crate::ir::{
    BlockId, Callee, IntervalRvalue, LocalId, LocalKind, MirClass, MirFunction, MirProgram,
    Operand, Place, Rvalue, SliceBounds, Statement, Terminator,
};

/// Name given to a reserved-but-not-yet-lowered lambda / method slot in
/// `MirProgram::functions`. Shared with
/// [`placeholder_function`](crate::lower::util::placeholder_function) so
/// the literal cannot drift away from the check that no such slot
/// survives lowering.
pub(crate) const LAMBDA_PLACEHOLDER_NAME: &str = "<lambda-placeholder>";

/// A well-formedness violation found by [`verify_function`] or
/// [`verify_program_refs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirError {
    /// Name of the offending function — or of the offending class, for
    /// the class-table checks in [`verify_program_refs`], which have no
    /// single function to blame.
    pub function: String,
    /// Human-readable description of the violation.
    pub message: String,
    /// Best available source location: the offending statement's span
    /// where one exists, else the enclosing function's or class's.
    /// [`Span::synthetic`] only when the IR carried nothing better.
    pub span: Span,
}

impl std::fmt::Display for MirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed MIR in `{}`: {}", self.function, self.message)
    }
}

impl leek_diagnostics::IntoDiagnostic for MirError {
    fn into_diagnostic(self) -> leek_diagnostics::Diagnostic {
        leek_diagnostics::convert::malformed_mir(self.span, self.to_string())
    }
}

/// Verify every invariant of a single function: the CFG shape, local
/// references, local metadata and statement-span parity.
///
/// Program-level references (function indices, class layout tables) need
/// the whole [`MirProgram`] — see [`verify_program_refs`].
pub fn verify_function(func: &MirFunction) -> Result<(), MirError> {
    verify_cfg(func)?;
    verify_spans(func)?;
    verify_locals_meta(func)?;
    verify_locals(func)?;
    verify_no_reachable_unreachable(func)?;
    Ok(())
}

/// Verify every function in a program, then the program-level
/// cross-references. Returns the first violation found.
pub fn verify_program(program: &MirProgram) -> Result<(), MirError> {
    for func in &program.functions {
        verify_function(func)?;
    }
    verify_program_refs(program)?;
    Ok(())
}

// ---- Per-function checks ----

fn err(func: &MirFunction, span: Span, message: String) -> MirError {
    MirError {
        function: func.name.clone(),
        message,
        span,
    }
}

/// Invariants 1-3: block count, entry, block-index consistency and
/// terminator targets.
fn verify_cfg(func: &MirFunction) -> Result<(), MirError> {
    let nblocks = func.blocks.len();
    if nblocks == 0 {
        return Err(err(
            func,
            func.span,
            "function has no basic blocks".to_string(),
        ));
    }
    if func.entry.0 as usize >= nblocks {
        return Err(err(
            func,
            func.span,
            format!(
                "entry block bb{} is out of range (only {nblocks} blocks)",
                func.entry.0
            ),
        ));
    }

    for (i, block) in func.blocks.iter().enumerate() {
        if block.id.0 as usize != i {
            return Err(err(
                func,
                block.terminator_span,
                format!(
                    "block at index {i} has id bb{} (index/id mismatch)",
                    block.id.0
                ),
            ));
        }
        let check = |target: BlockId| -> Result<(), MirError> {
            if (target.0 as usize) < nblocks {
                Ok(())
            } else {
                Err(err(
                    func,
                    block.terminator_span,
                    format!("block bb{i} jumps to out-of-range block bb{}", target.0),
                ))
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

/// Invariant 6. Deliberately tolerant of a *wholly* empty
/// `statement_spans`: blocks built outside the lowering (synthetic
/// thunks, test helpers) legitimately leave it empty and every reader
/// falls back when `statement_spans.get(i)` is `None`. What no reader
/// can survive is a *partially* filled vector, where `get(i)` silently
/// returns some other statement's span.
fn verify_spans(func: &MirFunction) -> Result<(), MirError> {
    for block in &func.blocks {
        let (spans, stmts) = (block.statement_spans.len(), block.statements.len());
        if !block.statement_spans.is_empty() && spans != stmts {
            return Err(err(
                func,
                block.terminator_span,
                format!(
                    "block bb{} has {spans} statement spans for {stmts} statements \
                     (must be empty or exactly parallel)",
                    block.id.0
                ),
            ));
        }
    }
    Ok(())
}

/// Invariant 5: `params` name real `Param`-kind slots and `default_init`
/// names a real block.
fn verify_locals_meta(func: &MirFunction) -> Result<(), MirError> {
    let (nlocals, nblocks) = (func.locals.len(), func.blocks.len());
    for (i, param) in func.params.iter().enumerate() {
        let Some(decl) = func.locals.get(param.0 as usize) else {
            return Err(err(
                func,
                func.span,
                format!(
                    "param {i} is out-of-range local _{} (only {nlocals} locals)",
                    param.0
                ),
            ));
        };
        if decl.kind != LocalKind::Param {
            return Err(err(
                func,
                decl.span,
                format!(
                    "param {i} names local _{}, which is declared {:?}, not Param",
                    param.0, decl.kind
                ),
            ));
        }
    }
    for (i, decl) in func.locals.iter().enumerate() {
        if let Some(block) = decl.default_init
            && block.0 as usize >= nblocks
        {
            return Err(err(
                func,
                decl.span,
                format!(
                    "local _{i} defaults from out-of-range block bb{} (only {nblocks} blocks)",
                    block.0
                ),
            ));
        }
    }
    Ok(())
}

/// Invariant 7: no *reachable* block keeps the `Unreachable` placeholder
/// terminator `new_block` seeds every block with.
///
/// `Unreachable` is legitimate on a block nothing jumps to — the lowering
/// opens a fresh block after closing one with `return`, and that block
/// stays predecessor-less if the source had nothing after the return. A
/// *reachable* one means the lowering opened a block, wired an edge into
/// it and then forgot to close it, which the backends turn into a trap on
/// a path the program actually takes.
///
/// Reachability is seeded the way [`crate::opt::remove_unreachable_blocks`]
/// seeds it: the entry, plus every parameter's `default_init` block — a
/// default-init block is entered when a caller omits that argument, so it
/// is reachable with no CFG edge pointing at it.
fn verify_no_reachable_unreachable(func: &MirFunction) -> Result<(), MirError> {
    let n = func.blocks.len();
    let mut reachable = vec![false; n];
    let mut stack: Vec<BlockId> = vec![func.entry];
    for local in &func.locals {
        if let Some(b) = local.default_init {
            stack.push(b);
        }
    }
    while let Some(b) = stack.pop() {
        let idx = b.0 as usize;
        // `verify_cfg` already rejected out-of-range targets; be defensive
        // anyway so the order of the checks is not load-bearing.
        if idx >= n || reachable[idx] {
            continue;
        }
        reachable[idx] = true;
        match &func.blocks[idx].terminator {
            Terminator::Goto(t) => stack.push(*t),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                stack.push(*then_block);
                stack.push(*else_block);
            }
            Terminator::Switch { arms, default, .. } => {
                for (_, t) in arms {
                    stack.push(*t);
                }
                stack.push(*default);
            }
            Terminator::Return(_) | Terminator::Unreachable => {}
        }
    }

    for (i, block) in func.blocks.iter().enumerate() {
        if reachable[i] && matches!(block.terminator, Terminator::Unreachable) {
            return Err(err(
                func,
                block.terminator_span,
                format!(
                    "reachable block bb{} still carries the `Unreachable` placeholder \
                     terminator (the lowering opened it and never closed it)",
                    block.id.0
                ),
            ));
        }
    }
    Ok(())
}

/// Invariant 4: every `LocalId` a statement or terminator reaches is in
/// range.
fn verify_locals(func: &MirFunction) -> Result<(), MirError> {
    let nlocals = func.locals.len();
    let mut refs = Refs::default();
    for block in &func.blocks {
        for (i, stmt) in block.statements.iter().enumerate() {
            refs.clear();
            statement_refs(stmt, &mut refs);
            if let Some(bad) = refs.first_out_of_range_local(nlocals) {
                return Err(err(
                    func,
                    block.statement_spans.get(i).copied().unwrap_or(func.span),
                    format!(
                        "bb{}[{i}] references out-of-range local _{} (only {nlocals} locals)",
                        block.id.0, bad.0
                    ),
                ));
            }
        }
        refs.clear();
        terminator_refs(&block.terminator, &mut refs);
        if let Some(bad) = refs.first_out_of_range_local(nlocals) {
            return Err(err(
                func,
                block.terminator_span,
                format!(
                    "the terminator of bb{} references out-of-range local _{} \
                     (only {nlocals} locals)",
                    block.id.0, bad.0
                ),
            ));
        }
    }
    Ok(())
}

// ---- Program-level checks ----

/// Invariants 8-10: cross-references that only make sense against the
/// whole program — function indices, unpatched lambda placeholders and
/// class layout slot numbering.
pub fn verify_program_refs(program: &MirProgram) -> Result<(), MirError> {
    let nfns = program.functions.len();

    for func in &program.functions {
        if func.name == LAMBDA_PLACEHOLDER_NAME {
            return Err(err(
                func,
                func.span,
                "a reserved lambda / method slot was never patched with its \
                 lowered body"
                    .to_string(),
            ));
        }
        let mut refs = Refs::default();
        for block in &func.blocks {
            for (i, stmt) in block.statements.iter().enumerate() {
                refs.clear();
                statement_refs(stmt, &mut refs);
                if let Some(bad) = refs.functions.iter().copied().find(|idx| *idx >= nfns) {
                    return Err(err(
                        func,
                        block.statement_spans.get(i).copied().unwrap_or(func.span),
                        format!(
                            "bb{}[{i}] builds a closure over out-of-range function #{bad} \
                             (only {nfns} functions)",
                            block.id.0
                        ),
                    ));
                }
            }
        }
    }

    for class in &program.classes {
        verify_class_refs(class, nfns)?;
    }
    Ok(())
}

fn verify_class_refs(class: &MirClass, nfns: usize) -> Result<(), MirError> {
    let bad = |span: Span, message: String| MirError {
        function: class.name.clone(),
        message,
        span,
    };
    let in_range = |idx: usize| idx < nfns;

    for (label, methods) in [
        ("method", &class.methods),
        ("constructor", &class.constructors),
    ] {
        for m in methods {
            if !in_range(m.function_idx) {
                return Err(bad(
                    m.span,
                    format!(
                        "{label} `{}` points at out-of-range function #{} (only {nfns} functions)",
                        m.name, m.function_idx
                    ),
                ));
            }
        }
    }

    for (label, fields) in [
        ("instance field", &class.instance_fields),
        ("static field", &class.static_fields),
    ] {
        for f in fields {
            if let Some(idx) = f.init_fn
                && !in_range(idx)
            {
                return Err(bad(
                    f.span,
                    format!(
                        "{label} `{}` initializes from out-of-range function #{idx} \
                         (only {nfns} functions)",
                        f.name
                    ),
                ));
            }
        }
    }

    for (i, slot) in class.field_layout.iter().enumerate() {
        if slot.slot != i {
            return Err(bad(
                class.span,
                format!(
                    "field_layout[{i}] (`{}`) claims slot {} — layout slots must equal \
                     their positions",
                    slot.name, slot.slot
                ),
            ));
        }
        if let Some(idx) = slot.init_fn
            && !in_range(idx)
        {
            return Err(bad(
                class.span,
                format!(
                    "field_layout[{i}] (`{}`) initializes from out-of-range function \
                     #{idx} (only {nfns} functions)",
                    slot.name
                ),
            ));
        }
    }

    for (i, slot) in class.vtable.iter().enumerate() {
        if slot.slot != i {
            return Err(bad(
                class.span,
                format!(
                    "vtable[{i}] (`{}`) claims slot {} — vtable slots must equal their \
                     positions",
                    slot.name, slot.slot
                ),
            ));
        }
        if !in_range(slot.function_idx) {
            return Err(bad(
                class.span,
                format!(
                    "vtable[{i}] (`{}`) points at out-of-range function #{} (only {nfns} \
                     functions)",
                    slot.name, slot.function_idx
                ),
            ));
        }
    }
    Ok(())
}

// ---- Reference walk ----

/// Scratch buffer for one exhaustive walk of a statement or terminator.
///
/// Reused across a function's statements (`clear` between nodes) so the
/// walk costs amortized zero allocations — the verifier runs on every
/// `lower_file` in debug builds and over the whole corpus.
#[derive(Default)]
struct Refs {
    locals: Vec<LocalId>,
    /// Indices into `MirProgram::functions` (today: `MakeLambda`).
    functions: Vec<usize>,
}

impl Refs {
    fn clear(&mut self) {
        self.locals.clear();
        self.functions.clear();
    }

    fn first_out_of_range_local(&self, nlocals: usize) -> Option<LocalId> {
        self.locals
            .iter()
            .copied()
            .find(|id| id.0 as usize >= nlocals)
    }
}

fn statement_refs(stmt: &Statement, out: &mut Refs) {
    match stmt {
        Statement::Assign(place, rvalue) => {
            place_refs(place, out);
            rvalue_refs(rvalue, out);
        }
        Statement::Call { dest, call } => {
            if let Some(place) = dest {
                place_refs(place, out);
            }
            match &call.callee {
                Callee::Function(_) | Callee::Builtin(_) => {}
                Callee::Method { receiver, .. } => out.locals.push(*receiver),
                Callee::Indirect(callee) => out.locals.push(*callee),
                Callee::SuperConstructor { this, .. } => out.locals.push(*this),
            }
            for arg in &call.args {
                operand_refs(arg, out);
            }
        }
        Statement::Charge(_) | Statement::ChargeVersioned { .. } => {}
        Statement::ApplyPromotion(local) => out.locals.push(*local),
    }
}

fn terminator_refs(term: &Terminator, out: &mut Refs) {
    match term {
        Terminator::Goto(_) | Terminator::Unreachable => {}
        Terminator::Branch { cond, .. } => operand_refs(cond, out),
        Terminator::Return(value) => {
            if let Some(op) = value {
                operand_refs(op, out);
            }
        }
        Terminator::Switch { discriminant, .. } => operand_refs(discriminant, out),
    }
}

fn operand_refs(op: &Operand, out: &mut Refs) {
    match op {
        Operand::Local(id) => out.locals.push(*id),
        Operand::Const(_) => {}
    }
}

fn optional_operand_refs(op: Option<&Operand>, out: &mut Refs) {
    if let Some(op) = op {
        operand_refs(op, out);
    }
}

fn slice_refs(bounds: &SliceBounds, out: &mut Refs) {
    optional_operand_refs(bounds.start.as_ref(), out);
    optional_operand_refs(bounds.end.as_ref(), out);
    optional_operand_refs(bounds.step.as_ref(), out);
}

fn interval_refs(interval: &IntervalRvalue, out: &mut Refs) {
    optional_operand_refs(interval.start.as_ref(), out);
    optional_operand_refs(interval.end.as_ref(), out);
    optional_operand_refs(interval.step.as_ref(), out);
}

fn place_refs(place: &Place, out: &mut Refs) {
    match place {
        Place::Local(id) => out.locals.push(*id),
        Place::Global(_, _) => {}
        Place::Field(base, _) => out.locals.push(*base),
        Place::Index(base, index) => {
            out.locals.push(*base);
            operand_refs(index, out);
        }
        Place::Slice(base, bounds) => {
            out.locals.push(*base);
            slice_refs(bounds, out);
        }
        Place::LambdaCapture { lambda, .. } => out.locals.push(*lambda),
    }
}

/// The exhaustive match, and the reason there is no `_ =>` arm anywhere
/// in this walk: a new [`Rvalue`] variant that carries a [`LocalId`] must
/// be a compile error here, not a silent hole in the verifier.
fn rvalue_refs(rvalue: &Rvalue, out: &mut Refs) {
    match rvalue {
        Rvalue::Use(op) | Rvalue::UseFresh(op) | Rvalue::MakeForeachIter(op) => {
            operand_refs(op, out);
        }
        Rvalue::Binary(_, lhs, rhs) => {
            operand_refs(lhs, out);
            operand_refs(rhs, out);
        }
        Rvalue::Unary(_, op) | Rvalue::Cast(_, op) => operand_refs(op, out),
        Rvalue::Field(base, _) | Rvalue::ForeachLen(base) => out.locals.push(*base),
        Rvalue::Index(base, index)
        | Rvalue::ForeachValueAt(base, index)
        | Rvalue::ForeachKeyAt(base, index) => {
            out.locals.push(*base);
            operand_refs(index, out);
        }
        Rvalue::Slice(base, bounds) => {
            out.locals.push(*base);
            slice_refs(bounds, out);
        }
        Rvalue::Array(items) => {
            for op in items {
                operand_refs(op, out);
            }
        }
        Rvalue::Map(pairs) => {
            for (k, v) in pairs {
                operand_refs(k, out);
                operand_refs(v, out);
            }
        }
        Rvalue::Set(elems) => {
            for elem in elems {
                for op in elem.operands() {
                    operand_refs(op, out);
                }
            }
        }
        Rvalue::Object(fields) => {
            for (_, op) in fields {
                operand_refs(op, out);
            }
        }
        Rvalue::New { args, .. } => {
            for op in args {
                operand_refs(op, out);
            }
        }
        Rvalue::Interval(interval) => interval_refs(interval, out),
        Rvalue::Synthetic(inner) => rvalue_refs(inner, out),
        Rvalue::MakeLambda {
            function_idx,
            captures,
        } => {
            out.functions.push(*function_idx);
            for op in captures {
                operand_refs(op, out);
            }
        }
        Rvalue::MakeSuper { this, .. } => out.locals.push(*this),
        Rvalue::FunctionRef(_)
        | Rvalue::GlobalRef(_, _)
        | Rvalue::BuiltinRef(_)
        | Rvalue::This
        | Rvalue::ClassSelf
        | Rvalue::Super
        | Rvalue::ClassRef(_, _)
        | Rvalue::Unsupported(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        BasicBlock, CallExpr, Const, FieldSlot, FunctionKind, LocalDecl, MirClass, MirMethod,
        MirProgram, Visibility, VtableSlot,
    };
    use leek_hir::DefId;
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

    fn local(kind: LocalKind) -> LocalDecl {
        LocalDecl {
            name: None,
            ty: Type::Any,
            kind,
            span: Span::synthetic(),
            default_init: None,
            inferred_ty: None,
            is_shared: false,
            is_by_ref: false,
        }
    }

    /// One block holding `stmts`, one `Temp` local (`_0`), returning void.
    fn func_with(stmts: Vec<Statement>) -> MirFunction {
        let mut f = func(vec![block(0, Terminator::Return(None))], 0);
        f.locals = vec![local(LocalKind::Temp)];
        f.blocks[0].statement_spans = vec![Span::synthetic(); stmts.len()];
        f.blocks[0].statements = stmts;
        f
    }

    /// `_0 = <rvalue>` — the shortest statement that carries an rvalue.
    fn assign(rvalue: Rvalue) -> Statement {
        Statement::Assign(Place::Local(LocalId(0)), rvalue)
    }

    fn program(functions: Vec<MirFunction>, classes: Vec<MirClass>) -> MirProgram {
        MirProgram {
            functions,
            classes,
            globals: Vec::new(),
        }
    }

    // ---- CFG checks (invariants 1-3) ----

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
                    discriminant: Operand::Const(Const::Int(0)),
                    arms: vec![(Const::Int(1), BlockId(2))],
                    default: BlockId(0),
                },
            )],
            0,
        );
        // arm targets bb2 which doesn't exist.
        assert!(verify_function(&f).is_err());
    }

    // ---- Local bounds (invariant 4) ----

    #[test]
    fn out_of_range_local_in_operand_is_rejected() {
        let f = func_with(vec![assign(Rvalue::Use(Operand::Local(LocalId(7))))]);
        let err = verify_function(&f).unwrap_err();
        assert!(
            err.message.contains("out-of-range local _7"),
            "{}",
            err.message
        );
    }

    #[test]
    fn out_of_range_local_in_place_is_rejected() {
        let f = func_with(vec![Statement::Assign(
            Place::Field(LocalId(9), "x".into()),
            Rvalue::Use(Operand::Const(Const::Int(1))),
        )]);
        let err = verify_function(&f).unwrap_err();
        assert!(
            err.message.contains("out-of-range local _9"),
            "{}",
            err.message
        );
    }

    #[test]
    fn out_of_range_local_in_callee_is_rejected() {
        let f = func_with(vec![Statement::Call {
            dest: None,
            call: CallExpr {
                callee: Callee::Method {
                    receiver: LocalId(9),
                    method: "m".into(),
                },
                args: Vec::new(),
                span: Span::synthetic(),
            },
        }]);
        let err = verify_function(&f).unwrap_err();
        assert!(
            err.message.contains("out-of-range local _9"),
            "{}",
            err.message
        );
    }

    #[test]
    fn out_of_range_local_in_nested_synthetic_rvalue_is_rejected() {
        // Proves the walk recurses into `Synthetic`'s boxed rvalue — the
        // foreach machinery wraps nearly every generated read in one.
        let f = func_with(vec![assign(Rvalue::Synthetic(Box::new(Rvalue::Index(
            LocalId(9),
            Operand::Const(Const::Int(0)),
        ))))]);
        let err = verify_function(&f).unwrap_err();
        assert!(
            err.message.contains("out-of-range local _9"),
            "{}",
            err.message
        );
    }

    #[test]
    fn out_of_range_local_in_branch_cond_is_rejected() {
        let mut f = func_with(Vec::new());
        f.blocks[0].terminator = Terminator::Branch {
            cond: Operand::Local(LocalId(4)),
            then_block: BlockId(0),
            else_block: BlockId(0),
        };
        let err = verify_function(&f).unwrap_err();
        assert!(
            err.message.contains("terminator of bb0") && err.message.contains("_4"),
            "{}",
            err.message
        );
    }

    #[test]
    fn out_of_range_local_in_switch_discriminant_is_rejected() {
        let mut f = func_with(Vec::new());
        f.blocks[0].terminator = Terminator::Switch {
            discriminant: Operand::Local(LocalId(4)),
            arms: Vec::new(),
            default: BlockId(0),
        };
        let err = verify_function(&f).unwrap_err();
        assert!(err.message.contains("_4"), "{}", err.message);
    }

    #[test]
    fn in_range_locals_are_accepted() {
        let f = func_with(vec![assign(Rvalue::Use(Operand::Local(LocalId(0))))]);
        assert!(verify_function(&f).is_ok());
    }

    // ---- Statement-span parity (invariant 6) ----

    #[test]
    fn partial_statement_spans_are_rejected() {
        let mut f = func_with(vec![
            assign(Rvalue::Use(Operand::Const(Const::Int(1)))),
            assign(Rvalue::Use(Operand::Const(Const::Int(2)))),
        ]);
        f.blocks[0].statement_spans.pop();
        let err = verify_function(&f).unwrap_err();
        assert!(
            err.message.contains("1 statement spans for 2 statements"),
            "{}",
            err.message
        );
    }

    #[test]
    fn empty_statement_spans_are_accepted() {
        // Synthetic functions (thunks, the lambda placeholder, test
        // helpers) legitimately carry no spans at all. Pinned so a later
        // tightening to strict equality is a deliberate, visible change.
        let mut f = func_with(vec![
            assign(Rvalue::Use(Operand::Const(Const::Int(1)))),
            assign(Rvalue::Use(Operand::Const(Const::Int(2)))),
        ]);
        f.blocks[0].statement_spans.clear();
        assert!(verify_function(&f).is_ok());
    }

    // ---- Local metadata (invariant 5) ----

    #[test]
    fn non_param_kind_in_params_is_rejected() {
        let mut f = func_with(Vec::new());
        // `_0` is a Temp, not a Param.
        f.params = vec![LocalId(0)];
        let err = verify_function(&f).unwrap_err();
        assert!(err.message.contains("not Param"), "{}", err.message);
    }

    #[test]
    fn out_of_range_param_is_rejected() {
        let mut f = func_with(Vec::new());
        f.params = vec![LocalId(3)];
        let err = verify_function(&f).unwrap_err();
        assert!(err.message.contains("param 0"), "{}", err.message);
    }

    #[test]
    fn param_kind_local_in_params_is_accepted() {
        let mut f = func_with(Vec::new());
        f.locals = vec![local(LocalKind::Param)];
        f.params = vec![LocalId(0)];
        assert!(verify_function(&f).is_ok());
    }

    #[test]
    fn out_of_range_default_init_is_rejected() {
        let mut f = func_with(Vec::new());
        f.locals[0].default_init = Some(BlockId(7));
        let err = verify_function(&f).unwrap_err();
        assert!(err.message.contains("bb7"), "{}", err.message);
    }

    // ---- Reporting ----

    #[test]
    fn a_violation_converts_to_an_e0302_diagnostic_at_the_offending_span() {
        use leek_diagnostics::IntoDiagnostic;

        let at = Span::new(leek_span::SourceId::new(1).unwrap(), 17, 23);
        let mut f = func_with(vec![assign(Rvalue::Use(Operand::Local(LocalId(7))))]);
        f.blocks[0].statement_spans = vec![at];

        let d = verify_function(&f).unwrap_err().into_diagnostic();
        assert_eq!(d.code.id(), "E0302");
        assert_eq!(d.severity, leek_diagnostics::Severity::Error);
        // The span is the offending *statement*, not the whole function —
        // that is what makes the release-mode `VerifyMir` report usable.
        assert_eq!(d.span, at);
        assert!(
            d.message.contains("malformed MIR in `test`"),
            "{}",
            d.message
        );
    }

    // ---- Reachable `Unreachable` (invariant 7) ----

    #[test]
    fn reachable_unreachable_terminator_is_rejected() {
        // bb0 -> bb1, and bb1 was never closed.
        let f = func(
            vec![
                block(0, Terminator::Goto(BlockId(1))),
                block(1, Terminator::Unreachable),
            ],
            0,
        );
        let err = verify_function(&f).unwrap_err();
        assert!(
            err.message.contains("reachable block bb1"),
            "{}",
            err.message
        );
    }

    #[test]
    fn unreachable_terminator_in_a_dead_block_is_accepted() {
        // Nothing jumps to bb1: the lowering legitimately opens a block
        // after closing one with `return` and leaves it predecessor-less.
        let f = func(
            vec![
                block(0, Terminator::Return(None)),
                block(1, Terminator::Unreachable),
            ],
            0,
        );
        assert_eq!(verify_function(&f), Ok(()));
    }

    #[test]
    fn a_default_init_block_counts_as_reachable() {
        // bb1 has no CFG edge into it, but a caller that omits the
        // parameter enters it — so an unclosed bb1 is still a bug.
        let mut f = func(
            vec![
                block(0, Terminator::Return(None)),
                block(1, Terminator::Unreachable),
            ],
            0,
        );
        f.locals = vec![local(LocalKind::Param)];
        f.params = vec![LocalId(0)];
        f.locals[0].default_init = Some(BlockId(1));
        let err = verify_function(&f).unwrap_err();
        assert!(
            err.message.contains("reachable block bb1"),
            "{}",
            err.message
        );
    }

    // ---- Program-level references (invariants 8-10) ----

    fn method(name: &str, function_idx: usize) -> MirMethod {
        MirMethod {
            name: name.to_string(),
            function_idx,
            is_static: false,
            user_arity: 0,
            visibility: Visibility::Public,
            span: Span::synthetic(),
        }
    }

    fn class(name: &str) -> MirClass {
        MirClass {
            def_id: DefId(1),
            name: name.to_string(),
            parent: None,
            parent_def: None,
            instance_fields: Vec::new(),
            static_fields: Vec::new(),
            methods: Vec::new(),
            constructors: Vec::new(),
            field_layout: Vec::new(),
            vtable: Vec::new(),
            span: Span::synthetic(),
        }
    }

    #[test]
    fn out_of_range_make_lambda_function_idx_is_rejected() {
        let f = func_with(vec![assign(Rvalue::MakeLambda {
            function_idx: 5,
            captures: Vec::new(),
        })]);
        // The function alone verifies — only the program knows how many
        // functions exist.
        assert!(verify_function(&f).is_ok());
        let err = verify_program(&program(vec![f], Vec::new())).unwrap_err();
        assert!(
            err.message.contains("out-of-range function #5"),
            "{}",
            err.message
        );
    }

    #[test]
    fn unreplaced_lambda_placeholder_is_rejected() {
        let mut placeholder = func(vec![block(0, Terminator::Return(None))], 0);
        placeholder.name = LAMBDA_PLACEHOLDER_NAME.to_string();
        let p = program(
            vec![
                func(vec![block(0, Terminator::Return(None))], 0),
                placeholder,
            ],
            Vec::new(),
        );
        let err = verify_program(&p).unwrap_err();
        assert!(err.message.contains("never patched"), "{}", err.message);
    }

    #[test]
    fn out_of_range_method_function_idx_is_rejected() {
        let mut c = class("Foo");
        c.methods = vec![method("bar", 3)];
        let p = program(
            vec![func(vec![block(0, Terminator::Return(None))], 0)],
            vec![c],
        );
        let err = verify_program(&p).unwrap_err();
        assert_eq!(err.function, "Foo");
        assert!(
            err.message.contains("method `bar`") && err.message.contains("#3"),
            "{}",
            err.message
        );
    }

    #[test]
    fn vtable_slot_index_mismatch_is_rejected() {
        let mut c = class("Foo");
        c.vtable = vec![VtableSlot {
            name: "bar".into(),
            slot: 4,
            function_idx: 0,
            user_arity: 0,
            visibility: Visibility::Public,
            owner: DefId(1),
        }];
        let p = program(
            vec![func(vec![block(0, Terminator::Return(None))], 0)],
            vec![c],
        );
        let err = verify_program(&p).unwrap_err();
        assert!(err.message.contains("claims slot 4"), "{}", err.message);
    }

    #[test]
    fn field_layout_slot_index_mismatch_is_rejected() {
        let mut c = class("Foo");
        c.field_layout = vec![FieldSlot {
            name: "x".into(),
            slot: 2,
            ty: Type::Any,
            is_final: false,
            init_fn: None,
            owner: DefId(1),
        }];
        let p = program(
            vec![func(vec![block(0, Terminator::Return(None))], 0)],
            vec![c],
        );
        let err = verify_program(&p).unwrap_err();
        assert!(err.message.contains("claims slot 2"), "{}", err.message);
    }

    #[test]
    fn well_formed_program_with_lambdas_and_classes_verifies() {
        // Positive control: the whole shape the rejection tests poke at,
        // assembled correctly, must pass.
        let lambda = func(vec![block(0, Terminator::Return(None))], 0);
        let main = func_with(vec![assign(Rvalue::MakeLambda {
            function_idx: 1,
            captures: vec![Operand::Local(LocalId(0))],
        })]);
        let mut c = class("Foo");
        c.methods = vec![method("bar", 1)];
        c.vtable = vec![VtableSlot {
            name: "bar".into(),
            slot: 0,
            function_idx: 1,
            user_arity: 0,
            visibility: Visibility::Public,
            owner: DefId(1),
        }];
        c.field_layout = vec![FieldSlot {
            name: "x".into(),
            slot: 0,
            ty: Type::Any,
            is_final: false,
            init_fn: None,
            owner: DefId(1),
        }];
        let p = program(vec![main, lambda], vec![c]);
        assert_eq!(verify_program(&p), Ok(()));
    }
}
