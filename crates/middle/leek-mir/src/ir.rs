//! MIR data types.
//!
//! Each function becomes a [`MirFunction`]: a list of [`LocalDecl`]s
//! (parameters and synthetic temporaries) plus a list of
//! [`BasicBlock`]s indexed by [`BlockId`]. Every block ends in a
//! [`Terminator`] — there are no fall-throughs between blocks.
//!
//! This is **not full SSA**. Locals can be reassigned; the
//! Cranelift frontend takes care of SSA conversion when the native
//! backend consumes us. We keep the form simple so the bytecode VM
//! can map locals to slot indices directly.
//!
//! All operands are either a local (a `LocalId`) or a literal
//! constant — there are no nested expression trees inside an
//! [`Rvalue`]. The HIR-to-MIR lowering pass introduces temporaries
//! as needed.

use leek_hir::DefId;
use leek_span::Span;
use leek_types::Type;

// ---- Top-level ----

/// A whole HIR file lowered to MIR. Carries the functions (one per
/// user-defined function plus a synthetic `main` for the top-level
/// statements) and a flat list of globals.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MirProgram {
    pub functions: Vec<MirFunction>,
    pub globals: Vec<MirGlobal>,
    pub classes: Vec<MirClass>,
}

impl MirProgram {
    /// Find the function lowered from `def_id`, if any.
    pub fn function(&self, def_id: DefId) -> Option<&MirFunction> {
        self.functions.iter().find(|f| f.def_id == Some(def_id))
    }

    /// The synthetic `main` function lowered from the file's
    /// top-level statements. Always present, even when empty.
    pub fn main(&self) -> Option<&MirFunction> {
        self.functions.iter().find(|f| f.kind == FunctionKind::Main)
    }

    /// Find a class by its HIR `DefId`.
    pub fn class(&self, def_id: DefId) -> Option<&MirClass> {
        self.classes.iter().find(|c| c.def_id == def_id)
    }

    /// Find a class by name. Used by `new ClassName(...)` and
    /// static-member lookup since those carry a name, not a
    /// DefId.
    pub fn class_by_name(&self, name: &str) -> Option<&MirClass> {
        self.classes.iter().find(|c| c.name == name)
    }

