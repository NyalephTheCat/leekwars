//! Pattern-match loop bodies to extract a symbolic iteration
//! count. Three shapes are supported in slice 1+2:
//!
//! - **C-style `for`** with `i = init; i < bound; i++` /
//!   `i += step` / `i *= step`. Returns a [`LoopBound`] derived
//!   from the comparison's rhs.
//! - **`foreach`** over a parameter-typed expression — the bound
//!   is `Size(param)`.
//! - **`while (cond)` with monotonic counter** — recognise the
//!   simple `i < N` shape where the body monotonically increases
//!   `i` (via `i++`, `i = i + k`, or `i *= k`). Loop iterations =
//!   bound on `i`.
//!
//! When we can't recognise a pattern we return
//! [`LoopBound::Unknown`] with a short reason for diagnostics.

use std::collections::{HashMap, HashSet};

use leek_hir::{
    BinaryOp, Block, Callee, DefId, Expr, ExprKind, Flow, ForStmt, ForeachStmt, NameRef, PostfixOp,
    Stmt, UnaryOp, VarDecl, Visit, Visitable, WhileStmt,
};

use crate::cost_expr::{CostExpr, SizeVar};

/// Per-function name resolution tables the size resolver consults.
/// Built once per analysed function/method body.
pub struct LocalIndex<'a> {
    /// `DefId → name` for top-level globals (so `count(g)` / `g.field`
    /// resolve to a global-rooted size variable).
    pub globals: &'a HashMap<DefId, String>,
    /// `DefId → resolved location` for locals that alias a size
    /// location (`var x = bag.items`), so reads of `x` resolve to the
    /// same place. Reassigned locals are excluded (see
    /// [`build_size_env`]).
    pub locals: &'a HashMap<DefId, SizeVar>,
}

/// A recognised loop iteration count, expressed in terms of size
/// variables of the enclosing function's parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopBound {
    /// Exact constant — `for (var i = 0; i < 5; i++)`.
    Const(u64),
    /// Counts proportional to a size variable — `i < count(arr)`
    /// or `for (x in arr)`.
    Size(SizeVar),
    /// Counts proportional to a size variable, divided by an
    /// integer step — `i < n; i += 4` → `n / 4`. We just record
    /// the size var; the constant factor doesn't matter for big-O.
    SizeOverStep { var: SizeVar, step: u32 },
    /// `log` of a size variable — `i *= 2` halving / doubling loop.
    LogSize(SizeVar),
    /// Couldn't determine the bound symbolically.
    Unknown(&'static str),
}

impl LoopBound {
    /// Convert to a CostExpr suitable for multiplying with a body
    /// cost. `SizeOverStep` collapses to its `Size` factor (the
    /// constant step is asymptotically irrelevant).
    pub fn to_cost_expr(&self) -> CostExpr {
        match self {
            LoopBound::Const(c) => CostExpr::Const(*c),
            LoopBound::Size(v) => CostExpr::Size(v.clone()),
            LoopBound::SizeOverStep { var, .. } => CostExpr::Size(var.clone()),
            LoopBound::LogSize(v) => CostExpr::Log(Box::new(CostExpr::Size(v.clone()))),
            LoopBound::Unknown(reason) => CostExpr::Unknown(reason),
        }
    }
}

/// Context passed to bound recognisers and the size resolver: the
/// parameter table plus the per-body global / local-alias index. The
/// analyser fills this in before walking each function.
pub struct BoundContext<'a> {
    pub params: &'a dyn ParamIndex,
    pub names: &'a LocalIndex<'a>,
}

/// Tiny abstraction so callers can fill in the param table.
pub trait ParamIndex {
    /// Returns `Some(index, name)` if `def` is a parameter of the
    /// function being analysed.
    fn lookup(&self, def: leek_hir::DefId) -> Option<(u32, String)>;
}

/// Resolve a `for (init; cond; step) body` to its bound. Slice-1
/// patterns:
/// - init: `var i = 0` (or any single counter init we can see).
/// - cond: `i < bound` / `i <= bound` (each maps to Const, Size, or
///   Unknown).
/// - step: `i++`, `i += k`, `i *= k`, `++i`, `--i` etc.
pub fn bound_of_for(stmt: &ForStmt, ctx: &BoundContext) -> LoopBound {
    let Some(counter_id) = counter_from_init(stmt.init.as_deref()) else {
        return LoopBound::Unknown("for-loop init isn't a single `var i = ...`");
    };
    let Some(cond) = &stmt.cond else {
        return LoopBound::Unknown("for-loop without condition");
    };
    let Some(comp_bound) = bound_from_condition(cond, counter_id, ctx) else {
        return LoopBound::Unknown("for-loop condition isn't `i < N` / `i <= N`");
    };
    let Some(step) = step_shape(stmt.step.as_ref(), counter_id) else {
        return LoopBound::Unknown("for-loop step isn't a simple counter update");
    };
    apply_step(comp_bound, step)
}

