//! HIR → Java emitter.
//!
//! Split across `emit/` submodules; see `mod.rs` for orchestration.
//!
//! Reads the HIR and produces the textual contents of one `AI_<id>.java`
//! file plus a `.lines` sidecar. Routes binary ops through the inline
//! Java operator when both sides are statically numeric, falling back
//! to `LeekOperations.add(...)`-style helpers otherwise — same shape
//! as the Java reference's `LeekExpression.writeJavaCode`.
//!
//! Scope: the core subset of v3/v4 features that the corpus exercises
//! widely. The Java reference's more exotic paths (narrowing casts on
//! boxed locals, by-ref `@x` argument boxing, `rfunction_` reassignable
//! function fixups) emit a structurally-valid approximation; tightening
//! to byte-parity is gated on the golden-output harness landing.

use leek_charge::{ChargeOpts, add_charges};
use leek_hir::{
    BinaryOp, Callee, Def, Expr, ExprKind, HirFile, Literal, NameRef, PostfixOp, Stmt, UnaryOp,
};
use leek_span::Span;
use leek_types::Type;
use std::fmt::Write as _;

use crate::mangle;
use crate::options::Options;
use crate::writer::JavaWriter;

/// Output of a single emission run.
pub struct EmittedJava {
    pub class_name: String,
    pub java: String,
    pub lines: String,
}

pub fn emit(hir: &HirFile, opts: &Options) -> EmittedJava {
    // Clean mode with `with_charge` rewrites the HIR up-front to
    // prepend a static `Charge(n)` to each block. Exact mode keeps
    // per-statement `ai.ops(1)` ticks (see `opts.emit_ops`).
    let owned_hir;
    let hir_ref = if opts.is_clean() && opts.with_charge {
        owned_hir = add_charges(hir, ChargeOpts::default());
        &owned_hir
    } else {
        hir
    };

    let mut em = Emitter::new(opts, hir_ref);
    em.emit_file();
    let class_name = opts.class_name();
    let (java, lines) = em.writer.into_parts();
    EmittedJava {
        class_name,
        java,
        lines,
    }
}

pub(crate) struct Emitter<'a> {
    opts: &'a Options,
    hir: &'a HirFile,
    writer: JavaWriter,
    /// True while emitting the body of a user function or method.
    /// The reference instruments returns inside user functions with
    /// a pair of standalone `ops(...)` calls (entry tick + expr cost)
    /// whereas main-block returns are bare.
    in_function: bool,
    /// Monotonic counter for foreach iterator temp names.
    iter_counter: u32,
    /// Lambda nesting depth. When > 0, the bare Java `this` would
    /// resolve to the anonymous `FunctionLeekValue` inner class
    /// instead of the surrounding AI — so we emit `ai` (the lambda's
    /// own AI parameter) instead at any "pass the AI" site.
    /// `Cell` because `write_expr` and friends take `&self`.
    lambda_depth: std::cell::Cell<u32>,
    /// Outlined-lambda factories queued for emission at end of class.
    /// Each entry is one full `private FunctionLeekValue
    /// __anon_<n>(…) { … }` declaration. Block-bodied lambdas that
    /// close over outer locals get routed through these so the
    /// captured values become final parameters (Java's inner-class
    /// rules accept those even when the original locals are
    /// reassignable).
    outlined: std::cell::RefCell<Vec<String>>,
    /// Hoisted `FunctionLeekValue` singletons for first-class function/builtin
    /// references (`var f = test`, `test == test`). One field per distinct
    /// function so repeated references are the SAME instance — `equals_equals`
    /// then compares two refs to one object (`test == test` → true). Keyed by
    /// the field name; value is the full `private FunctionLeekValue <name> = …;`
    /// declaration, emitted at class-body end like `outlined`.
    fn_singletons: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
    /// True while rendering an *outlined* lambda body (one hoisted to an
    /// AI-level `__anon_<n>` method). There, `<u_Class>.this` is NOT in scope
    /// (the method isn't lexically inside the class), so a class-instance `this`
    /// falls back to bare `this`. Inline lambdas (anonymous classes inside a
    /// class method) keep `<u_Class>.this`.
    in_outlined: std::cell::Cell<bool>,
    /// `@`-by-ref parameter locals (at v1) currently in scope that are bound to
    /// a runtime `Box`. Reads emit `<name>.get()`, writes route through the
    /// `Box`'s `set`/`add_eq`/`increment`/… so a mutation propagates back to the
    /// caller's variable / array element (the v1 runtime passes element boxes to
    /// `@` callbacks). At v2+ `@` params are plain (no propagation), so this set
    /// stays empty.
    ref_boxes: std::cell::RefCell<std::collections::HashSet<leek_hir::DefId>>,
    /// Monotonic counter for `__anon_<n>` outlined-lambda names.
    outline_counter: std::cell::Cell<u32>,
    /// DefId currently being initialized by a `var X = …`. When the
    /// init expression is a recursive lambda referencing `X`, the
    /// lambda emitter excludes `X` from the capture set (passing it
    /// in would read uninitialized memory and Java rejects with
    /// "might not have been initialized").
    initializing_def: std::cell::Cell<Option<leek_hir::DefId>>,
    /// When emitting a self-recursive lambda body (`var fact =
    /// function(x) { fact(x-1) ... }`), this is set to the
    /// var's `DefId`. Any `NameRef::Local(def)` matching it
    /// emits `_self_box[0]` (the array-box holding the
    /// in-construction lambda) instead of the user-facing name.
    self_rec_def: std::cell::Cell<Option<leek_hir::DefId>>,
    /// Builtin names the source reassigns somewhere (`push = 1`,
    /// `cos = function(...) {...}`). At v1 upstream allows this
    /// and subsequent reads/calls of the name see the user's
    /// value instead of the builtin. We route those through a
    /// `__shadows` HashMap field on the AI class — see
    /// `emit_file`. Populated by `collect_shadowed_builtins`
    /// before the file's statements emit.
    shadowed_builtins: std::cell::RefCell<std::collections::HashSet<String>>,
    /// Function-local variables that must be heap-boxed (`Object[]`) because a
    /// directly-nested lambda captures *and writes* them. LeekScript closures
    /// capture by reference, so the write must be visible in the enclosing
    /// scope — Java's effectively-final rule forbids that for a plain captured
    /// local, so the variable is shared through a 1-element array and every
    /// read/write goes via `[0]` (the same trick as `_self_box`). Populated
    /// once by `collect_boxed_locals` in `emit_file`. `DefId`s are unique
    /// file-wide, so a single set serves every function.
    boxed_locals: std::cell::RefCell<std::collections::HashSet<leek_hir::DefId>>,
    /// The user class whose method body is currently being emitted, if any.
    /// Lets `this.field` / `this.method(...)` resolve to direct Java field /
    /// method access (the generated class has real Java fields + `u_<m>`
    /// methods) instead of the reflective `getField` / `callObjectAccess`
    /// fallback used for `Object`-typed bases. `None` outside a class method.
    current_class: std::cell::Cell<Option<&'a leek_hir::Class>>,
}