    /// Resolve a method call against a class's flattened vtable,
    /// mirroring the interpreter's arity-aware dispatch: when
    /// `argc` is `Some(n)`, prefer the `(name, n)` overload; the
    /// any-arity fallback picks the most-derived class's
    /// first-declared same-name method. Instance methods only.
    pub fn resolve_method<'c>(
        &self,
        class: &'c MirClass,
        name: &str,
        argc: Option<usize>,
    ) -> Option<&'c VtableSlot> {
        if let Some(n) = argc
            && let Some(e) = class
                .vtable
                .iter()
                .find(|e| e.name == name && e.user_arity == n)
        {
            return Some(e);
        }
        class
            .vtable
            .iter()
            .filter(|e| e.name == name)
            .max_by_key(|e| (self.class_depth(e.owner), std::cmp::Reverse(e.slot)))
    }

    /// Select the constructor `class` (or its nearest ancestor that
    /// declares any) uses for `argc` arguments. Matches the
    /// interpreter: walk child-first to the first class with
    /// constructors, then prefer an exact-arity match, else its
    /// first constructor. Returns its `function_idx`.
    pub fn select_constructor(&self, class: &MirClass, argc: usize) -> Option<usize> {
        let mut cursor = Some(class);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(c) = cursor {
            if !seen.insert(c.def_id) {
                break;
            }
            if !c.constructors.is_empty() {
                if let Some(ctor) = c.constructors.iter().find(|k| k.user_arity == argc) {
                    return Some(ctor.function_idx);
                }
                return Some(c.constructors[0].function_idx);
            }
            cursor = c.parent_def.and_then(|p| self.class(p));
        }
        None
    }

    /// Distance from the root of the inheritance chain (root = 0).
    /// Deeper = more derived. Cycle-safe.
    fn class_depth(&self, def_id: DefId) -> usize {
        let mut depth = 0;
        let mut cursor = self.class(def_id).and_then(|c| c.parent_def);
        let mut seen: std::collections::HashSet<DefId> = std::collections::HashSet::new();
        while let Some(p) = cursor {
            if !seen.insert(p) {
                break;
            }
            depth += 1;
            cursor = self.class(p).and_then(|c| c.parent_def);
        }
        depth
    }

    /// Resolve `parent` names to `DefId`s and build each class's
    /// inheritance-flattened `field_layout` + `vtable`. Run once
    /// after all classes are lowered. Idempotent.
    pub fn compute_class_layouts(&mut self) {
        let n = self.classes.len();
        let name_to_idx: std::collections::HashMap<String, usize> = self
            .classes
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        let parent_idx: Vec<Option<usize>> = self
            .classes
            .iter()
            .map(|c| c.parent.as_ref().and_then(|p| name_to_idx.get(p).copied()))
            .collect();
        let parent_def: Vec<Option<DefId>> = parent_idx
            .iter()
            .map(|pi| pi.map(|i| self.classes[i].def_id))
            .collect();

        let mut layouts: Vec<(Vec<FieldSlot>, Vec<VtableSlot>)> = Vec::with_capacity(n);
        for i in 0..n {
            // Build the ancestor chain leaf→root (cycle-safe), then
            // reverse so ancestors are flattened first.
            let mut chain: Vec<usize> = Vec::new();
            let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
            let mut cur = Some(i);
            while let Some(ci) = cur {
                if !seen.insert(ci) {
                    break;
                }
                chain.push(ci);
                cur = parent_idx[ci];
            }
            chain.reverse();

            let mut fields: Vec<FieldSlot> = Vec::new();
            let mut vtable: Vec<VtableSlot> = Vec::new();
            for &ci in &chain {
                let owner = self.classes[ci].def_id;
                for f in &self.classes[ci].instance_fields {
                    if let Some(existing) = fields.iter_mut().find(|s| s.name == f.name) {
                        existing.ty = f.ty.clone();
                        existing.is_final = f.is_final;
                        existing.init_fn = f.init_fn;
                        existing.owner = owner;
                    } else {
                        let slot = fields.len();
                        fields.push(FieldSlot {
                            name: f.name.clone(),
                            slot,
                            ty: f.ty.clone(),
                            is_final: f.is_final,
                            init_fn: f.init_fn,
                            owner,
                        });
                    }
                }
                for m in &self.classes[ci].methods {
                    if m.is_static {
                        continue;
                    }
                    if let Some(existing) = vtable
                        .iter_mut()
                        .find(|e| e.name == m.name && e.user_arity == m.user_arity)
                    {
                        existing.function_idx = m.function_idx;
                        existing.visibility = m.visibility;
                        existing.owner = owner;
                    } else {
                        let slot = vtable.len();
                        vtable.push(VtableSlot {
                            name: m.name.clone(),
                            slot,
                            function_idx: m.function_idx,
                            user_arity: m.user_arity,
                            visibility: m.visibility,
                            owner,
                        });
                    }
                }
            }
            layouts.push((fields, vtable));
        }

        for (i, (fields, vtable)) in layouts.into_iter().enumerate() {
            self.classes[i].parent_def = parent_def[i];
            self.classes[i].field_layout = fields;
            self.classes[i].vtable = vtable;
        }
    }
}

// ---- Classes ----

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct MirClass {
    pub def_id: DefId,
    pub name: String,
    /// Parent class name, if any. Lookup happens at runtime
    /// through [`MirProgram::class_by_name`].
    pub parent: Option<String>,
    /// Parent class resolved to its `DefId`. Filled by
    /// [`MirProgram::compute_class_layouts`] after all classes are
    /// lowered, so backends can walk the inheritance chain without
    /// re-resolving names. `None` for roots / unknown parents.
    pub parent_def: Option<DefId>,
    /// Own (directly declared) instance fields, in source order.
    pub instance_fields: Vec<MirField>,
    pub static_fields: Vec<MirField>,
    /// Own (directly declared) methods, in source order.
    pub methods: Vec<MirMethod>,
    pub constructors: Vec<MirMethod>,
    /// Inheritance-flattened instance-field layout (ancestors'
    /// fields first, then own). Each field gets a stable `slot`
    /// index; a child re-declaration reuses the parent's slot and
    /// records the most-derived declaration. Computed once by
    /// [`MirProgram::compute_class_layouts`].
    pub field_layout: Vec<FieldSlot>,
    /// Inheritance-flattened instance-method table. `(name,
    /// user_arity)` is unique — a child method with the same name
    /// AND arity overrides the parent's entry (keeping its slot);
    /// a same-name/different-arity method is a separate overload
    /// slot. Computed once by [`MirProgram::compute_class_layouts`].
    pub vtable: Vec<VtableSlot>,
    pub span: Span,
}

