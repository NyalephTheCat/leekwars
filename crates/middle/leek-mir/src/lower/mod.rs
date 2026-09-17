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
//! backends from having to model them.
//!
//! ## Unsupported markers
//!
//! [`Rvalue::Unsupported`](crate::ir::Rvalue::Unsupported) is not a "not yet implemented" marker —
//! classes, lambdas with captures and `super` dispatch all lower. It
//! marks the shapes that are *errors*: an unbound local, `super`
//! outside a method or inside a static, and a `this(...)` call. Each
//! site also emits a diagnostic, and the program is still returned so
//! the caller sees every error rather than only the first.

use std::collections::HashMap;

use leek_diagnostics::Diagnostic;
use leek_hir::{DefId, HirFile};
use leek_query::OptLevel;
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
/// sites are marked with [`Rvalue::Unsupported`](crate::ir::Rvalue::Unsupported) where applicable.
pub fn lower_file(hir: &HirFile) -> (MirProgram, Vec<Diagnostic>) {
    let mut ctx = ProgramCtx::new(hir);
    ctx.lower();
    // Catch malformed IR at construction in debug/test builds, instead of as
    // a downstream backend panic or miscompile: bad block ids, out-of-range
    // jumps, out-of-range locals in any place / rvalue / callee / terminator,
    // drifted statement-span parity, non-`Param` params, and dangling
    // function indices or unpatched lambda placeholders. Compiled out in
    // release. See `crate::verify`.
    //
    // A panic, not a diagnostic, and deliberately so: malformed MIR is a
    // compiler bug with no user-actionable span, and downgrading it here
    // would let the bad IR reach the backends' unchecked `functions[idx]` /
    // `locals[id.0]` indexing — a worse panic, further from the cause.
    // Release-mode *reporting* is what `lower_and_optimize` is for.
    #[cfg(debug_assertions)]
    if let Err(e) = crate::verify::verify_program(&ctx.program) {
        panic!("{e}");
    }
    (ctx.program, ctx.errors)
}

/// Lower a HIR file into MIR, run the backend-agnostic passes if `opt`
/// asks for them, and check the result's structural invariants.
///
/// The pure entry point behind
/// [`lower_mir_query`](crate::query::lower_mir_query) and
/// `leek_db::queries::lower_program_mir`. The returned diagnostics are
/// [`lower_file`]'s own, plus an `E0302` if the program came out
/// malformed.
///
/// That last check is the release-mode counterpart of the
/// `debug_assert` inside [`lower_file`]: in a debug build the assert has
/// already panicked on a malformed program, so the diagnostic is what a
/// release build reports instead of letting bad IR reach the backends'
/// unchecked indexing. It runs *after* the optimization passes, so it
/// covers what those produce too — which is what [`opt`](crate::opt)
/// asks its callers for.
pub fn lower_and_optimize(hir: &HirFile, opt: OptLevel) -> (MirProgram, Vec<Diagnostic>) {
    let (mut program, mut diagnostics) = lower_file(hir);
    if opt.optimizes() {
        crate::optimize_program(&mut program);
    }
    diagnostics.extend(verify_diagnostic(&program));
    (program, diagnostics)
}

/// The `E0302` for a malformed program, or `None` when it verifies.
fn verify_diagnostic(program: &MirProgram) -> Option<Diagnostic> {
    crate::verify::verify_program(program)
        .err()
        .map(leek_diagnostics::IntoDiagnostic::into_diagnostic)
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
    /// Whether the body is a constructor. `class` is late-bound in an
    /// instance method but not here: the receiver is still being built
    /// when the parameter defaults are filled, so reading its runtime
    /// class would read a half-made object.
    is_constructor: bool,
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

#[cfg(test)]
mod verify_tests {
    use leek_diagnostics::Diagnostic;
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_query::OptLevel;
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    use super::{lower_and_optimize, verify_diagnostic};
    use crate::ir::{BasicBlock, BlockId, FunctionKind, MirFunction, MirProgram, Terminator};

    fn e0302_count(diags: &[Diagnostic]) -> usize {
        diags.iter().filter(|d| d.code.id() == "E0302").count()
    }

    fn hir(src: &str) -> leek_hir::HirFile {
        let source = SourceId::new(1).unwrap();
        let parsed = leek_parser::parse_with_features(
            src,
            source,
            Version::V4,
            leek_parser::ParseFeatures::default(),
        );
        let ast = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("source file root");
        let (hir, _) = leek_hir::lower_file_versioned(&ast, source, 4);
        hir
    }

    /// A deliberately malformed program: bb0 jumps to a block that does not
    /// exist. Lowering cannot produce one — its own debug assert fires
    /// first — so handing a program straight to the verifier is the only way
    /// to see the release-mode report [`lower_and_optimize`] appends.
    fn malformed() -> MirProgram {
        let bad = MirFunction {
            def_id: None,
            kind: FunctionKind::Main,
            name: "main".to_string(),
            params: Vec::new(),
            return_ty: leek_types::Type::Void,
            locals: Vec::new(),
            blocks: vec![BasicBlock {
                id: BlockId(0),
                statements: Vec::new(),
                statement_spans: Vec::new(),
                // bb9 does not exist.
                terminator: Terminator::Goto(BlockId(9)),
                terminator_span: leek_span::Span::synthetic(),
            }],
            entry: BlockId(0),
            owning_class: None,
            span: leek_span::Span::synthetic(),
        };
        MirProgram {
            functions: vec![bad],
            globals: Vec::new(),
            classes: Vec::new(),
        }
    }

    #[test]
    fn a_malformed_program_is_reported_instead_of_panicking() {
        let diags: Vec<Diagnostic> = verify_diagnostic(&malformed()).into_iter().collect();
        assert_eq!(
            e0302_count(&diags),
            1,
            "expected one malformed-MIR diagnostic, got {diags:?}"
        );
        assert!(
            diags[0].message.contains("out-of-range block bb9"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn a_well_formed_program_lowers_and_optimizes_without_a_report() {
        let file = hir(
            "class A { real x = 1 int f(n) { for (var i in [1, 2]) { n = n + i } return n } }\n\
             var g = x -> x + 1\nvar a = new A()\nvar y = g(a.f(2))\n",
        );
        for opt in [OptLevel::O0, OptLevel::O1] {
            let (_program, diags) = lower_and_optimize(&file, opt);
            assert_eq!(e0302_count(&diags), 0, "{opt:?}: {diags:?}");
        }
    }
}