mod call;
mod class;
mod expr;
mod lambda;
mod literals;
mod stmt;
mod switch;
mod traits;

pub(crate) use traits::EmitExpr;

impl<'a> Emitter<'a> {
    pub(crate) fn new(opts: &'a Options, hir: &'a HirFile) -> Self {
        Self {
            opts,
            hir,
            writer: JavaWriter::new(),
            in_function: false,
            iter_counter: 0,
            lambda_depth: std::cell::Cell::new(0),
            outlined: std::cell::RefCell::new(Vec::new()),
            fn_singletons: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            in_outlined: std::cell::Cell::new(false),
            ref_boxes: std::cell::RefCell::new(std::collections::HashSet::new()),
            outline_counter: std::cell::Cell::new(0),
            initializing_def: std::cell::Cell::new(None),
            self_rec_def: std::cell::Cell::new(None),
            shadowed_builtins: std::cell::RefCell::new(std::collections::HashSet::new()),
            boxed_locals: std::cell::RefCell::new(std::collections::HashSet::new()),
            current_class: std::cell::Cell::new(None),
        }
    }

    /// Identifier to pass for the AI reference at this emit point. Inside a
    /// lambda → `ai` (the lambda's own AI parameter); inside a class method →
    /// `<AIClass>.this` (bare `this` there is the *instance*, not the AI); at
    /// the AI top level → `this`. Anywhere we hand off the AI should call this.
    pub(crate) fn ai_this(&self) -> String {
        if self.lambda_depth.get() > 0 {
            "ai".to_string()
        } else if self.current_class.get().is_some() {
            format!("{}.this", self.opts.class_name())
        } else {
            "this".to_string()
        }
    }

    pub(crate) fn def_name(&self, id: leek_hir::DefId) -> &str {
        self.hir
            .defs
            .get(id.0 as usize)
            .map_or("__unresolved", leek_hir::Def::name)
    }

    /// Declared arity of a user function. Used by [`user_fn_wrapper`]
    /// when emitting a first-class reference to the function. Returns
    /// 0 for non-function defs — wrapper emits a zero-arg invocation
    /// in that case, which still compiles.
    pub(crate) fn user_fn_arity(&self, id: leek_hir::DefId) -> usize {
        self.hir
            .defs
            .get(id.0 as usize)
            .and_then(|d| match d {
                Def::Function(f) => Some(f.params.len()),
                _ => None,
            })
            .unwrap_or(0)
    }

    /// Map a source span to a 1-based Leek line for the `.lines` sidecar.
    pub(crate) fn line_of(&self, span: Span) -> u32 {
        if let Some(table) = &self.opts.line_table {
            table.line_col(span.start).line
        } else {
            self.writer.current_line()
        }
    }