impl MirClass {
    /// Resolve an instance-field name to its flattened-layout slot.
    pub fn field_slot(&self, name: &str) -> Option<&FieldSlot> {
        self.field_layout.iter().find(|f| f.name == name)
    }
}

/// One instance-field slot in a class's flattened layout. Lets a
/// backend allocate an instance (slot count) and resolve a field
/// name → slot in O(1) without re-walking the class chain.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSlot {
    pub name: String,
    pub slot: usize,
    pub ty: Type,
    pub is_final: bool,
    /// Initializer of the most-derived declaration. For instance
    /// fields the function takes `this` as its first param.
    pub init_fn: Option<usize>,
    /// Class that declares the effective (most-derived) field.
    pub owner: DefId,
}

/// One instance-method entry in a class's flattened vtable.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct VtableSlot {
    pub name: String,
    pub slot: usize,
    pub function_idx: usize,
    pub user_arity: usize,
    pub visibility: Visibility,
    /// Class that declares the effective (most-derived) method.
    pub owner: DefId,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct MirField {
    pub name: String,
    /// `None` if the field has no initializer (defaults to null).
    /// For instance fields, the function takes `this` as its
    /// first param. For static fields, the function is nullary.
    pub init_fn: Option<usize>,
    pub visibility: Visibility,
    /// When `true`, writes through the field name silently no-op
    /// after the initial value is set. Matches upstream's `final`
    /// modifier — assignment doesn't error, it just doesn't take.
    pub is_final: bool,
    /// Declared field type (`real` in `class A { real x }`), or
    /// `Type::Any` if unannotated. Used by the interp to coerce
    /// the init value / writes to the declared type.
    pub ty: Type,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct MirMethod {
    pub name: String,
    pub function_idx: usize,
    pub is_static: bool,
    /// Number of user-visible parameters (excludes the synthetic
    /// `this` slot prepended for instance methods).
    pub user_arity: usize,
    pub visibility: Visibility,
    pub span: Span,
}

/// A global variable. Its initializer (if any) is lowered into the
/// main function's prologue rather than carried inline, so the
/// declaration here is just signature.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct MirGlobal {
    pub def_id: DefId,
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

// ---- Functions ----

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct MirFunction {
    /// `None` for the synthetic `main` function lowered from the
    /// file's top-level statements.
    pub def_id: Option<DefId>,
    pub kind: FunctionKind,
    pub name: String,
    pub params: Vec<LocalId>,
    pub return_ty: Type,
    pub locals: Vec<LocalDecl>,
    pub blocks: Vec<BasicBlock>,
    pub entry: BlockId,
    /// When this function is a method, constructor, or field
    /// initializer of a class, the class's DefId. The interpreter
    /// uses it to enforce private/protected visibility from the
    /// caller side.
    pub owning_class: Option<DefId>,
    pub span: Span,
}

impl MirFunction {
    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    pub fn local(&self, id: LocalId) -> &LocalDecl {
        &self.locals[id.0 as usize]
    }
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionKind {
    /// User-defined function (`function foo() { … }`).
    User,
    /// Synthetic function lowered from the file's top-level
    /// statements. There is exactly one per program.
    Main,
}

leek_span::newtype_index! {
    /// Index into [`MirFunction::locals`].
    pub struct LocalId;
    display = "_{}";
}

leek_span::newtype_index! {
    /// Index into [`MirFunction::blocks`].
    pub struct BlockId;
    display = "bb{}";
}

/// One local slot. Parameters live in the first `params.len()`
/// slots; synthetic temporaries (`_t0`, `_t1`, …) come after.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct LocalDecl {
    /// User-visible name, if any. Synthetic temporaries leave this
    /// `None`. Kept for debug output and for the Java backend's
    /// future "explain MIR" mode.
    pub name: Option<String>,
    pub ty: Type,
    pub kind: LocalKind,
    pub span: Span,
    /// For parameters with a default value, the block that
    /// computes the default. The block ends in a
    /// [`Terminator::Return`] yielding the default value, which
    /// the interpreter assigns to this local when the caller
    /// omits the corresponding argument.
    pub default_init: Option<BlockId>,
    /// Type inferred from a `var x = init` initializer (when
    /// `ty` was not explicitly written by the user). Drives
    /// compound-assign coercion only — plain `=` doesn't widen
    /// based on this. Distinct from `ty` so non-strict tests
    /// that expect `var a = 5.5; a = 2` to land as Int still pass.
    pub inferred_ty: Option<Type>,
    /// True when this local is referenced by a nested lambda's
    /// capture set. The interpreter wraps shared locals in a
    /// `Value::Cell` so writes from the closure propagate back to
    /// the outer scope (and vice versa).
    pub is_shared: bool,
    /// True when this local is a `@x` reference parameter — the
    /// caller's slot is wrapped in a `Value::Cell` at the call
    /// site and the cell is passed in, so writes propagate up to
    /// the caller. Implies `is_shared`; tracked separately because
    /// captured-but-not-@ locals share storage with closures but
    /// NOT with the caller.
    pub is_by_ref: bool,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    Param,
    UserLocal,
    /// Synthetic temporary introduced by lowering.
    Temp,
}

// ---- Basic blocks ----

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct BasicBlock {
    pub id: BlockId,
    pub statements: Vec<Statement>,
    /// Source span of each statement, parallel to `statements` (same
    /// length). Populated by the lowering's `push_stmt`; used by the
    /// native backend's debug build to map machine code back to source
    /// lines for breakpoints. Blocks built outside the lowering (test
    /// helpers, synthetic thunks) may leave this empty — readers must
    /// tolerate a missing entry.
    pub statement_spans: Vec<Span>,
    pub terminator: Terminator,
    /// Source span of the terminator (e.g. the `return` line). Lets the
    /// native debug backend place a safepoint on lines that lower to only a
    /// terminator. [`Span::synthetic`] when the block came from outside the
    /// lowering or the terminator has no source location.
    pub terminator_span: Span,
}

/// Side-effect-free or simple-effect statement. Anything that needs
/// to change control flow is a [`Terminator`] instead.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// `place = rvalue;`
    Assign(Place, Rvalue),
    /// `place = call(args);` — calls live as their own statement so
    /// backends can see the effect even when the result is unused.
    /// `place` is `None` for a discarded result (e.g. `foo();`).
    Call { dest: Option<Place>, call: CallExpr },
    /// Op-budget tick lowered from [`leek_hir::Stmt::Charge`]. A
    /// backend that doesn't enforce a budget can skip these.
    Charge(u64),
    /// Drain the interpreter's "pending v1-v3 LegacyArray
    /// promotion" side-channel into the given local. Emitted
    /// after builtins that may morph their first arg from an
    /// Array to a sparse Map (e.g. `removeElement`,
    /// `assocReverse`, `assocSort`). A no-op when no promotion
    /// was set.
    ApplyPromotion(LocalId),
}

