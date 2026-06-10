//! HIR data types.
//!
//! The HIR is a typed, name-resolved tree. Each name use carries a
//! [`DefId`] pointing back to its declaration; each expression
//! carries its inferred [`Type`]. The structure mirrors the AST
//! shape but drops the rowan node references — at this layer we own
//! the tree.
//!
//! Spans are kept on every node so diagnostics from later passes
//! can still attribute errors to source positions.

use leek_span::Span;
use leek_types::Type;

leek_span::newtype_index! {
    /// Stable identifier for a definition within a single HIR file.
    /// Globals, functions, classes, methods, parameters, and locals
    /// each get a unique `DefId`.
    pub struct DefId;
    display = "Def#{}";
}

/// Per-file HIR. Top-level statements (the "main block") run in the
/// order they appear, threaded with the items in declaration order.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HirFile {
    /// All declarations indexed by `DefId.0`. Position is the
    /// canonical slot for the `DefId` to look up its definition.
    pub defs: Vec<Def>,
    /// Top-level items in source order. The interpreter walks these
    /// after class/function items have been pre-registered.
    pub items: Vec<ItemId>,
    /// Top-level statements (the "main block"). Run after all items
    /// are registered.
    pub main: Vec<Stmt>,
}

/// Pointer into [`HirFile::defs`]. Same shape as `DefId` but kept
/// distinct so signatures clearly express intent.
pub type ItemId = DefId;

/// A registered declaration. The body lives separately so multiple
/// queries (resolver, type checker, interpreter) can share the
/// signature view without cloning the body.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Def {
    Function(Function),
    Class(Class),
    Global(Global),
    /// A local variable / parameter. Body is None; only the signature
    /// fields (name, span, type) are used.
    Local(Local),
}