    pub(crate) fn emit_file(&mut self) {
        let hir = self.hir;
        // Pre-scan for builtin reassignments (`push = 1`,
        // `cos = function() {...}`). At v1 upstream allows
        // reassigning a builtin name; the new value shadows the
        // builtin for subsequent reads / calls. We route those
        // through an instance HashMap on the AI class.
        collect_shadowed_builtins(hir, &mut self.shadowed_builtins.borrow_mut());
        // Pre-scan for locals a directly-nested lambda captures *and writes*;
        // those become shared `Object[]` boxes (see `boxed_locals`).
        lambda::collect_boxed_locals(hir, &mut self.boxed_locals.borrow_mut());

        self.writer.add_line("import leekscript.runner.*;");
        self.writer.add_line("import leekscript.runner.values.*;");
        self.writer.add_line("import leekscript.runner.classes.*;");
        self.writer.add_line("import leekscript.common.*;");
        // Host-environment dispatch classes (e.g. the fight functions'
        // `com.leekwars.generator.classes.*`) so `EntityClass.getCell(…)`
        // resolves.
        if let Some(env) = &self.opts.environment {
            for ns in env.imports() {
                self.writer.add_line(&format!("import {ns};"));
            }
        }
        if !self.shadowed_builtins.borrow().is_empty() {
            self.writer.add_line("import java.util.HashMap;");
        }
        self.writer.newline();

        let class = self.opts.class_name();
        self.writer.add_line(&format!(
            "public class {class} extends {} {{",
            self.opts.base_class
        ));
        // Exact mode flush-lefts everything inside the class to match
        // the reference. Clean mode keeps the indent for readability.
        if self.opts.is_clean() {
            self.writer.push_indent();
        }

        // Inner classes first — Java requires forward references via
        // simple name, so emitting up-front is cleanest.
        for def in &hir.defs {
            if let Def::Class(c) = def {
                self.emit_class(c);
            }
        }
        // AI-level class members: the `ClassLeekValue` handle field and the
        // `new_<class>(args)` construction helper, one per user class.
        for def in &hir.defs {
            if let Def::Class(c) = def {
                self.emit_class_ai_member(c);
            }
        }
        // AI-level static members: static-method bodies + the
        // `createStaticClass_<C>` / `initClass_<C>` hooks run by `staticInit`.
        for def in &hir.defs {
            if let Def::Class(c) = def {
                self.emit_class_static_members(c);
            }
        }
        // `__shadows` field — holds user-assigned values for any
        // builtin name the source reassigns. See
        // `collect_shadowed_builtins`. Empty when the program
        // doesn't reassign any builtin.
        if !self.shadowed_builtins.borrow().is_empty() {
            self.writer
                .add_line("private final HashMap<String, Object> __shadows = new HashMap<>();");
        }

        // Globals as fields (declared before constructor so init in
        // ctor body can reference them by name).
        for def in &hir.defs {
            if let Def::Global(g) = def {
                let name = mangle::global(self.opts, &g.name);
                let ty = java_type_for(g.ty.as_ref());
                self.writer.add_line(&format!("private {ty} {name};"));
                self.writer
                    .add_line(&format!("private boolean g_init_{} = false;", g.name));
            }
        }

        // Constructor.
        // The reference's `super(N, V)` carries the number of *instructions*
        // in the main block, where the parser registers `if/else` as a
        // pair of sibling instructions. We mirror that here so the
        // `super(...)` argument matches the reference exactly.
        let stmt_count = main_stmt_count(&hir.main);
        let version = self.opts.version_byte();
        self.writer
            .add_line(&format!("public {class}() throws LeekRunException {{"));
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        self.writer
            .add_line(&format!("super({stmt_count}, {version});"));
        // Register each user class's methods on its `ClassLeekValue` so dynamic
        // dispatch (`callObjectAccess`) and construction (`execute`) work.
        for def in &hir.defs {
            if let Def::Class(c) = def {
                self.emit_class_registration(c);
            }
        }
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        self.writer.add_line("}");

        // staticInit hook: declare then initialize each class's static fields
        // (the `createStaticClass_<C>` / `initClass_<C>` hooks). Two passes so a
        // static initializer can reference another class's statics.
        self.writer
            .add_line("public void staticInit() throws LeekRunException {");
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        for def in &hir.defs {
            if let Def::Class(c) = def {
                self.writer
                    .add_line(&format!("createStaticClass_{}();", c.name));
            }
        }
        for def in &hir.defs {
            if let Def::Class(c) = def {
                self.writer.add_line(&format!("initClass_{}();", c.name));
            }
        }
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        self.writer.add_line("}");

        // User functions. A bodiless function is an external signature
        // (e.g. a signature file's `function add(...) -> T;` with a
        // `@java-backend:` directive): its calls are emitted inline from
        // the directive, so it contributes no method here. Emitting a
        // stub would produce invalid Java. Normal code never has
        // bodiless functions (the parser requires a body).
        for def in &hir.defs {
            if let Def::Function(f) = def {
                if f.body.is_none() {
                    continue;
                }
                self.emit_function(f);
            }
        }

        // runIA — main block lives here in the reference layout.
        self.writer
            .add_line("public Object runIA(Session session) throws LeekRunException {");
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        // Reference behavior (AbstractLeekBlock.writeJavaCode): when
        // the last main-block statement is a bare expression, emit
        // `return <expr>;` instead of `<expr>;` — otherwise the
        // emitted Java has a trailing dangling-expression statement.
        let main = hir.main.as_slice();
        let (head, trailing_expr) = match main.last() {
            Some(Stmt::Expr(e)) => (&main[..main.len() - 1], Some(e.clone())),
            _ => (main, None),
        };
        self.emit_stmts(head);
        if let Some(e) = trailing_expr {
            let code = self.expr_to_string(&e);
            // Wrap in `ops(...)` only if the expression has runtime
            // cost — same rule the Java reference applies for
            // `LeekExpressionInstruction` returns.
            let rendered = if self.opts.emit_ops {
                let cost = expr_op_cost(&e);
                if cost > 0 {
                    format!("ops({code}, {cost})")
                } else {
                    code
                }
            } else {
                code
            };
            self.writer.add_line(&format!("return {rendered};"));
        } else if !ends_with_return(head) {
            self.writer.add_line("return null;");
        }
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        self.writer.add_line("}");

        // Trailing AI-path boilerplate — the reference emits these so
        // runtime stack traces can resolve back to the source file
        // and ID. Only in exact mode; clean mode doesn't bother.
        if !self.opts.is_clean() {
            let path = if self.opts.source_path.is_empty() {
                String::new()
            } else {
                escape_string(&self.opts.source_path, true)
            };
            self.writer.add_line(&format!(
                "protected String getAIString() {{ return \"{path}\";}}"
            ));
            self.writer.add_line(&format!(
                "protected String[] getErrorFiles() {{ return new String[] {{\"{path}\", }};}}"
            ));
            self.writer.newline();
            self.writer.add_line(&format!(
                "protected int[] getErrorFilesID() {{ return new int[] {{{}, }};}}",
                self.opts.ai_id
            ));
            self.writer.newline();
        }

        // Outlined-lambda factory methods (`__anon_<n>(...)`) — one
        // per block-bodied lambda that captures outer locals. Java's
        // inner-class rules forbid reading reassignable outer locals
        // directly, but accept passing them into a method as final
        // parameters; the FunctionLeekValue we build inside the
        // factory closes over those final params instead.
        let outlined: Vec<String> = std::mem::take(&mut *self.outlined.borrow_mut());
        for helper in outlined {
            self.writer.add_line(&helper);
        }
        // Hoisted first-class function/builtin singletons (`ufunction_<name>`).
        let singletons = std::mem::take(&mut *self.fn_singletons.borrow_mut());
        for decl in singletons.into_values() {
            self.writer.add_line(&decl);
        }

        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        self.writer.add_line("}");
    }
}

// ---- free-standing helpers --------------------------------------------------

/// Strip non-identifier characters for inclusion in a Java
/// identifier. Mirrors `mangle::safe_chars` but kept free-standing
/// because the function-prologue rebind needs the raw stem.
/// Instruction count for `super(n, version)` — mirrors upstream
/// `MainLeekBlock.mInstructions.size()`: top-level main-block
/// instructions only. Nested loop/switch bodies are separate blocks
/// and do not contribute. `if`/`else` registers as two sibling
/// instructions at the level being counted.
pub(crate) fn main_stmt_count(stmts: &[Stmt]) -> u32 {
    stmts
        .iter()
        .map(|s| match s {
            Stmt::If(i) if i.else_branch.is_some() => 2,
            Stmt::Block(b) => main_stmt_count(&b.stmts),
            _ => 1,
        })
        .sum()
}