pub fn bound_of_foreach(stmt: &ForeachStmt, ctx: &BoundContext) -> LoopBound {
    bound_from_iter_expr(&stmt.iter, ctx).map_or(
        LoopBound::Unknown("foreach iter isn't a parameter / count(param)"),
        LoopBound::Size,
    )
}

pub fn bound_of_while(stmt: &WhileStmt, ctx: &BoundContext) -> LoopBound {
    // We accept the same `i < N` shape as `for`. The body must
    // monotonically advance `i`; for now we accept loops whose
    // body contains a recognised step (`i++`, `i += k`, `i *= k`)
    // as a top-level statement.
    let cond = &stmt.cond;
    let Some(counter_id) = counter_from_condition(cond) else {
        return LoopBound::Unknown("while-cond isn't `i op N`");
    };
    let Some(comp_bound) = bound_from_condition(cond, counter_id, ctx) else {
        return LoopBound::Unknown("while-cond isn't `i < N`");
    };
    let Some(step) = step_in_body(&stmt.body, counter_id) else {
        return LoopBound::Unknown("while-body has no monotonic step on the counter");
    };
    apply_step(comp_bound, step)
}

// ─── helpers ────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum CounterStep {
    /// `i++`, `++i`, `i += 1` etc.
    PlusOne,
    /// `i += k`.
    PlusK(u32),
    /// `i *= k` (k >= 2) — log iteration count. Step factor is
    /// kept for symmetry but big-O doesn't care.
    #[allow(dead_code)]
    MulK(u32),
    /// Step on a different variable / shape we don't model.
    Unrecognised,
}

fn counter_from_init(init: Option<&Stmt>) -> Option<leek_hir::DefId> {
    match init? {
        Stmt::VarDecl(VarDecl { def, .. }) => Some(*def),
        _ => None,
    }
}

fn counter_from_condition(cond: &Expr) -> Option<leek_hir::DefId> {
    let ExprKind::Binary(op, lhs, rhs) = &cond.kind else {
        return None;
    };
    if !matches!(
        op,
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    ) {
        return None;
    }
    if let Some(id) = local_def_of(lhs) {
        return Some(id);
    }
    local_def_of(rhs)
}

fn local_def_of(e: &Expr) -> Option<leek_hir::DefId> {
    match &e.kind {
        ExprKind::Name(NameRef::Local(id)) => Some(*id),
        _ => None,
    }
}

/// `i < N` / `i <= N` / `N > i` / `N >= i` → bound on `i`.
fn bound_from_condition(
    cond: &Expr,
    counter_id: leek_hir::DefId,
    ctx: &BoundContext,
) -> Option<LoopBound> {
    let ExprKind::Binary(op, lhs, rhs) = &cond.kind else {
        return None;
    };
    // counter `op` bound — only the "counter smaller than bound"
    // shape is accepted; `<`/`<=` with counter on lhs, or
    // `>`/`>=` with counter on rhs.
    let counter_on_lhs = local_def_of(lhs) == Some(counter_id);
    let counter_on_rhs = local_def_of(rhs) == Some(counter_id);
    let bound_side = match (op, counter_on_lhs, counter_on_rhs) {
        (BinaryOp::Lt | BinaryOp::Le, true, _) => rhs,
        (BinaryOp::Gt | BinaryOp::Ge, _, true) => lhs,
        _ => return None,
    };
    bound_from_value_expr(bound_side, ctx)
}

/// Convert an `Expr` representing the loop's bound into a
/// [`LoopBound`]. Recognises:
/// - integer literal → `Const`
/// - any size location ([`resolve_size_var`]) → `Size`
fn bound_from_value_expr(e: &Expr, ctx: &BoundContext) -> Option<LoopBound> {
    if let Some(n) = literal_uint(e) {
        return Some(LoopBound::Const(n));
    }
    resolve_size_var(e, ctx).map(LoopBound::Size)
}

/// `count(x)` / a parameter name / a `this.field` access → its SizeVar.
fn bound_from_iter_expr(e: &Expr, ctx: &BoundContext) -> Option<SizeVar> {
    resolve_size_var(e, ctx)
}