// ---- Terminators ----

/// How a block ends. Exactly one terminator per block.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Terminator {
    /// Unconditional jump.
    Goto(BlockId),
    /// `if cond { then } else { else }`. `cond` is an operand —
    /// short-circuit lowering of `&&` / `||` / `??` happens before
    /// we reach the terminator.
    Branch {
        cond: Operand,
        then_block: BlockId,
        else_block: BlockId,
    },
    /// `return value`.
    Return(Option<Operand>),
    /// `switch (disc) { case k0 -> bb0; … default -> bbN }`.
    Switch {
        discriminant: Operand,
        arms: Vec<(Const, BlockId)>,
        default: BlockId,
    },
    /// Statically unreachable. We emit this after a `Return` block
    /// is closed so we always have *some* terminator while
    /// lowering, and after explicit `throw`-style intrinsics if we
    /// add them later.
    Unreachable,
}

// ---- Places, operands, rvalues ----

/// An l-value — somewhere a value can be stored.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    Local(LocalId),
    /// File-level global by `DefId`. Carries the source name so
    /// backends can render diagnostics without a back-reference to
    /// the HIR.
    Global(DefId, String),
    /// `base.field`. `base` is a local so that we never have to
    /// re-evaluate a side-effecting expression; lowering puts the
    /// base into a temp first.
    Field(LocalId, String),
    /// `base[index]`. Same temp-first rule applies to `index`.
    Index(LocalId, Operand),
    /// `obj[a:b:c] = …`. `start`/`end`/`step` use `Option<Operand>`
    /// to model the omitted bounds. Sliced writes are rare in
    /// Leekscript so we keep the shape minimal.
    Slice(LocalId, SliceBounds),
    /// Mutate a closure's captured slot. Used by `lower_var_decl`
    /// to patch a self-recursive lambda's capture (the lambda
    /// captures its own binding, which was null at construction
    /// time; this patch lands the lambda's own value back into
    /// the slot after the surrounding `Assign` completes).
    LambdaCapture {
        lambda: LocalId,
        slot: usize,
    },
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct SliceBounds {
    pub start: Option<Operand>,
    pub end: Option<Operand>,
    pub step: Option<Operand>,
}