pub(crate) fn sanitize_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            for unit in c.to_string().encode_utf16() {
                write!(out, "_u{unit:04X}").unwrap();
            }
        }
    }
    out
}

/// Static op cost of an expression for the `ops(value, n)` overload.
/// Mirrors the Java reference's `LeekExpression.computeOperations` and
/// the cost table in `LeekValueType.java`: literals / var reads / casts
/// / calls / `&&` / `||` are free; most binary operators cost 1;
/// `*` costs `MUL_COST=2`, `/`, `%`, `\` cost `DIV_COST=MOD_COST=5`,
/// `**` costs `POW_COST=40`.
pub(crate) fn expr_op_cost(e: &Expr) -> u32 {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) => 0,
        ExprKind::Binary(op, l, r) => {
            let sub = expr_op_cost(l) + expr_op_cost(r);
            sub + binary_op_cost(*op)
        }
        ExprKind::Unary(op, x) => unary_op_cost(*op) + expr_op_cost(x),
        ExprKind::Postfix(_, x) => 1 + expr_op_cost(x),
        // User-function calls account for their own work via the
        // per-statement ticks inside the callee body. Builtin calls
        // are different — the upstream `LeekFunctions.method(name,
        // _, cost, _)` table assigns a static per-call cost we have
        // to charge at the call site. Argument-evaluation costs
        // still bubble up either way.
        ExprKind::Call(c) => {
            let mut total: u32 = c.args.iter().map(expr_op_cost).sum();
            if let Callee::Function(NameRef::Builtin(name)) = &c.callee {
                total += builtin_call_cost(name);
            }
            total
        }
        // Field access on a builtin class reference (`Real.MIN_VALUE`,
        // `Integer.MAX_VALUE`) has 0 static cost — upstream's
        // emit folds these to a `getField(...)` call without an
        // ops wrapper. Instance/object field access keeps the
        // standard `1 + base.ops` formula from
        // `LeekObjectAccess.analyze`.
        ExprKind::Field(b, _) => {
            if matches!(&b.kind, ExprKind::Name(NameRef::Builtin(_))) {
                0
            } else {
                1 + expr_op_cost(b)
            }
        }
        // Index access (`arr[i]`) has no static cost — only the
        // child costs bubble up. Runtime adds 1 op
        // (`ArrayLeekValue.READ_OPERATIONS`) on the read itself.
        // Mirrors upstream's `LeekArrayAccess.analyze`.
        ExprKind::Index(b, i) => expr_op_cost(b) + expr_op_cost(i),
        ExprKind::Slice(s) => {
            1 + expr_op_cost(&s.base)
                + s.start.as_deref().map_or(0, expr_op_cost)
                + s.end.as_deref().map_or(0, expr_op_cost)
                + s.step.as_deref().map_or(0, expr_op_cost)
        }
        ExprKind::Array(items) | ExprKind::Set(items) => {
            // The reference charges 2 per element (allocation + put).
            items.iter().map(|e| 2 + expr_op_cost(e)).sum::<u32>()
        }
        ExprKind::Map(pairs) => pairs
            .iter()
            .map(|(k, v)| 2 + expr_op_cost(k) + expr_op_cost(v))
            .sum::<u32>(),
        ExprKind::Object(fields) => fields.iter().map(|(_, v)| 2 + expr_op_cost(v)).sum::<u32>(),
        ExprKind::Ternary(c, _, _) => {
            // `LeekTernaire.computeOperations`: `1 + cond_cost`.
            // Branches don't contribute — each branch's cost is
            // accounted for via the per-branch `ops(...)` wrapper
            // emitted at the call site (see `write_ternary`).
            1 + expr_op_cost(c)
        }
        ExprKind::Interval(iv) => {
            // Mirrors upstream `LeekInterval.computeOperations`:
            // 2 ops base + each endpoint expression's cost.
            2 + iv.start.as_deref().map_or(0, expr_op_cost)
                + iv.end.as_deref().map_or(0, expr_op_cost)
                + iv.step.as_deref().map_or(0, expr_op_cost)
        }
        // Casts and `new` are free at the expression level; the
        // operator's own work is in the body of the called helper.
        ExprKind::Cast(x, _) => expr_op_cost(x),
        ExprKind::New(n) => n.args.iter().map(expr_op_cost).sum(),
        ExprKind::Lambda(_) => 0,
    }
}

/// Per-builtin static op cost. Extracted from upstream
/// `LeekFunctions.java` — the third positional arg in
/// `method("name", "Class", N, …)` is the runtime op count
/// charged at every call site. Names absent from this table cost
/// 0 (matches `method("…", "…", …)` overloads that omit the
/// numeric arg in upstream).
pub(crate) fn builtin_call_cost(name: &str) -> u32 {
    leek_builtins::op_cost_emit(name)
}

/// Per-operator op cost, mirroring `LeekValueType.*_COST`. The
/// default for "normal" binary ops is 1; only mul/div/mod/pow and
/// the short-circuit logicals deviate.
pub(crate) fn binary_op_cost(op: BinaryOp) -> u32 {
    use BinaryOp::{
        AddAssign, And, Assign, BitAndAssign, BitOrAssign, BitXorAssign, Div, DivAssign, IntDiv,
        IntDivAssign, Mod, ModAssign, Mul, MulAssign, NullCoalesceAssign, Or, Pow, PowAssign,
        ShiftLAssign, ShiftRAssign, SubAssign, UShiftRAssign,
    };
    match op {
        Mul | MulAssign => 2,
        Div | DivAssign | IntDiv | IntDivAssign | Mod | ModAssign => 5,
        Pow | PowAssign => 40,
        // Short-circuit logicals: upstream's emit wraps the LHS in
        // `ops(lhs, lhs.ops+1)` — the +1 is the AND/OR's own cost
        // (short-circuited away when the RHS doesn't run). Our flat
        // wrapper folds that into the static cost.
        And | Or => 1,
        // Assignments themselves are zero-cost on top of the rhs.
        // Plain `=` counts as 1 op (per `LeekExpression.computeOperations`
        // — assignment falls into the default else-branch that adds 1).
        // Compound assigns inherit the underlying op's cost via the
        // matches above; pure `=` lands here.
        Assign | AddAssign | SubAssign | BitAndAssign | BitOrAssign | BitXorAssign
        | ShiftLAssign | ShiftRAssign | UShiftRAssign | NullCoalesceAssign => 1,
        _ => 1,
    }
}