/// Resolve `e` to the [`SizeVar`] of the value it denotes — the central
/// "what is this thing's size?" routine shared by loop bounds and
/// call-site argument sizing.
///
/// It unwraps `count`/`length`/`size`/`mapSize`, then walks an access
/// path down to a *stable root*: a parameter, a global, `this`, or a
/// local that aliases one of those. So it covers a bare parameter
/// (`arr`), a global (`count(g)`), a field off `this` / a parameter /
/// a global (`this.cells`, `obj.cells`, `g.rows`), nested chains
/// (`this.grid.rows`), and locals bound to any of these
/// (`var x = bag.items; … x …`).
///
/// Returns `None` when the size can't be tied to a stable root — an
/// untracked local, a computed expression, or `this` itself (an object,
/// not a sized container).
pub(crate) fn resolve_size_var(e: &Expr, ctx: &BoundContext) -> Option<SizeVar> {
    // `count(x)` / `length(x)` / … → size of `x`.
    if let ExprKind::Call(call) = &e.kind
        && let Callee::Function(NameRef::Builtin(name)) = &call.callee
        && matches!(name.as_str(), "count" | "length" | "size" | "mapSize")
    {
        return resolve_size_var(call.args.first()?, ctx);
    }
    let loc = resolve_location(e, ctx)?;
    // A bare object root (`this`) isn't a sized container — only a field
    // path off it is. A bare parameter or global *is* the container.
    if loc.source.path.is_empty() && matches!(loc.source.root, crate::cost_expr::SizeRoot::This) {
        return None;
    }
    Some(loc)
}

/// Walk an access path (`a.b.c`) down to its root, returning the
/// location as a [`SizeVar`]. Roots are a parameter, a global,
/// `this`/`super`/`class`, or a local aliasing one of those; anything
/// else (an untracked local, a call result, …) yields `None`.
fn resolve_location(e: &Expr, ctx: &BoundContext) -> Option<SizeVar> {
    match &e.kind {
        ExprKind::Name(NameRef::Local(id)) => {
            if let Some((idx, name)) = ctx.params.lookup(*id) {
                Some(SizeVar::new(idx, name))
            } else {
                // A local that aliases a size location.
                ctx.names.locals.get(id).cloned()
            }
        }
        ExprKind::Name(NameRef::Global(id)) => {
            let name = ctx.names.globals.get(id)?;
            Some(SizeVar::global(id.0, name.clone()))
        }
        ExprKind::Name(NameRef::This | NameRef::Super | NameRef::Class_) => {
            Some(SizeVar::this_object())
        }
        ExprKind::Field(receiver, field, _) => {
            let base = resolve_location(receiver, ctx)?;
            Some(base.with_field(field))
        }
        _ => None,
    }
}

// ─── per-body local environment ─────────────────────────────────────

/// Build the local-alias table for `stmts`: each `var x = <init>` whose
/// initializer resolves to a size location maps `x`'s `DefId` to that
/// location, so later reads of `x` resolve to the same place. Locals
/// that are reassigned anywhere in the body are excluded (their value —
/// and thus their size — is no longer pinned to the initializer).
pub(crate) fn build_size_env(
    stmts: &[Stmt],
    params: &dyn ParamIndex,
    globals: &HashMap<DefId, String>,
) -> HashMap<DefId, SizeVar> {
    let reassigned = assigned_locals(stmts);
    let mut env: HashMap<DefId, SizeVar> = HashMap::new();
    let mut builder = SizeEnvBuilder {
        params,
        globals,
        reassigned: &reassigned,
        env: &mut env,
    };
    for s in stmts {
        let _ = s.walk(&mut builder);
    }
    env
}

struct SizeEnvBuilder<'a> {
    params: &'a dyn ParamIndex,
    globals: &'a HashMap<DefId, String>,
    reassigned: &'a HashSet<DefId>,
    env: &'a mut HashMap<DefId, SizeVar>,
}

impl Visit<Stmt> for SizeEnvBuilder<'_> {
    fn visit(&mut self, s: &Stmt) -> Flow {
        if let Stmt::VarDecl(v) = s
            && let Some(init) = &v.init
            && !self.reassigned.contains(&v.def)
        {
            // Resolve against the partial env built so far (so chains
            // like `var a = bag.items; var b = a` work), then record.
            let loc = {
                let ctx = BoundContext {
                    params: self.params,
                    names: &LocalIndex {
                        globals: self.globals,
                        locals: self.env,
                    },
                };
                resolve_location(init, &ctx)
            };
            if let Some(loc) = loc {
                self.env.insert(v.def, loc);
            }
        }
        Flow::Walk
    }
}
impl Visit<Block> for SizeEnvBuilder<'_> {}
impl Visit<Expr> for SizeEnvBuilder<'_> {}

/// Collect the `DefId`s of locals assigned (after declaration) anywhere
/// in `stmts` — via `=`/compound assignment or `++`/`--`. Such locals
/// can't be treated as stable aliases of their initializer.
fn assigned_locals(stmts: &[Stmt]) -> HashSet<DefId> {
    let mut out = HashSet::new();
    let mut finder = AssignFinder { out: &mut out };
    for s in stmts {
        let _ = s.walk(&mut finder);
    }
    out
}

struct AssignFinder<'a> {
    out: &'a mut HashSet<DefId>,
}