/// An r-value — a (possibly nontrivial) value-producing operation.
/// Every nested sub-expression is already a flat [`Operand`].
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Rvalue {
    Use(Operand),
    /// Like [`Rvalue::Use`] but the operand is a *freshly produced*
    /// value (e.g. the new array a builtin like `arrayMap` returns),
    /// so the interpreter's v1 pass-by-value clone is skipped on
    /// assignment. Cloning would deep-copy the result and break the
    /// references its elements share (`arrayMap(a, v -> r)` returns an
    /// array whose every element aliases the same `r`). Identical to
    /// `Use` in all other respects and for v2+.
    UseFresh(Operand),
    Binary(BinOp, Operand, Operand),
    Unary(UnOp, Operand),
    Cast(CastKind, Operand),
    Field(LocalId, String),
    Index(LocalId, Operand),
    Slice(LocalId, SliceBounds),
    Array(Vec<Operand>),
    Map(Vec<(Operand, Operand)>),
    Set(Vec<Operand>),
    Object(Vec<(String, Operand)>),
    /// `new ClassName(args)`. Args are flat operands.
    New {
        class: String,
        args: Vec<Operand>,
    },
    /// `[a..b]` interval literal.
    Interval(IntervalRvalue),
    /// Snapshot an iterable into a foreach-iteration state — an
    /// `Array<[key, value]>` the lowering then walks with a normal
    /// index loop. Materialising the iteration as a snapshot makes
    /// mutation during iteration safe and lets the rest of MIR
    /// stay generic over the source type (array / map / set /
    /// string / interval / object).
    MakeForeachIter(Operand),
    /// Construct a closure value. `function_idx` indexes into
    /// `MirProgram.functions` — the lambda's body lives there with
    /// `captures.len()` extra parameter slots prepended to its
    /// regular params. The interpreter evaluates `captures` in the
    /// enclosing frame and binds them to those slots when the
    /// closure is later invoked.
    MakeLambda {
        function_idx: usize,
        captures: Vec<Operand>,
    },
    /// A first-class function reference (named function used as a
    /// value, e.g. `var f = foo`).
    FunctionRef(DefId),
    /// Read of a file-level global.
    GlobalRef(DefId, String),
    /// Reference to a built-in function or constant by name.
    BuiltinRef(String),
    /// `this` reference inside a method body.
    This,
    /// `class` keyword inside a method body — yields the current
    /// class as a value.
    ClassSelf,
    /// `super` reference inside a method body. The receiver is
    /// the same as `this`, but method dispatch on this value
    /// statically resolves against the parent class.
    MakeSuper {
        this: LocalId,
        parent_class: String,
    },
    /// Deprecated marker — kept to avoid breaking call sites; the
    /// lowerer no longer emits this. Treated by the interpreter
    /// the same as `Unsupported("super")`.
    Super,
    /// Class name used as a value (`var c = MyClass`).
    ClassRef(DefId, String),
    /// Placeholder for HIR shapes we don't fully lower yet
    /// (lambdas with captures, class-bound method values, etc.).
    /// The string is a short tag for debugging; backends that hit
    /// this should report `LowerError::Unsupported`.
    Unsupported(&'static str),
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct IntervalRvalue {
    pub start: Option<Operand>,
    pub end: Option<Operand>,
    pub step: Option<Operand>,
    pub start_inclusive: bool,
    pub end_inclusive: bool,
    /// `true` when the source-level endpoint forced real format
    /// (e.g. `Infinity` literal in `]-Infinity..1]`) even though
    /// the runtime value is the same `Real(±inf)` produced by the
    /// `∞` symbol. Lets the display widen the other bound to a
    /// real (`1.0`) without changing the unbounded-sentinel
    /// behaviour of `∞`.
    pub start_forces_real: bool,
    pub end_forces_real: bool,
}

/// A flat value reference. Either a local slot or a literal
/// constant — no nested arithmetic, no nested calls.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Local(LocalId),
    Const(Const),
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Null,
    Bool(bool),
    Int(i64),
    /// Stored as `f64::to_bits` so [`Const`] can be `Eq`/`Hash`able
    /// for the switch-arm table. Convert with [`Const::as_real`].
    Real(u64),
    String(String),
}

impl Eq for Const {}

impl std::hash::Hash for Const {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Const::Null => {}
            Const::Bool(b) => b.hash(state),
            Const::Int(i) => i.hash(state),
            Const::Real(bits) => bits.hash(state),
            Const::String(s) => s.hash(state),
        }
    }
}