pub(crate) fn unary_op_cost(op: UnaryOp) -> u32 {
    use UnaryOp::{Pos, Ref};
    match op {
        // `@x` is a pass-through; `+x` is a no-op for numbers.
        Ref | Pos => 0,
        _ => 1,
    }
}

/// True if the expression can appear as a Java statement on its own
/// (without a `nothing(...)` wrap). Java limits statement expressions
/// to method/constructor invocations, assignments, and `++`/`--` —
/// every other shape needs to be wrapped or it's a compile error.
pub(crate) fn is_valid_statement_expr(e: &Expr) -> bool {
    use ExprKind::{Binary, Call, New, Postfix, Unary};
    match &e.kind {
        Call(_) | New(_) => true,
        Binary(op, _, _) if op.is_assignment() => true,
        Unary(UnaryOp::PreInc | UnaryOp::PreDec, _) => true,
        Postfix(PostfixOp::PostInc | PostfixOp::PostDec, _) => true,
        _ => false,
    }
}

/// True if the expression is a "pure value read" — literal, name
/// reference, field/index read, or a lambda definition. These can
/// stand alone as values but aren't valid Java *statements* when
/// they appear in statement position (e.g. `null;`). The reference
/// drops them entirely in non-trailing positions.
pub(crate) fn is_pure_value_expr(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Literal(_)
            | ExprKind::Name(_)
            | ExprKind::Field(_, _)
            | ExprKind::Index(_, _)
            | ExprKind::Slice(_)
            | ExprKind::Lambda(_)
    )
}

pub(crate) fn is_string_expr(e: &Expr) -> bool {
    matches!(e.ty, Type::String) || matches!(&e.kind, ExprKind::Literal(Literal::String(_)))
}

pub(crate) fn is_primitive_number(ty: &Type) -> bool {
    matches!(ty, Type::Integer | Type::Real | Type::Boolean)
}

/// Like [`is_primitive_number`], but also accepts the syntactic
/// shape of a numeric literal even when HIR type info has decayed
/// to `Any`. Recurses through unary `-`/`+` so `-12` (parsed as
/// `Neg(Int(12))`) is treated as a primitive too — otherwise every
/// negative literal would fall back to the boxed Object path.
pub(crate) fn is_primitive_number_expr(e: &Expr) -> bool {
    if is_primitive_number(&e.ty) {
        return true;
    }
    match &e.kind {
        ExprKind::Literal(Literal::Int(_) | Literal::Real(_) | Literal::Bool(_)) => true,
        ExprKind::Unary(UnaryOp::Neg | UnaryOp::Pos, inner) => is_primitive_number_expr(inner),
        _ => false,
    }
}

/// Coerce an emitted value `inner` to a class field's Java type for a write
/// (`v = longint(inner)` for an `integer` field, etc.), mirroring upstream's
/// `compileConvert`. `Object` / unknown fields store the value as-is.
pub(crate) fn coerce_field_write(java_ty: Option<&str>, inner: &str) -> String {
    match java_ty {
        Some("long") => format!("longint({inner})"),
        Some("double") => format!("real({inner})"),
        Some("boolean") => format!("bool({inner})"),
        _ => inner.to_string(),
    }
}

pub(crate) fn java_type_for(ty: Option<&Type>) -> &'static str {
    match ty {
        Some(Type::Integer) => "long",
        Some(Type::Real) => "double",
        Some(Type::Boolean) => "boolean",
        _ => "Object",
    }
}

/// True when this receiver builtin's second positional argument is a
/// `FunctionLeekValue` (a higher-order callback). The cast at the
/// call site lets javac pick the right overload on the value class
/// instead of failing with "Object cannot be converted to
/// FunctionLeekValue".
/// A receiver builtin whose first non-receiver argument is a concrete
/// collection value class (`setUnion(set, OTHER_SET)`, `mapMerge(map, OTHER_MAP)`),
/// so the `Object`-typed argument needs a cast to that class.
pub(crate) fn receiver_collection_arg_cast(name: &str) -> Option<&'static str> {
    match name {
        "setDifference" | "setDisjunction" | "setIntersection" | "setIsSubsetOf" | "setUnion" => {
            Some("SetLeekValue")
        }
        "mapMerge" | "mapPutAll" | "mapReplaceAll" => Some("MapLeekValue"),
        // `intervalContains(value)` resolves to the abstract base's
        // `(AI, Number)` overload — the `Object` arg must be cast to `Number`.
        "intervalContains" => Some("Number"),
        _ => None,
    }
}

pub(crate) fn takes_function_arg(name: &str) -> bool {
    matches!(
        name,
        "arrayMap"
            | "arrayFilter"
            | "arrayFind"
            | "arrayPartition"
            | "arrayIter"
            | "arraySome"
            | "arrayEvery"
            | "arraySort"
            | "arrayFoldLeft"
            | "arrayFoldRight"
            | "mapIter"
            | "mapMap"
            | "mapFilter"
            | "mapEvery"
            | "mapSome"
            | "mapFold"
            | "setFilter"
    )
}