impl Def {
    pub fn name(&self) -> &str {
        match self {
            Def::Function(f) => &f.name,
            Def::Class(c) => &c.name,
            Def::Global(g) => &g.name,
            Def::Local(l) => &l.name,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            Def::Function(f) => f.span,
            Def::Class(c) => c.span,
            Def::Global(g) => g.span,
            Def::Local(l) => l.span,
        }
    }
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub span: Span,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub body: Option<Block>,
    /// `(backend, body)` directives parsed from the function's doc
    /// comment (`@java-backend: …`). Only populated for bodiless
    /// signatures in signature-file mode; empty for ordinary functions.
    /// Backends substitute the body at call sites.
    pub backend_directives: Vec<(String, String)>,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub def: DefId,
    pub name: String,
    pub ty: Option<Type>,
    pub default: Option<Expr>,
    /// `@x` reference param — the callee's slot shares storage
    /// with the caller's. Writes inside the callee propagate back.
    pub is_by_ref: bool,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    pub name: String,
    pub span: Span,
    pub parent: Option<String>,
    pub fields: Vec<Field>,
    pub methods: Vec<MethodDef>,
    pub constructors: Vec<MethodDef>,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub def: DefId,
    pub name: String,
    pub ty: Option<Type>,
    pub init: Option<Expr>,
    pub is_static: bool,
    pub is_final: bool,
    pub visibility: Visibility,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct MethodDef {
    pub def: DefId,
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<Type>,
    pub body: Option<Block>,
    pub is_static: bool,
    pub visibility: Visibility,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
    Protected,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Global {
    pub name: String,
    pub ty: Option<Type>,
    pub init: Option<Expr>,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Local {
    pub name: String,
    pub ty: Option<Type>,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// Bare expression statement: `foo()`, `x + 1`, etc.
    Expr(Expr),
    /// `var x = 1` / `integer x = 1` / `global x = 1`.
    VarDecl(VarDecl),
    /// `return [expr]`.
    Return(Option<Expr>),
    If(IfStmt),
    While(WhileStmt),
    DoWhile(DoWhileStmt),
    For(ForStmt),
    Foreach(ForeachStmt),
    Break(Span),
    Continue(Span),
    Block(Block),
    Switch(SwitchStmt),
    /// `include("file")` — included at the parser stage, kept here as
    /// a placeholder for source-map round-tripping.
    Include(IncludeStmt),
    /// `import foo.bar` / `import "foo.bar"` — library import handled
    /// by resolver/runtime plumbing.
    Import(ImportStmt),
    /// Static op-budget deduction synthesized by the
    /// [`leek-charge`] pass. Canonical HIR has no `Charge` nodes;
    /// backends that don't enforce a budget can ignore this variant.
    ///
    /// Only carries a *static* count — dynamic, input-scaled costs
    /// (e.g. `replace(s, a, b)` ≈ `len(s) * len(a)`) are the
    /// responsibility of each builtin's implementation. Java's
    /// runtime `replace` calls `ai.ops(...)` internally; the
    /// interpreter's `replace` bumps its counter inside Rust. The
    /// pass therefore only ever needs to express the constant
    /// per-statement/per-expression overhead.
    ///
    /// Never user-writable.
    ///
    /// [`leek-charge`]: ../../leek-charge/index.html
    Charge(u64),
}

impl Stmt {
    /// Source span of this statement. `Return(None)` and `Charge` carry no
    /// span of their own and yield a zero-width synthetic span.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Stmt::Expr(e) => e.span,
            Stmt::VarDecl(v) => v.span,
            Stmt::Return(Some(e)) => e.span,
            Stmt::If(i) => i.span,
            Stmt::While(w) => w.span,
            Stmt::DoWhile(d) => d.span,
            Stmt::For(f) => f.span,
            Stmt::Foreach(fe) => fe.span,
            Stmt::Switch(s) => s.span,
            Stmt::Block(b) => b.span,
            Stmt::Break(s) | Stmt::Continue(s) => *s,
            Stmt::Include(i) => i.span,
            Stmt::Import(i) => i.span,
            Stmt::Return(None) | Stmt::Charge(_) => {
                Span::synthetic()
            }
        }
    }
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct VarDecl {
    pub def: DefId,
    pub name: String,
    pub ty: Option<Type>,
    pub init: Option<Expr>,
    pub is_global: bool,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct IfStmt {
    pub cond: Expr,
    pub then_branch: Box<Stmt>,
    pub else_branch: Option<Box<Stmt>>,
    /// `true` when this `if` is the lowering of a soft return (`return? x` →
    /// `if (x) return x`). The reference compiles a soft return without the
    /// per-`if` op tick on the condition, so backends skip it.
    pub soft: bool,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct WhileStmt {
    pub cond: Expr,
    pub body: Box<Stmt>,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct DoWhileStmt {
    pub body: Box<Stmt>,
    pub cond: Expr,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct ForStmt {
    pub init: Option<Box<Stmt>>,
    pub cond: Option<Expr>,
    pub step: Option<Expr>,
    pub body: Box<Stmt>,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct ForeachStmt {
    /// Optional key binding (`for (k : v in arr)`).
    pub key: Option<ForeachBind>,
    pub value: ForeachBind,
    pub iter: Expr,
    pub body: Box<Stmt>,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct ForeachBind {
    pub def: DefId,
    pub name: String,
    pub is_new: bool,
    /// `@`-by-reference iterator (`for (var @v in arr)`). At v1 a by-ref value
    /// binding is set through a runtime `Box` and costs one op per iteration,
    /// where a by-value declaration costs two.
    pub is_by_ref: bool,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchStmt {
    pub discriminant: Expr,
    pub arms: Vec<SwitchArm>,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchArm {
    /// `None` for the `default` arm.
    pub case: Option<Expr>,
    pub body: Vec<Stmt>,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct IncludeStmt {
    pub path: String,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct ImportStmt {
    pub path: String,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub span: Span,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Literal(Literal),
    /// Resolved name reference.
    Name(NameRef),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    Postfix(PostfixOp, Box<Expr>),
    /// `f(args)` or `obj.m(args)` — `callee` carries the resolution.
    Call(Box<Call>),
    /// `obj.field` — read.
    Field(Box<Expr>, String),
    /// `obj[index]`.
    Index(Box<Expr>, Box<Expr>),
    /// `obj[a:b:c]`.
    Slice(SliceExpr),
    /// `[a, b, c]` literal.
    Array(Vec<Expr>),
    /// `[k: v, k: v]` literal.
    Map(Vec<(Expr, Expr)>),
    /// `{a, b, c}` / `<a, b, c>` set literal.
    Set(Vec<Expr>),
    /// `{f: v, …}` object literal — keys are identifiers.
    Object(Vec<(String, Expr)>),
    /// `cond ? then : else`.
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `[a..b]`-style interval.
    Interval(IntervalExpr),
    /// `expr as Type`.
    Cast(Box<Expr>, Type),
    /// `new ClassName(args)`.
    New(NewExpr),
    /// Lambda / anonymous function.
    Lambda(LambdaExpr),
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub callee: Callee,
    pub args: Vec<Expr>,
    pub span: Span,
}

/// Resolved call target. `Method` and `StaticMethod` keep the
/// receiver expression separately so the interpreter can evaluate
/// `this`/`obj` before dispatching.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Callee {
    /// Bare function call: `foo(args)`.
    Function(NameRef),
    /// `obj.m(args)`.
    Method { receiver: Expr, method: String },
    /// Arbitrary callable expression: `(some_expr)(args)`.
    Expr(Expr),
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct SliceExpr {
    pub base: Box<Expr>,
    pub start: Option<Box<Expr>>,
    pub end: Option<Box<Expr>>,
    pub step: Option<Box<Expr>>,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct IntervalExpr {
    pub start: Option<Box<Expr>>,
    pub end: Option<Box<Expr>>,
    pub step: Option<Box<Expr>>,
    pub start_inclusive: bool,
    pub end_inclusive: bool,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct NewExpr {
    pub class: String,
    pub args: Vec<Expr>,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct LambdaExpr {
    pub params: Vec<Param>,
    pub body: LambdaBody,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum LambdaBody {
    Block(Block),
    Expr(Box<Expr>),
}

/// Resolved name reference. The interpreter dispatches on this.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum NameRef {
    /// Local variable / parameter (declared in some enclosing scope).
    Local(DefId),
    /// Top-level global.
    Global(DefId),
    /// User-defined function.
    Function(DefId),
    /// Class name (e.g. as a value or for `ClassName.staticMember`).
    Class(DefId),
    /// Builtin function or constant.
    Builtin(String),
    /// `this` inside a method body.
    This,
    /// `super` inside a method body.
    Super,
    /// `class` inside a method body — yields the current class.
    Class_,
    /// Couldn't resolve. The interpreter treats this as null in
    /// non-strict, errors in strict.
    Unresolved(String),
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Real(f64),
    String(String),
    Bool(bool),
    Null,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    /// Integer division (`\`).
    IntDiv,
    Pow,
    Eq,
    Ne,
    /// Identity equality (`===`).
    IdentityEq,
    /// Identity inequality (`!==`).
    IdentityNe,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Xor,
    BitAnd,
    BitOr,
    BitXor,
    ShiftL,
    ShiftR,
    UShiftR,
    NullCoalesce,
    /// `in`, `not in`, `is`, `instanceof` — relational keyword ops.
    In,
    NotIn,
    Is,
    Instanceof,
    // Assignment family. Lowering preserves the compound form so
    // back-ends that need exact-source mirroring can re-emit it.
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    IntDivAssign,
    ModAssign,
    PowAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    ShiftLAssign,
    ShiftRAssign,
    UShiftRAssign,
    NullCoalesceAssign,
}

impl BinaryOp {
    pub fn is_assignment(self) -> bool {
        matches!(
            self,
            Self::Assign
                | Self::AddAssign
                | Self::SubAssign
                | Self::MulAssign
                | Self::DivAssign
                | Self::IntDivAssign
                | Self::ModAssign
                | Self::PowAssign
                | Self::BitAndAssign
                | Self::BitOrAssign
                | Self::BitXorAssign
                | Self::ShiftLAssign
                | Self::ShiftRAssign
                | Self::UShiftRAssign
                | Self::NullCoalesceAssign
        )
    }

    /// For compound assignments, return the underlying binary op
    /// (`+=` → `Add`). Returns `None` for plain `=`.
    pub fn compound_base(self) -> Option<BinaryOp> {
        Some(match self {
            Self::AddAssign => Self::Add,
            Self::SubAssign => Self::Sub,
            Self::MulAssign => Self::Mul,
            Self::DivAssign => Self::Div,
            Self::IntDivAssign => Self::IntDiv,
            Self::ModAssign => Self::Mod,
            Self::PowAssign => Self::Pow,
            Self::BitAndAssign => Self::BitAnd,
            Self::BitOrAssign => Self::BitOr,
            Self::BitXorAssign => Self::BitXor,
            Self::ShiftLAssign => Self::ShiftL,
            Self::ShiftRAssign => Self::ShiftR,
            Self::UShiftRAssign => Self::UShiftR,
            Self::NullCoalesceAssign => Self::NullCoalesce,
            _ => return None,
        })
    }
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Numeric negation `-x`.
    Neg,
    /// Numeric plus `+x` (no-op for numbers, conversion sigil for
    /// strings).
    Pos,
    /// Logical not `!x` / `not x`.
    Not,
    /// Bitwise not `~x`.
    BitNot,
    /// Prefix increment `++x`.
    PreInc,
    /// Prefix decrement `--x`.
    PreDec,
    /// `@x` — the by-reference / pass-by-ref operator. In v3+
    /// everything is reference-typed by default so `@` is a
    /// semantic no-op; v1/v2 use it to box primitives. Lowered as
    /// identity by both backends.
    Ref,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostfixOp {
    /// `x++`.
    PostInc,
    /// `x--`.
    PostDec,
    /// `x!` — non-null assertion. Kept as a tag here for backends.
    NonNull,
}