impl Visit<Expr> for AssignFinder<'_> {
    fn visit(&mut self, e: &Expr) -> Flow {
        match &e.kind {
            ExprKind::Binary(op, lhs, _) if is_assign_op(*op) => {
                if let Some(id) = local_def_of(lhs) {
                    self.out.insert(id);
                }
            }
            ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec, inner)
            | ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, inner) => {
                if let Some(id) = local_def_of(inner) {
                    self.out.insert(id);
                }
            }
            _ => {}
        }
        Flow::Walk
    }
}
impl Visit<Block> for AssignFinder<'_> {}
impl Visit<Stmt> for AssignFinder<'_> {}

fn is_assign_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Assign
            | BinaryOp::AddAssign
            | BinaryOp::SubAssign
            | BinaryOp::MulAssign
            | BinaryOp::DivAssign
            | BinaryOp::IntDivAssign
            | BinaryOp::ModAssign
            | BinaryOp::PowAssign
            | BinaryOp::BitAndAssign
            | BinaryOp::BitOrAssign
            | BinaryOp::BitXorAssign
            | BinaryOp::ShiftLAssign
            | BinaryOp::ShiftRAssign
            | BinaryOp::UShiftRAssign
            | BinaryOp::NullCoalesceAssign
    )
}

fn literal_uint(e: &Expr) -> Option<u64> {
    match &e.kind {
        ExprKind::Literal(leek_hir::Literal::Int(v)) if *v >= 0 => {
            Some(u64::try_from(*v).expect("non-negative by guard"))
        }
        _ => None,
    }
}

/// Recognise a step expression on the counter variable.
fn step_shape(step: Option<&Expr>, counter_id: leek_hir::DefId) -> Option<CounterStep> {
    let e = step?;
    match &e.kind {
        ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, inner) => {
            if local_def_of(inner) == Some(counter_id) {
                Some(CounterStep::PlusOne)
            } else {
                Some(CounterStep::Unrecognised)
            }
        }
        ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec, inner) => {
            if local_def_of(inner) == Some(counter_id) {
                Some(CounterStep::PlusOne)
            } else {
                Some(CounterStep::Unrecognised)
            }
        }
        ExprKind::Binary(BinaryOp::AddAssign, lhs, rhs) => {
            if local_def_of(lhs) != Some(counter_id) {
                return Some(CounterStep::Unrecognised);
            }
            literal_uint(rhs).map(|k| {
                if k == 1 {
                    CounterStep::PlusOne
                } else {
                    CounterStep::PlusK(u32::try_from(k).unwrap_or(u32::MAX))
                }
            })
        }
        ExprKind::Binary(BinaryOp::MulAssign, lhs, rhs) => {
            if local_def_of(lhs) != Some(counter_id) {
                return Some(CounterStep::Unrecognised);
            }
            literal_uint(rhs)
                .filter(|k| *k >= 2)
                .map(|k| CounterStep::MulK(u32::try_from(k).unwrap_or(u32::MAX)))
        }
        _ => Some(CounterStep::Unrecognised),
    }
}

/// Walk a while-body's top-level statements for a step on the
/// counter. We accept the same shapes as `step_shape`.
fn step_in_body(body: &Stmt, counter_id: leek_hir::DefId) -> Option<CounterStep> {
    let stmts = stmts_of(body);
    for s in stmts {
        // Walk through Expr statements; nested control flow is
        // too risky to claim monotonicity.
        if let Stmt::Expr(e) = s
            && let Some(step) = step_shape(Some(e), counter_id)
            && !matches!(step, CounterStep::Unrecognised)
        {
            return Some(step);
        }
    }
    None
}

fn stmts_of(s: &Stmt) -> &[Stmt] {
    match s {
        Stmt::Block(Block { stmts, .. }) => stmts,
        _ => std::slice::from_ref(s),
    }
}

/// Combine a comparison-derived bound with a step shape to get the
/// final iteration count.
fn apply_step(bound: LoopBound, step: CounterStep) -> LoopBound {
    match (bound, step) {
        (b, CounterStep::PlusOne) => b,
        (LoopBound::Size(v), CounterStep::PlusK(k)) => LoopBound::SizeOverStep { var: v, step: k },
        (LoopBound::Const(c), CounterStep::PlusK(k)) => LoopBound::Const(c / u64::from(k)),
        (LoopBound::Size(v), CounterStep::MulK(_)) => LoopBound::LogSize(v),
        (LoopBound::Const(c), CounterStep::MulK(_)) => {
            // log_k(c) — but k is filtered to ≥2 elsewhere, so
            // log2 is a safe upper bound.
            LoopBound::Const(64 - u64::from(c.leading_zeros()))
        }
        (_, CounterStep::Unrecognised) => LoopBound::Unknown("for-loop step shape not recognised"),
        (b, _) => b,
    }
}