/// True when this builtin's v1–v3 implementation is named
/// `<name>_v1_3` on its receiver class. Mirrors the
/// `version.return_type.isArray() && version <= 3` check in
/// upstream `JavaWriter.writeFunctionCall`. The list is the full
/// set of `_v1_3` methods found under
/// `official-generator/.../leekscript/runner/`:
/// `arrayConcat`, `arrayFilter`, `arrayFlatten`, `arrayMap`,
/// `arrayPartition`, `arraySort`, `intervalToArray`, `setToArray`,
/// `split`, `subArray`.
pub(crate) fn needs_v1_3_suffix(name: &str) -> bool {
    matches!(
        name,
        "arrayConcat"
            | "arrayFilter"
            | "arrayFlatten"
            | "arrayMap"
            | "arrayPartition"
            | "arraySort"
            | "intervalToArray"
            | "setToArray"
            | "split"
            | "subArray"
    )
}

/// Per-builtin arity for first-class references. Matches the first
/// `CallableVersion` in upstream `LeekFunctions.java`. Names absent
/// from this table get arity 1 (the common case for math/string
/// functions). Used only by [`builtin_fn_wrapper`].
pub(crate) fn builtin_arity(name: &str) -> usize {
    match name {
        // Number — arity 2
        "atan2" | "pow" | "hypot" | "randInt" | "randFloat" | "randReal" | "min" | "max"
        | "rotateLeft" | "rotateRight" | "isPermutation" => 2,
        // Number — arity 0
        "rand" => 0,
        // System — arity 0 (these used to default to 1, causing
        // `getOperations()` and friends to short-circuit to null
        // before they could dispatch into SystemClass).
        "getOperations"
        | "getMaxOperations"
        | "getInstructionsCount"
        | "getTimestamp"
        | "getAITimestamp"
        | "getCurrentTime"
        | "getUsedRAM"
        | "getMaxRAM"
        | "getRemainingOperations"
        | "getRamUsage"
        | "getOperationsCount"
        | "getMaxRam"
        | "getSeed" => 0,
        // String — arity 2
        "charAt" | "startsWith" | "endsWith" | "contains" | "indexOf" => 2,
        // String — arity 3
        "replace" => 3,
        // Array — receiver + value
        "push" | "pushAll" | "unshift" | "removeElement" | "arrayRemoveAll" | "join" | "search"
        | "inArray" | "fill" | "removeKey" | "mapContains" | "mapContainsKey" | "mapRemove"
        | "mapRemoveAll" | "mapFill" | "setContains" | "setPut" | "setRemove" => 2,
        // Array — receiver + (callback or array)
        "arrayMap" | "arrayFilter" | "arrayFind" | "arrayPartition" | "arrayIter" | "arraySome"
        | "arrayEvery" | "arrayConcat" | "mapIter" | "mapMap" | "mapSearch" | "mapFilter"
        | "mapEvery" | "mapSome" | "mapMerge" | "setFilter" | "setUnion" | "setIntersection"
        | "setDifference" | "setDisjunction" | "setIsSubsetOf" => 2,
        // Array — receiver + 2 args
        "insert" | "arrayFoldLeft" | "arrayFoldRight" | "mapPut" | "mapReplace" | "mapFold" => 3,
        _ => 1,
    }
}

/// Arity for builtins where we're certain of the upper bound — i.e.
/// the static (non-overloaded) NumberClass / StringClass / ValueClass
/// functions with a single signature in `LeekFunctions.java`. Used
/// by the over-arity short-circuit in `write_call` so we only null
/// out calls we KNOW would fail javac. Returns `None` for receiver
/// builtins (which dispatch dynamically and might handle extras) and
/// for overloaded builtins like `substring`/`indexOf` whose
/// `CallableVersion[]` includes a longer signature.
pub(crate) fn builtin_arity_strict(name: &str) -> Option<usize> {
    Some(match name {
        // NumberClass — one-arg math functions
        "abs" | "ceil" | "floor" | "round" | "signum" | "sqrt" | "cbrt" | "log" | "log2"
        | "log10" | "exp" | "cos" | "sin" | "tan" | "acos" | "asin" | "atan" | "toRadians"
        | "toDegrees" | "bitCount" | "trailingZeros" | "leadingZeros" | "bitReverse"
        | "byteReverse" | "binString" | "hexString" | "realBits" | "bitsToReal" | "isFinite"
        | "isInfinite" | "isNaN" => 1,
        // NumberClass — two-arg
        "atan2" | "pow" | "hypot" | "randInt" | "randReal" | "rotateLeft" | "rotateRight"
        | "isPermutation" => 2,
        // NumberClass — zero-arg
        "rand" => 0,
        // StringClass — one-arg
        "length" | "toUpper" | "toLower" | "trim" => 1,
        // StringClass — two-arg (no overload-with-3 in this list)
        "startsWith" | "endsWith" | "contains" => 2,
        // StringClass — three-arg, single signature
        "replace" => 3,
        // ValueClass — one-arg
        "string" | "typeOf" | "number" | "unknown" => 1,
        _ => return None,
    })
}