impl Const {
    pub fn real(f: f64) -> Self {
        Const::Real(f.to_bits())
    }

    pub fn as_real(&self) -> Option<f64> {
        match self {
            Const::Real(bits) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }
}

// ---- Calls ----

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct CallExpr {
    pub callee: Callee,
    pub args: Vec<Operand>,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Callee {
    /// User-defined function resolved by `DefId`.
    Function(DefId),
    /// Built-in function or constant lookup by name.
    Builtin(String),
    /// `receiver.method(args)`. Receiver is a local because lowering
    /// stashes it in a temp before dispatching.
    Method { receiver: LocalId, method: String },
    /// `expr(args)` where the callee is an arbitrary value (lambda,
    /// stored function reference, etc.). The local holds the
    /// already-evaluated callee value.
    Indirect(LocalId),
    /// `super(args)` from a subclass constructor — dispatch to
    /// `parent_class`'s constructor with `this` as the receiver.
    /// The interpreter looks up the matching constructor by
    /// arity on the parent class.
    SuperConstructor { this: LocalId, parent_class: String },
}

// ---- Operators ----

/// Strict, eager binary operators. Short-circuit operators (`&&`,
/// `||`, `??`) are lowered to CFG control flow before reaching MIR.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    IntDiv,
    Pow,
    Eq,
    Ne,
    IdentityEq,
    IdentityNe,
    Lt,
    Le,
    Gt,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    /// Compound `^=` desugared into a binary op. Distinct from
    /// `BitXor` because the `^=` form means POWER-assign in v1
    /// (`x ^= 5` → `x = x ** 5`) and XOR-assign in v2+. The
    /// interpreter dispatches on its `version` field. Standalone
    /// `^` is always XOR and lowers to `BitXor` instead.
    CompoundXor,
    /// Logical / value XOR (`xor`). Both operands are evaluated;
    /// the result is the boolean XOR of their truthiness. Distinct
    /// from `BitXor` (`^`) which operates on the integer
    /// representations.
    Xor,
    ShiftL,
    ShiftR,
    UShiftR,
    In,
    NotIn,
    Is,
    Instanceof,
}

impl BinOp {
    /// Operations charged for this binary op, mirroring the upstream VM. The
    /// single source of truth shared by the interpreter and the native backend
    /// so both report identical `.ops(N)` counts. Note: `In`/`NotIn` defaults
    /// to 2 (the interval case, the common `.ops` target); an array-RHS `in`
    /// over-charges by 1, matching the interpreter's existing behavior.
    #[must_use]
    pub fn op_cost(self) -> u64 {
        match self {
            BinOp::Mul => 2,
            BinOp::Div | BinOp::IntDiv | BinOp::Mod => 5,
            BinOp::Pow => 40,
            BinOp::CompoundXor => 40, // v1 POW-assign worst case
            BinOp::In | BinOp::NotIn => 2,
            _ => 1,
        }
    }
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Pos,
    Not,
    BitNot,
    /// `@x` — by-reference operator. Identity at the value level
    /// (composites already alias via `Rc`); kept as its own
    /// variant so downstream tools can preserve source intent.
    Ref,
}

/// Explicit conversions. The narrowing/widening that HIR leaves
/// implicit becomes an explicit `Cast` in MIR so backends never
/// have to re-derive it from operand types.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastKind {
    IntToReal,
    RealToInt,
    /// `as bool` / coercion for the condition of a branch.
    ToBool,
    /// `as string` / implicit stringification.
    ToString,
    /// User-written `expr as T` where T isn't one of the special
    /// numeric casts above. The target type lives alongside the
    /// rvalue's containing assignment, so we don't carry it here.
    User,
}