/// Build an inline `FunctionLeekValue` wrapper for a first-class
/// reference to a builtin function. Returns `None` for names that
/// aren't in our builtin table — the caller falls back to emitting
/// the bare name (which will fail to compile, but matches the
/// pre-fix behavior).
pub(crate) fn builtin_fn_wrapper(name: &str, version: leek_syntax::Version) -> Option<String> {
    let b = crate::builtins::lookup(name)?;
    let arity = builtin_arity(name);
    let body = match b.dispatch {
        crate::builtins::Dispatch::Static { class } => {
            // `<Class>.<name>(ai, coerce(values[0]), …)` — values
            // arrive boxed inside `Box` when the lambda is invoked
            // via a higher-order builtin (`arrayMap`/`arrayFilter`),
            // so route through `AI.load(...)` first to unwrap.
            let coerce = |i: usize| -> String {
                if class == "NumberClass" {
                    if b.prefer_long {
                        format!("((Number) AI.load(values[{i}])).longValue()")
                    } else {
                        format!("((Number) AI.load(values[{i}])).doubleValue()")
                    }
                } else if class == "StringClass" {
                    format!("(String) AI.load(values[{i}])")
                } else {
                    format!("AI.load(values[{i}])")
                }
            };
            let mut s = format!("return {class}.{name}(ai");
            for i in 0..arity {
                s.push_str(", ");
                s.push_str(&coerce(i));
            }
            s.push_str(");");
            s
        }
        crate::builtins::Dispatch::Receiver {
            v4_class,
            legacy_class,
        } => {
            // `((<class>) values[0]).<name>[_v1_3](ai, values[1..])`
            let class = if matches!(version, leek_syntax::Version::V4) {
                v4_class
            } else {
                legacy_class
            };
            let suffix = if !matches!(version, leek_syntax::Version::V4) && needs_v1_3_suffix(name)
            {
                "_v1_3"
            } else {
                ""
            };
            let mut s = format!("return (({class}) values[0]).{name}{suffix}(ai");
            for i in 1..arity {
                // Higher-order builtins (`arrayMap`/`arrayFilter`/…)
                // declare their callback parameter as
                // `FunctionLeekValue`, not `Object`. Cast at the call
                // site so javac can pick the right overload.
                if i == 1 && takes_function_arg(name) {
                    write!(s, ", (FunctionLeekValue) values[{i}]").unwrap();
                } else {
                    write!(s, ", values[{i}]").unwrap();
                }
            }
            s.push_str(");");
            s
        }
    };
    // Charge the builtin's per-call op cost at wrapper entry —
    // mirrors upstream's `writeAnonymousSystemFunctions` which
    // emits `ops(<cost>);` ahead of the call. Without it the
    // higher-order builtin (`arrayMap([1,2,3], cos)`) under-counts
    // by `cost × n` ops.
    let entry_charge = builtin_call_cost(name);
    let body = if entry_charge > 0 {
        format!("ops({entry_charge}); {body}")
    } else {
        body
    };
    // Guard against being called with too few args — defensively
    // return null instead of NPEing on `values[i]`. Matches the
    // shape of upstream's lambda emission.
    let guarded = if arity > 0 {
        format!("if (values.length < {arity}) return null; {body}")
    } else {
        body
    };
    Some(format!(
        "new FunctionLeekValue({arity}, \"#Function {name}\") {{ \
         public Object run(AI ai, Object thiz, Object... values) \
         throws LeekRunException {{ {guarded} }} }}"
    ))
}

/// Build an inline `FunctionLeekValue` wrapper for a first-class
/// reference to a user function (`function f(x) {...} var arr = [f]`).
/// `mangled` is `f_<name>`; `arity` is the function's declared
/// parameter count.
pub(crate) fn user_fn_wrapper(mangled: &str, arity: usize) -> String {
    let mut body = format!("return {mangled}(");
    for i in 0..arity {
        if i > 0 {
            body.push_str(", ");
        }
        write!(body, "values[{i}]").unwrap();
    }
    body.push_str(");");
    let guarded = if arity > 0 {
        format!("if (values.length < {arity}) return null; {body}")
    } else {
        body
    };
    format!(
        "new FunctionLeekValue({arity}) {{ \
         public Object run(AI ai, Object thiz, Object... values) \
         throws LeekRunException {{ {guarded} }} }}"
    )
}

pub(crate) fn java_class_name(ty: &Type) -> &'static str {
    match ty {
        Type::Integer => "Long",
        Type::Real => "Double",
        Type::Boolean => "Boolean",
        Type::String => "String",
        Type::Array(_) => "ArrayLeekValue",
        Type::Map(_, _) => "MapLeekValue",
        Type::Set(_) => "SetLeekValue",
        _ => "Object",
    }
}

/// Count statements the way the reference parser does: an `if`
/// with an `else` arm is split into two sibling instructions in
/// `mInstructions`, so it counts as 2.
/// Walk the entire HIR (main + every function/method body) and
/// collect builtin names that appear on the left-hand side of an
/// `=` assignment. Those names get routed through `__shadows` at
/// emit time so subsequent reads see the user-assigned value
/// instead of the original builtin function reference.
pub(crate) fn collect_shadowed_builtins(
    hir: &leek_hir::HirFile,
    out: &mut std::collections::HashSet<String>,
) {
    pub(crate) fn scan_expr(e: &Expr, out: &mut std::collections::HashSet<String>) {
        if let ExprKind::Binary(op, lhs, rhs) = &e.kind {
            if matches!(op, BinaryOp::Assign)
                && let ExprKind::Name(NameRef::Builtin(name)) = &lhs.kind
            {
                out.insert(name.clone());
            }
            scan_expr(lhs, out);
            scan_expr(rhs, out);
            return;
        }
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::Name(_) => {}
            ExprKind::Unary(_, x)
            | ExprKind::Postfix(_, x)
            | ExprKind::Cast(x, _)
            | ExprKind::Field(x, _) => scan_expr(x, out),
            ExprKind::Index(b, i) => {
                scan_expr(b, out);
                scan_expr(i, out);
            }
            ExprKind::Call(c) => {
                if let leek_hir::Callee::Method { receiver, .. } = &c.callee {
                    scan_expr(receiver, out);
                }
                if let leek_hir::Callee::Expr(ce) = &c.callee {
                    scan_expr(ce, out);
                }
                for a in &c.args {
                    scan_expr(a, out);
                }
            }
            ExprKind::Array(items) | ExprKind::Set(items) => {
                for e in items {
                    scan_expr(e, out);
                }
            }
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    scan_expr(k, out);
                    scan_expr(v, out);
                }
            }
            ExprKind::Object(pairs) => {
                for (_, v) in pairs {
                    scan_expr(v, out);
                }
            }
            ExprKind::Ternary(c, t, f) => {
                scan_expr(c, out);
                scan_expr(t, out);
                scan_expr(f, out);
            }
            ExprKind::Slice(s) => {
                scan_expr(&s.base, out);
                for e in [&s.start, &s.end, &s.step].into_iter().flatten() {
                    scan_expr(e, out);
                }
            }
            ExprKind::Lambda(lam) => match &lam.body {
                leek_hir::LambdaBody::Block(b) => scan_block(b, out),
                leek_hir::LambdaBody::Expr(e) => scan_expr(e, out),
            },
            ExprKind::New(n) => {
                for a in &n.args {
                    scan_expr(a, out);
                }
            }
            ExprKind::Interval(iv) => {
                for e in [&iv.start, &iv.end, &iv.step].into_iter().flatten() {
                    scan_expr(e, out);
                }
            }
            ExprKind::Binary(_, _, _) => unreachable!("handled above"),
        }
    }
    pub(crate) fn scan_stmt(s: &Stmt, out: &mut std::collections::HashSet<String>) {
        match s {
            Stmt::Expr(e) => scan_expr(e, out),
            Stmt::VarDecl(v) => {
                if let Some(init) = &v.init {
                    scan_expr(init, out);
                }
            }
            Stmt::Return(Some(e)) => scan_expr(e, out),
            Stmt::Return(None) => {}
            Stmt::If(i) => {
                scan_expr(&i.cond, out);
                scan_stmt(&i.then_branch, out);
                if let Some(el) = &i.else_branch {
                    scan_stmt(el, out);
                }
            }
            Stmt::While(w) => {
                scan_expr(&w.cond, out);
                scan_stmt(&w.body, out);
            }
            Stmt::DoWhile(dw) => {
                scan_expr(&dw.cond, out);
                scan_stmt(&dw.body, out);
            }
            Stmt::For(f) => {
                if let Some(init) = &f.init {
                    scan_stmt(init, out);
                }
                if let Some(c) = &f.cond {
                    scan_expr(c, out);
                }
                if let Some(s) = &f.step {
                    scan_expr(s, out);
                }
                scan_stmt(&f.body, out);
            }
            Stmt::Foreach(fe) => {
                scan_expr(&fe.iter, out);
                scan_stmt(&fe.body, out);
            }
            Stmt::Block(b) => scan_block(b, out),
            Stmt::Switch(s) => {
                scan_expr(&s.discriminant, out);
                for arm in &s.arms {
                    for stmt in &arm.body {
                        scan_stmt(stmt, out);
                    }
                }
            }
            _ => {}
        }
    }
    pub(crate) fn scan_block(b: &leek_hir::Block, out: &mut std::collections::HashSet<String>) {
        for s in &b.stmts {
            scan_stmt(s, out);
        }
    }
    for s in &hir.main {
        scan_stmt(s, out);
    }
    for def in &hir.defs {
        if let Def::Function(f) = def
            && let Some(b) = &f.body
        {
            scan_block(b, out);
        }
    }
}
pub(crate) fn is_div_expr(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Binary(BinaryOp::Div, _, _))
}
pub(crate) fn ends_with_return(stmts: &[Stmt]) -> bool {
    stmts.last().is_some_and(stmt_definitely_returns)
}

/// True when control flow can't fall off the end of this statement —
/// i.e. it always exits the enclosing function. Java will reject a
/// trailing `return null;` after such a statement as unreachable.
pub(crate) fn stmt_definitely_returns(s: &Stmt) -> bool {
    match s {
        // Only `return` actually leaves the *function*. `break`/`continue`
        // only escape the enclosing loop, so they don't suppress the
        // trailing `return null;` at function-body end. Treating them
        // as definite-returns produced `do { break } while (true);`-
        // shaped methods that javac flagged as `missing return`.
        Stmt::Return(_) => true,
        Stmt::If(i) => {
            let then_returns = stmt_definitely_returns(&i.then_branch);
            let else_returns = i
                .else_branch
                .as_deref()
                .is_some_and(stmt_definitely_returns);
            then_returns && else_returns
        }
        Stmt::Block(b) => ends_with_return(&b.stmts),
        // A `do { ... } while (cond)` body runs at least once, so a
        // body that always returns means the whole loop always
        // returns. Without this arm `javac` rejects the trailing
        // `return null;` after the loop as unreachable. An infinite
        // `do … while (true)` likewise never falls through.
        Stmt::DoWhile(d) => stmt_definitely_returns(&d.body) || is_infinite_loop(&d.cond, &d.body),
        // `while (true) { … }` with no `break` escaping the loop never
        // completes normally — code after it is unreachable, so suppress
        // the trailing `return null;`.
        Stmt::While(w) => is_infinite_loop(&w.cond, &w.body),
        // A `switch` always returns when it has a `default` arm and every arm
        // (default included) definitely returns and none `break`s out.
        Stmt::Switch(s) => {
            s.arms.iter().any(|a| a.case.is_none())
                && s.arms.iter().all(|a| {
                    ends_with_return(&a.body) && !a.body.iter().any(stmt_has_own_break)
                })
        }
        _ => false,
    }
}

/// True for a `while`/`do-while` whose condition is the literal `true` and whose
/// body has no `break` targeting it — an infinite loop that never falls through.
fn is_infinite_loop(cond: &Expr, body: &Stmt) -> bool {
    matches!(&cond.kind, ExprKind::Literal(Literal::Bool(true))) && !stmt_has_own_break(body)
}

/// Whether `s` contains a `break` that would exit the *current* loop — i.e. one
/// not nested inside another loop or switch (which would capture it). Used to
/// tell whether a `while(true)` is truly infinite.
fn stmt_has_own_break(s: &Stmt) -> bool {
    match s {
        Stmt::Break(_) => true,
        Stmt::Block(b) => b.stmts.iter().any(stmt_has_own_break),
        Stmt::If(i) => {
            stmt_has_own_break(&i.then_branch)
                || i.else_branch.as_deref().is_some_and(stmt_has_own_break)
        }
        // A nested loop / switch captures its own `break`; don't descend.
        _ => false,
    }
}

pub(crate) fn is_terminator(s: &Stmt) -> bool {
    matches!(s, Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_))
}

pub(crate) fn escape_string(s: &str, v2plus: bool) -> String {
    // Shared codec — see `leek_text::escape_java`. Translates Leek
    // literal text into a Java string literal body: v1 keeps the
    // historical `\"` → `\\"` behaviour, v2+ emits the modern escape;
    // non-ASCII becomes `\uXXXX` (surrogate pairs for supplementary
    // chars) so the emitted Java source is pure ASCII.
    let mode = if v2plus {
        leek_text::EscapeMode::V2Plus
    } else {
        leek_text::EscapeMode::V1
    };
    leek_text::escape_java(s, mode)
}
