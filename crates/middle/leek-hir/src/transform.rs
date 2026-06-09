//! Backend-agnostic HIR transforms.
//!
//! [`fold_constants`] rewrites references to named constants into literal
//! values. It is an *opt-in* pass — the caller supplies the `name →
//! literal` map and decides when to run it — so it stays decoupled from
//! any particular constant source. Its motivating use is the leek-wars
//! fight constants (`WEAPON_PISTOL` → `37`), whose values a driver pulls
//! from `leek_environment::leekwars_constant_values()`; because the pass
//! mutates the shared HIR, *every* backend (Java, native, interpreter)
//! then sees a plain literal instead of an undefined identifier.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use leek_types::Type;

use crate::ir::{
    BinaryOp, Block, Callee, Def, DefId, Expr, ExprKind, HirFile, LambdaBody, Literal, NameRef,
    PostfixOp, Stmt, UnaryOp,
};
use crate::visit::{
    Flow, VisitMut, VisitableMut, walk_expr_children, walk_expr_children_mut,
    walk_stmt_child_exprs, walk_stmt_child_exprs_mut, walk_stmt_child_stmts,
    walk_stmt_child_stmts_mut,
};

/// Replace every `Name(Builtin(name))` whose `name` is a key in `values`
/// with the corresponding literal, throughout `hir` (function bodies,
/// class members + field inits, globals, and the main block). Returns the
/// number of substitutions made.
///
/// Only `NameRef::Builtin` is folded: those are the unresolved
/// builtin/constant identifiers (a registered environment constant lowers
/// to one). Locals, globals, functions, and classes are real bindings and
/// are left untouched.
pub fn fold_constants(hir: &mut HirFile, values: &HashMap<String, Literal>) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut folder = ConstFolder { values, count: 0 };
    for def in &mut hir.defs {
        match def {
            Def::Function(f) => {
                if let Some(b) = &mut f.body {
                    let _ = b.walk_mut(&mut folder);
                }
            }
            Def::Class(c) => {
                for field in &mut c.fields {
                    if let Some(e) = &mut field.init {
                        let _ = e.walk_mut(&mut folder);
                    }
                }
                for m in c.methods.iter_mut().chain(c.constructors.iter_mut()) {
                    if let Some(b) = &mut m.body {
                        let _ = b.walk_mut(&mut folder);
                    }
                }
            }
            Def::Global(g) => {
                if let Some(e) = &mut g.init {
                    let _ = e.walk_mut(&mut folder);
                }
            }
            Def::Local(_) => {}
        }
    }
    for s in &mut hir.main {
        let _ = s.walk_mut(&mut folder);
    }
    folder.count
}

/// In-place constant-folding visitor. The only node it rewrites is a
/// `Name(Builtin(name))` whose `name` is in `values`; everything else is
/// left for the framework to recurse into. Substituting a literal prunes
/// recursion (a literal has no children) via [`Flow::Skip`].
struct ConstFolder<'a> {
    values: &'a HashMap<String, Literal>,
    count: usize,
}

impl VisitMut<Expr> for ConstFolder<'_> {
    fn visit_mut(&mut self, e: &mut Expr) -> Flow {
        if let ExprKind::Name(NameRef::Builtin(name)) = &e.kind
            && let Some(lit) = self.values.get(name)
        {
            e.kind = ExprKind::Literal(lit.clone());
            self.count += 1;
            return Flow::Skip; // a literal has no children to recurse into
        }
        Flow::Walk
    }
}

// `ConstFolder` only reacts to expressions; blocks and statements use the
// `VisitMut` default no-op so the `HirVisitorMut` umbrella is satisfied.
impl VisitMut<Block> for ConstFolder<'_> {}
impl VisitMut<Stmt> for ConstFolder<'_> {}

/// Run the backend-agnostic HIR optimization passes to a fixpoint.
///
/// Constant propagation and folding feed each other: propagating `A` into
/// `var B = A + 1` lets folding reduce it to `var B = 3`, which then makes `B`
/// itself a propagation candidate on the next round (`var C = B * 2` → `6`). A
/// single linear pass stops after one such step, so we iterate until nothing
/// changes. Each pass only ever replaces a sub-tree with a smaller constant or
/// drops a dead declaration, so the program shrinks monotonically and the loop
/// converges quickly; the bound is a safety backstop, not an expected limit.
///
/// Returns the total number of rewrites across all rounds.
pub fn optimize_hir(hir: &mut HirFile) -> usize {
    const MAX_ROUNDS: usize = 8;
    let mut total = 0;
    for _ in 0..MAX_ROUNDS {
        let mut changed = 0;
        changed += propagate_const_globals(hir);
        changed += propagate_const_locals(hir);
        changed += inline_calls(hir);
        changed += fold_expressions(hir);
        changed += eliminate_dead_statements(hir);
        total += changed;
        if changed == 0 {
            break;
        }
    }
    total
}

/// Evaluate constant sub-expressions to literals, in place, throughout
/// `hir`. Returns the number of expressions rewritten.
///
/// This is a *codegen* optimization: every folded expression is one fewer
/// node the [`leek-charge`](../../leek-charge/index.html) pass charges for,
/// so it directly lowers a program's static op budget on every backend.
/// Run it *after* [`fold_constants`] so substituted constants (e.g.
/// `WEAPON_PISTOL` → `37`) become foldable operands (`WEAPON_PISTOL == 37`
/// → `true`).
///
/// Folding is intentionally conservative: it rewrites only operations whose
/// result is identical across all backends regardless of language version —
/// integer (wrapping) and real `+`/`-`/`*`, numeric ordering comparisons,
/// same-type / numeric `==`/`!=`, boolean `&&`/`||`/`xor`, integer bitwise
/// and shift ops, the corresponding unary ops, and collapsing a ternary with
/// a constant boolean condition to the taken branch. Operations with
/// version- or coercion-dependent semantics (`/`, `\`, `%`, `**`, `??`,
/// `in`, identity ops, mixed string/number `==`, casts) are left untouched.
pub fn fold_expressions(hir: &mut HirFile) -> usize {
    let mut folder = ExprFolder { count: 0 };
    for def in &mut hir.defs {
        match def {
            Def::Function(f) => {
                if let Some(b) = &mut f.body {
                    folder.block(b);
                }
            }
            Def::Class(c) => {
                for field in &mut c.fields {
                    if let Some(e) = &mut field.init {
                        folder.expr(e);
                    }
                }
                for m in c.methods.iter_mut().chain(c.constructors.iter_mut()) {
                    if let Some(b) = &mut m.body {
                        folder.block(b);
                    }
                }
            }
            Def::Global(g) => {
                if let Some(e) = &mut g.init {
                    folder.expr(e);
                }
            }
            Def::Local(_) => {}
        }
    }
    for s in &mut hir.main {
        folder.stmt(s);
    }
    folder.count
}

/// Post-order constant evaluator. Children are folded before their parent so
/// nested constants collapse in a single pass (`1 + 2 + 3` → `6`).
struct ExprFolder {
    count: usize,
}

impl ExprFolder {
    fn block(&mut self, b: &mut Block) {
        for s in &mut b.stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        walk_stmt_child_exprs_mut(s, &mut |e| self.expr(e));
        walk_stmt_child_stmts_mut(s, &mut |s2| self.stmt(s2));
    }

    fn expr(&mut self, e: &mut Expr) {
        // `walk_expr_children_mut` treats a lambda as a leaf; descend into its
        // body and parameter defaults explicitly so their constants fold too.
        if let ExprKind::Lambda(lam) = &mut e.kind {
            for p in &mut lam.params {
                if let Some(d) = &mut p.default {
                    self.expr(d);
                }
            }
            match &mut lam.body {
                LambdaBody::Block(b) => self.block(b),
                LambdaBody::Expr(inner) => self.expr(inner),
            }
            return;
        }
        walk_expr_children_mut(e, &mut |c| self.expr(c));
        self.try_fold(e);
    }

    /// Attempt to rewrite `e` itself, assuming its children are already
    /// folded. The expression's [`Type`](leek_types::Type) is preserved: a
    /// folded result has the same type the type checker assigned the node.
    fn try_fold(&mut self, e: &mut Expr) {
        // Collapse `const_bool ? a : b` to the taken branch.
        if let ExprKind::Ternary(cond, then_e, else_e) = &mut e.kind {
            if let ExprKind::Literal(Literal::Bool(b)) = cond.kind {
                let taken = if b { then_e } else { else_e };
                let chosen = std::mem::replace(taken, Box::new(placeholder()));
                *e = *chosen;
                self.count += 1;
            }
            return;
        }
        let folded = match &e.kind {
            ExprKind::Binary(op, l, r) => match (&l.kind, &r.kind) {
                (ExprKind::Literal(a), ExprKind::Literal(b)) => fold_binary(*op, a, b),
                _ => None,
            },
            ExprKind::Unary(op, x) => match &x.kind {
                ExprKind::Literal(a) => fold_unary(*op, a),
                _ => None,
            },
            // A call to a pure, cross-backend-deterministic builtin on constant
            // arguments (`abs(-5)` → `5`). `NameRef::Builtin` means the resolver
            // confirmed the stdlib function (a user redefinition resolves to
            // `Function` instead), so this is safe.
            ExprKind::Call(c) => match &c.callee {
                Callee::Function(NameRef::Builtin(name)) => fold_builtin_call(name, &c.args),
                _ => None,
            },
            _ => None,
        };
        if let Some(lit) = folded {
            e.kind = ExprKind::Literal(lit);
            self.count += 1;
        }
    }
}

/// A throwaway expression used only as the `mem::replace` placeholder when
/// moving the taken branch out of a folded ternary. Never observed.
fn placeholder() -> Expr {
    Expr {
        kind: ExprKind::Literal(Literal::Null),
        ty: leek_types::Type::Any,
        span: leek_span::Span::synthetic(),
    }
}

/// The numeric (`int`/`real`) view of a literal, or `None` for non-numbers.
#[allow(clippy::cast_precision_loss)]
fn as_real(l: &Literal) -> Option<f64> {
    match l {
        Literal::Int(i) => Some(*i as f64),
        Literal::Real(r) => Some(*r),
        _ => None,
    }
}

/// Fold a binary op applied to two literal operands. Returns `None` when the
/// op/operand combination is outside the version-independent safe set, so the
/// caller leaves the expression untouched.
fn fold_binary(op: BinaryOp, a: &Literal, b: &Literal) -> Option<Literal> {
    use Literal::{Bool, Int, Real, String};
    match op {
        // Arithmetic: integer ops wrap (matching the interpreter's
        // `wrapping_*` and Java `long`); a real operand promotes to real.
        // `String + String` concatenates. Mixed string/number `+` is left
        // alone (its coercion rules are backend-sensitive).
        BinaryOp::Add => match (a, b) {
            (Int(x), Int(y)) => Some(Int(x.wrapping_add(*y))),
            (String(x), String(y)) => Some(String(format!("{x}{y}"))),
            _ => Some(Real(as_real(a)? + as_real(b)?)),
        },
        BinaryOp::Sub => match (a, b) {
            (Int(x), Int(y)) => Some(Int(x.wrapping_sub(*y))),
            _ => Some(Real(as_real(a)? - as_real(b)?)),
        },
        BinaryOp::Mul => match (a, b) {
            (Int(x), Int(y)) => Some(Int(x.wrapping_mul(*y))),
            _ => Some(Real(as_real(a)? * as_real(b)?)),
        },
        // Ordering comparisons on numbers. A non-orderable pair (NaN) yields
        // `None`, matching the interpreter's "no ordering → false" only by
        // declining to fold — safe either way.
        BinaryOp::Lt => num_cmp(a, b, |o| o == Ordering::Less),
        BinaryOp::Le => num_cmp(a, b, |o| o != Ordering::Greater),
        BinaryOp::Gt => num_cmp(a, b, |o| o == Ordering::Greater),
        BinaryOp::Ge => num_cmp(a, b, |o| o != Ordering::Less),
        // Equality only for same-type primitives or two numbers — these are
        // version-independent. Mixed string/number/bool/null are not folded.
        BinaryOp::Eq => prim_eq(a, b).map(Bool),
        BinaryOp::Ne => prim_eq(a, b).map(|v| Bool(!v)),
        // Boolean logic on boolean literals (both operands are constant, so
        // short-circuiting is moot).
        BinaryOp::And => bool_pair(a, b, |x, y| x && y),
        BinaryOp::Or => bool_pair(a, b, |x, y| x || y),
        BinaryOp::Xor => bool_pair(a, b, |x, y| x ^ y),
        // Integer bitwise and shift ops (shift amount masked to 0..64 like
        // the interpreter).
        BinaryOp::BitAnd => int_pair(a, b, |x, y| x & y),
        BinaryOp::BitOr => int_pair(a, b, |x, y| x | y),
        BinaryOp::BitXor => int_pair(a, b, |x, y| x ^ y),
        BinaryOp::ShiftL => int_pair(a, b, |x, y| x << (y & 63)),
        BinaryOp::ShiftR => int_pair(a, b, |x, y| x >> (y & 63)),
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
        BinaryOp::UShiftR => int_pair(a, b, |x, y| ((x as u64) >> (y & 63)) as i64),
        _ => None,
    }
}

fn num_cmp(a: &Literal, b: &Literal, pred: impl Fn(Ordering) -> bool) -> Option<Literal> {
    let ord = match (a, b) {
        (Literal::Int(x), Literal::Int(y)) => x.cmp(y),
        _ => as_real(a)?.partial_cmp(&as_real(b)?)?,
    };
    Some(Literal::Bool(pred(ord)))
}

fn prim_eq(a: &Literal, b: &Literal) -> Option<bool> {
    use Literal::{Bool, Int, Real, String};
    match (a, b) {
        (Int(x), Int(y)) => Some(x == y),
        (Bool(x), Bool(y)) => Some(x == y),
        (String(x), String(y)) => Some(x == y),
        (Real(x), Real(y)) => Some((x - y).abs() == 0.0),
        (Int(_) | Real(_), Int(_) | Real(_)) => Some(as_real(a)? == as_real(b)?),
        _ => None,
    }
}

fn bool_pair(a: &Literal, b: &Literal, f: impl Fn(bool, bool) -> bool) -> Option<Literal> {
    match (a, b) {
        (Literal::Bool(x), Literal::Bool(y)) => Some(Literal::Bool(f(*x, *y))),
        _ => None,
    }
}

fn int_pair(a: &Literal, b: &Literal, f: impl Fn(i64, i64) -> i64) -> Option<Literal> {
    match (a, b) {
        (Literal::Int(x), Literal::Int(y)) => Some(Literal::Int(f(*x, *y))),
        _ => None,
    }
}

/// Fold a unary op applied to a literal operand. `None` outside the safe set.
fn fold_unary(op: UnaryOp, a: &Literal) -> Option<Literal> {
    match (op, a) {
        // `wrapping_neg` so `-i64::MIN` doesn't overflow, matching `rt::neg`.
        (UnaryOp::Neg, Literal::Int(i)) => Some(Literal::Int(i.wrapping_neg())),
        (UnaryOp::Neg, Literal::Real(r)) => Some(Literal::Real(-r)),
        // `+x` is identity on numbers.
        (UnaryOp::Pos, Literal::Int(i)) => Some(Literal::Int(*i)),
        (UnaryOp::Pos, Literal::Real(r)) => Some(Literal::Real(*r)),
        (UnaryOp::Not, Literal::Bool(b)) => Some(Literal::Bool(!b)),
        (UnaryOp::BitNot, Literal::Int(i)) => Some(Literal::Int(!i)),
        _ => None,
    }
}

/// Evaluate a call to a pure math builtin on constant literal arguments,
/// returning the result literal. `None` for anything outside the whitelist or
/// with non-literal / wrong-arity / out-of-domain arguments.
///
/// The whitelist is restricted to functions whose result is **bit-identical
/// across every backend** (the Rust interpreter/native share one C
/// implementation, and the Java backend agrees): integer/real selection
/// (`abs`, `min`, `max`), exact flooring (`floor`, `ceil`), and IEEE-754
/// correctly-rounded `sqrt`. `round` (rounding-mode dependent) and the
/// transcendentals (`sin`/`cos`/`pow`/… — libm may differ across platforms)
/// are deliberately excluded. Semantics mirror
/// `leek_runtime`'s builtins exactly (e.g. `abs(i64::MIN)` wraps; `floor`
/// returns an integer).
fn fold_builtin_call(name: &str, args: &[Expr]) -> Option<Literal> {
    // Every argument must already be a literal.
    let mut lits = Vec::with_capacity(args.len());
    for a in args {
        match &a.kind {
            ExprKind::Literal(l) => lits.push(l),
            _ => return None,
        }
    }
    match (name, lits.as_slice()) {
        ("abs", [a]) => match a {
            // `wrapping_abs` matches `rt::abs` (`abs(i64::MIN)` stays `i64::MIN`).
            Literal::Int(i) => Some(Literal::Int(i.wrapping_abs())),
            Literal::Real(r) => Some(Literal::Real(r.abs())),
            _ => None,
        },
        ("min", [a, b]) => fold_min_max(a, b, true),
        ("max", [a, b]) => fold_min_max(a, b, false),
        ("floor", [a]) => fold_floor_ceil(a, true),
        ("ceil", [a]) => fold_floor_ceil(a, false),
        ("sqrt", [a]) => {
            let x = as_real(a)?;
            // sqrt of a negative is NaN, which we never want as a literal.
            if x < 0.0 {
                return None;
            }
            let r = x.sqrt();
            r.is_finite().then_some(Literal::Real(r))
        }
        _ => None,
    }
}

/// `min`/`max` on two numeric literals: two integers stay an integer (the
/// selected value); a real operand promotes the result to real, mirroring the
/// interpreter's `<=` / `>=` selection.
fn fold_min_max(a: &Literal, b: &Literal, is_min: bool) -> Option<Literal> {
    if let (Literal::Int(x), Literal::Int(y)) = (a, b) {
        return Some(Literal::Int(if is_min { *x.min(y) } else { *x.max(y) }));
    }
    let (fx, fy) = (as_real(a)?, as_real(b)?);
    let take_first = if is_min { fx <= fy } else { fx >= fy };
    Some(Literal::Real(if take_first { fx } else { fy }))
}

/// `floor`/`ceil` of a *real* literal → integer, only when the result fits an
/// `i64` exactly (so we don't have to replicate the runtime's out-of-range
/// saturation). Integer arguments are left unfolded — flooring an integer is a
/// no-op rarely written, and round-tripping a large `i64` through `f64` could
/// lose precision.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn fold_floor_ceil(a: &Literal, is_floor: bool) -> Option<Literal> {
    let Literal::Real(r) = a else { return None };
    let v = if is_floor { r.floor() } else { r.ceil() };
    if v.is_finite() && v >= i64::MIN as f64 && v <= i64::MAX as f64 {
        Some(Literal::Int(v as i64))
    } else {
        None
    }
}

/// Substitute immutable `var x = <literal>` locals at their uses, then drop the
/// now-dead declarations. Returns the number of reads rewritten.
///
/// This is a *codegen* optimization run before [`fold_expressions`]: replacing a
/// constant-valued variable exposes its uses to folding (`var W = 5; a * W` →
/// `a * 5` → foldable when `a` is itself constant) and a constant `if (FLAG)`
/// then collapses in the MIR control-flow pass. It directly lowers a program's
/// op budget on every backend.
///
/// A local qualifies only when it is provably immutable and uncoerced:
/// - declared `var x = <literal>` (not `global`),
/// - the literal's type matches the declared slot (so `real x = 5` — which
///   stores `5.0` — is *not* propagated as the integer `5`),
/// - never assigned, incremented/decremented, `@`-referenced, passed as a call
///   argument (which could bind to a by-reference parameter), or captured by a
///   lambda.
///
/// Because the value is provably constant for the binding's whole lifetime, the
/// substitution is exact regardless of LeekScript's dynamic typing — unlike
/// algebraic identities, it makes no assumption about inferred types.
pub fn propagate_const_locals(hir: &mut HirFile) -> usize {
    let mut total = 0;
    for def in &mut hir.defs {
        match def {
            Def::Function(f) => {
                if let Some(b) = &mut f.body {
                    total += propagate_in_body(&mut b.stmts);
                }
            }
            Def::Class(c) => {
                for m in c.methods.iter_mut().chain(c.constructors.iter_mut()) {
                    if let Some(b) = &mut m.body {
                        total += propagate_in_body(&mut b.stmts);
                    }
                }
            }
            Def::Global(_) | Def::Local(_) => {}
        }
    }
    total += propagate_in_body(&mut hir.main);
    total
}

/// Run the analysis + substitution over one function body / the main block.
fn propagate_in_body(stmts: &mut Vec<Stmt>) -> usize {
    let mut candidates: HashMap<DefId, Literal> = HashMap::new();
    collect_const_decls(stmts, &mut candidates);
    if candidates.is_empty() {
        return 0;
    }
    let mut disqualified: HashSet<DefId> = HashSet::new();
    for s in stmts.iter() {
        analyze_stmt(s, &mut disqualified);
    }
    candidates.retain(|d, _| !disqualified.contains(d));
    if candidates.is_empty() {
        return 0;
    }
    let mut count = 0;
    for s in stmts.iter_mut() {
        replace_in_stmt(s, &candidates, &mut count);
    }
    drop_dead_decls(stmts, &candidates);
    count
}

/// Whether `lit` is stored without coercion in a slot declared `ty`. A `None`
/// or `Any` declaration stores the literal as-is; an explicit primitive type
/// must match the literal's kind exactly (notably `real x = 5` coerces, so the
/// integer literal does *not* match).
fn literal_matches_decl(lit: &Literal, ty: &Option<Type>) -> bool {
    match ty {
        None | Some(Type::Any) => true,
        Some(Type::Integer) => matches!(lit, Literal::Int(_)),
        Some(Type::Real) => matches!(lit, Literal::Real(_)),
        Some(Type::String) => matches!(lit, Literal::String(_)),
        Some(Type::Boolean) => matches!(lit, Literal::Bool(_)),
        _ => false,
    }
}

/// Collect `var x = <literal>` declarations (recursively) whose literal is
/// stored without coercion.
fn collect_const_decls(stmts: &[Stmt], map: &mut HashMap<DefId, Literal>) {
    for s in stmts {
        if let Stmt::VarDecl(v) = s
            && !v.is_global
            && let Some(init) = &v.init
            && let ExprKind::Literal(lit) = &init.kind
            && literal_matches_decl(lit, &v.ty)
        {
            map.insert(v.def, lit.clone());
        }
        walk_stmt_child_stmts(s, &mut |c| collect_const_decls(std::slice::from_ref(c), map));
    }
}

/// Mark every local that is written, address-taken, passed to a call, or
/// captured by a lambda — i.e. anything that could make it non-constant.
fn analyze_stmt(s: &Stmt, disq: &mut HashSet<DefId>) {
    walk_stmt_child_exprs(s, &mut |e| analyze_expr(e, disq));
    walk_stmt_child_stmts(s, &mut |c| analyze_stmt(c, disq));
}

fn analyze_expr(e: &Expr, disq: &mut HashSet<DefId>) {
    match &e.kind {
        // Assignment / compound assignment: the whole left-hand side is a write
        // target. Disqualify every local mentioned there (covers `x = …`,
        // `x += …`, and any destructuring / complex lvalue).
        ExprKind::Binary(op, lhs, _) if op.is_assignment() => {
            collect_local_reads(lhs, disq);
        }
        // In-place mutation and address-of.
        ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::Ref, x) => {
            collect_local_reads(x, disq);
        }
        ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, x) => {
            collect_local_reads(x, disq);
        }
        // A local passed *directly* as an argument may bind to a by-reference
        // parameter and be mutated by the callee. (A sub-expression like
        // `f(x + 1)` passes a fresh value, so it doesn't count.)
        ExprKind::Call(c) => {
            for arg in &c.args {
                if let ExprKind::Name(NameRef::Local(d)) = &arg.kind {
                    disq.insert(*d);
                }
            }
        }
        ExprKind::New(n) => {
            for arg in &n.args {
                if let ExprKind::Name(NameRef::Local(d)) = &arg.kind {
                    disq.insert(*d);
                }
            }
        }
        // A captured local shares storage with the closure, which may write it.
        // Conservatively disqualify everything the lambda references and don't
        // optimize inside it.
        ExprKind::Lambda(_) => {
            collect_local_reads(e, disq);
            return;
        }
        _ => {}
    }
    walk_expr_children(e, &mut |c| analyze_expr(c, disq));
}

/// Add every `Local` referenced in `e` (including inside a lambda) to `set`.
fn collect_local_reads(e: &Expr, set: &mut HashSet<DefId>) {
    if let ExprKind::Name(NameRef::Local(d)) = &e.kind {
        set.insert(*d);
    }
    if let ExprKind::Lambda(lam) = &e.kind {
        for p in &lam.params {
            if let Some(default) = &p.default {
                collect_local_reads(default, set);
            }
        }
        match &lam.body {
            LambdaBody::Block(b) => {
                for st in &b.stmts {
                    walk_stmt_child_exprs(st, &mut |x| collect_local_reads(x, set));
                    walk_stmt_child_stmts(st, &mut |c| {
                        walk_stmt_child_exprs(c, &mut |x| collect_local_reads(x, set));
                    });
                }
            }
            LambdaBody::Expr(x) => collect_local_reads(x, set),
        }
    } else {
        walk_expr_children(e, &mut |c| collect_local_reads(c, set));
    }
}

fn replace_in_stmt(s: &mut Stmt, map: &HashMap<DefId, Literal>, count: &mut usize) {
    walk_stmt_child_exprs_mut(s, &mut |e| replace_in_expr(e, map, count));
    walk_stmt_child_stmts_mut(s, &mut |c| replace_in_stmt(c, map, count));
}

fn replace_in_expr(e: &mut Expr, map: &HashMap<DefId, Literal>, count: &mut usize) {
    if let ExprKind::Name(NameRef::Local(d)) = &e.kind
        && let Some(lit) = map.get(d)
    {
        e.kind = ExprKind::Literal(lit.clone());
        *count += 1;
        return;
    }
    if let ExprKind::Lambda(lam) = &mut e.kind {
        for p in &mut lam.params {
            if let Some(default) = &mut p.default {
                replace_in_expr(default, map, count);
            }
        }
        match &mut lam.body {
            LambdaBody::Block(b) => {
                for st in &mut b.stmts {
                    replace_in_stmt(st, map, count);
                }
            }
            LambdaBody::Expr(x) => replace_in_expr(x, map, count),
        }
    } else {
        walk_expr_children_mut(e, &mut |c| replace_in_expr(c, map, count));
    }
}

/// Remove `var x = <literal>` statements whose local was fully propagated (so
/// it now has no reads). Recurses into nested statement lists.
fn drop_dead_decls(stmts: &mut Vec<Stmt>, dead: &HashMap<DefId, Literal>) {
    stmts.retain(|s| !matches!(s, Stmt::VarDecl(v) if dead.contains_key(&v.def)));
    for s in stmts.iter_mut() {
        match s {
            Stmt::If(i) => {
                drop_dead_in_boxed(&mut i.then_branch, dead);
                if let Some(e) = &mut i.else_branch {
                    drop_dead_in_boxed(e, dead);
                }
            }
            Stmt::While(w) => drop_dead_in_boxed(&mut w.body, dead),
            Stmt::DoWhile(d) => drop_dead_in_boxed(&mut d.body, dead),
            Stmt::For(f) => drop_dead_in_boxed(&mut f.body, dead),
            Stmt::Foreach(fe) => drop_dead_in_boxed(&mut fe.body, dead),
            Stmt::Block(b) => drop_dead_decls(&mut b.stmts, dead),
            Stmt::Switch(sw) => {
                for arm in &mut sw.arms {
                    drop_dead_decls(&mut arm.body, dead);
                }
            }
            _ => {}
        }
    }
}

fn drop_dead_in_boxed(s: &mut Box<Stmt>, dead: &HashMap<DefId, Literal>) {
    if let Stmt::Block(b) = s.as_mut() {
        drop_dead_decls(&mut b.stmts, dead);
    }
}

/// Remove statements proven dead: constant-condition `if` / `while` branches
/// and pure expression-statements whose value is discarded. Returns the number
/// of statements eliminated.
///
/// Runs only at `-O1`, so the analysis paths (`check`, `lint`, the LSP) still
/// see — and can report on — the code as written; the optimized build simply
/// doesn't emit what it proved unreachable. Folds:
/// - `if (true) { A } else { B }` → `A`; `if (false) …` → the `else` (or
///   nothing). Only a *boolean* literal condition qualifies.
/// - `while (false) { … }` → removed; `do { B } while (false)` → `B` (runs once).
/// - a bare expression-statement that is a literal or a plain variable read
///   (no call, assignment, or increment) → removed.
///
/// Constant conditions reach here after propagation + folding (`var DEBUG =
/// false; if (DEBUG) …`). For the interpreter / native backends the equivalent
/// branch is also dropped in the MIR control-flow pass; doing it on HIR lets the
/// Java backend (which lowers HIR directly) benefit too, and trims the static
/// per-statement charge.
pub fn eliminate_dead_statements(hir: &mut HirFile) -> usize {
    let mut count = 0;
    for def in &mut hir.defs {
        match def {
            Def::Function(f) => {
                if let Some(b) = &mut f.body {
                    eliminate_in_stmts(&mut b.stmts, &mut count);
                }
            }
            Def::Class(c) => {
                for m in c.methods.iter_mut().chain(c.constructors.iter_mut()) {
                    if let Some(b) = &mut m.body {
                        eliminate_in_stmts(&mut b.stmts, &mut count);
                    }
                }
            }
            Def::Global(_) | Def::Local(_) => {}
        }
    }
    eliminate_in_stmts(&mut hir.main, &mut count);
    count
}

/// The boolean value of a constant condition, or `None` if it isn't a `bool`
/// literal (other types would need version-dependent truthiness coercion).
fn const_bool(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Literal(Literal::Bool(b)) => Some(*b),
        _ => None,
    }
}

/// A bare expression-statement safe to drop when its value is unused: a literal
/// or a plain resolved variable read (no call, assignment, index, or field
/// access that could have a side effect or raise in strict mode).
fn is_pure_discardable(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Literal(_) | ExprKind::Name(NameRef::Local(_) | NameRef::Global(_) | NameRef::This)
    )
}

fn eliminate_in_stmts(stmts: &mut Vec<Stmt>, count: &mut usize) {
    // Clean nested statement lists first (post-order), so a dead branch we
    // splice in is already simplified.
    for s in stmts.iter_mut() {
        eliminate_in_children(s, count);
    }
    let old = std::mem::take(stmts);
    let last_idx = old.len().wrapping_sub(1);
    for (idx, s) in old.into_iter().enumerate() {
        // A function body's (and the top-level program's) trailing expression
        // statement is its *implicit return value* (`return? null 5` lowers to
        // `if (null) return null` then `5`, and the program yields 5). So a
        // pure expression statement is only dead when it is NOT the last
        // statement — dropping the last one would change the result to null.
        let is_last = idx == last_idx;
        // Folding a constant-condition `if`/loop to its taken branch is unsound
        // at the TRAILING statement when the exposed branch is a bare expression
        // statement: the last statement is the implicit return, and an `if`/loop
        // *statement* yields `null`, whereas a bare expression yields its value.
        // `if (true) count++` returns `null`, not the pre-increment count. (A
        // block / other statement branch yields `null` too, so folding to those
        // stays sound — matching the `Stmt::Expr` arm's existing `is_last`
        // guard.) Dropping a dead trailing `if (false)`/`while (false)` is also
        // unsound (it would expose the *previous* statement), so keep those too.
        match s {
            Stmt::If(i) => match const_bool(&i.cond) {
                Some(true) if !(is_last && matches!(*i.then_branch, Stmt::Expr(_))) => {
                    *count += 1;
                    stmts.push(*i.then_branch);
                }
                Some(false)
                    if !(is_last && matches!(i.else_branch.as_deref(), Some(Stmt::Expr(_)) | None)) =>
                {
                    *count += 1;
                    if let Some(e) = i.else_branch {
                        stmts.push(*e);
                    }
                }
                _ => stmts.push(Stmt::If(i)),
            },
            Stmt::While(w) if !is_last && const_bool(&w.cond) == Some(false) => *count += 1,
            Stmt::DoWhile(d)
                if const_bool(&d.cond) == Some(false)
                    && !(is_last && matches!(*d.body, Stmt::Expr(_))) =>
            {
                *count += 1;
                stmts.push(*d.body);
            }
            Stmt::Expr(e) if is_pure_discardable(&e) && !is_last => *count += 1,
            other => stmts.push(other),
        }
    }
}

/// Recurse into a statement's nested statement lists / bodies. Constant-`if`
/// elimination itself happens at the [`Vec`] level in [`eliminate_in_stmts`];
/// braced bodies are `Block`s, so they route back through there.
fn eliminate_in_children(s: &mut Stmt, count: &mut usize) {
    match s {
        Stmt::If(i) => {
            eliminate_in_children(i.then_branch.as_mut(), count);
            if let Some(e) = &mut i.else_branch {
                eliminate_in_children(e.as_mut(), count);
            }
        }
        Stmt::While(w) => eliminate_in_children(w.body.as_mut(), count),
        Stmt::DoWhile(d) => eliminate_in_children(d.body.as_mut(), count),
        Stmt::For(f) => eliminate_in_children(f.body.as_mut(), count),
        Stmt::Foreach(fe) => eliminate_in_children(fe.body.as_mut(), count),
        Stmt::Block(b) => eliminate_in_stmts(&mut b.stmts, count),
        Stmt::Switch(sw) => {
            for arm in &mut sw.arms {
                eliminate_in_stmts(&mut arm.body, count);
            }
        }
        _ => {}
    }
}

/// Substitute immutable file-level `global G = <literal>` constants at their
/// uses across the whole file, then drop their initializers. Returns the number
/// of reads rewritten.
///
/// The whole-file analogue of [`propagate_const_locals`]. A global's
/// initializer lives in a `VarDecl { is_global: true }` in `main`; reads from
/// any function, method, field initializer, or `main` resolve to the same
/// [`NameRef::Global`]. A global qualifies only when:
/// - it has exactly one initializer and that initializer is a type-matched
///   literal (so a coercing slot like `global real G = 5` is left alone),
/// - it is never assigned, incremented/decremented, `@`-referenced, or passed
///   as a call argument anywhere — including inside lambda bodies (globals are
///   accessed directly, not captured, so a lambda that only *reads* one is
///   fine; one that *writes* it disqualifies).
pub fn propagate_const_globals(hir: &mut HirFile) -> usize {
    // 1. Candidate globals from their initializers in `main`. A global with
    //    more than one initializer isn't a simple constant.
    let mut candidates: HashMap<DefId, Literal> = HashMap::new();
    let mut seen_init: HashSet<DefId> = HashSet::new();
    for s in &hir.main {
        if let Stmt::VarDecl(v) = s
            && v.is_global
        {
            if !seen_init.insert(v.def) {
                candidates.remove(&v.def);
                continue;
            }
            if let Some(init) = &v.init
                && let ExprKind::Literal(lit) = &init.kind
                && literal_matches_decl(lit, &v.ty)
            {
                candidates.insert(v.def, lit.clone());
            }
        }
    }
    if candidates.is_empty() {
        return 0;
    }

    // 2. Disqualify any global written / address-taken / passed to a call,
    //    scanning every expression in the file (lambda bodies included).
    let mut disq: HashSet<DefId> = HashSet::new();
    for_each_file_expr(hir, &mut |e| analyze_global_expr(e, &mut disq));
    candidates.retain(|d, _| !disq.contains(d));
    if candidates.is_empty() {
        return 0;
    }

    // 3. Replace reads everywhere, then 4. drop the now-dead initializers.
    let mut count = 0;
    for_each_file_expr_mut(hir, &mut |e| {
        if let ExprKind::Name(NameRef::Global(d)) = &e.kind
            && let Some(lit) = candidates.get(d)
        {
            e.kind = ExprKind::Literal(lit.clone());
            count += 1;
        }
    });
    hir.main
        .retain(|s| !matches!(s, Stmt::VarDecl(v) if v.is_global && candidates.contains_key(&v.def)));
    count
}

fn analyze_global_expr(e: &Expr, disq: &mut HashSet<DefId>) {
    match &e.kind {
        ExprKind::Binary(op, lhs, _) if op.is_assignment() => collect_global_reads(lhs, disq),
        ExprKind::Unary(UnaryOp::PreInc | UnaryOp::PreDec | UnaryOp::Ref, x) => {
            collect_global_reads(x, disq);
        }
        ExprKind::Postfix(PostfixOp::PostInc | PostfixOp::PostDec, x) => {
            collect_global_reads(x, disq);
        }
        ExprKind::Call(c) => {
            for arg in &c.args {
                if let ExprKind::Name(NameRef::Global(d)) = &arg.kind {
                    disq.insert(*d);
                }
            }
        }
        ExprKind::New(n) => {
            for arg in &n.args {
                if let ExprKind::Name(NameRef::Global(d)) = &arg.kind {
                    disq.insert(*d);
                }
            }
        }
        _ => {}
    }
}

fn collect_global_reads(e: &Expr, set: &mut HashSet<DefId>) {
    visit_expr_all(e, &mut |x| {
        if let ExprKind::Name(NameRef::Global(d)) = &x.kind {
            set.insert(*d);
        }
    });
}

// ---- whole-file expression traversal (descends into lambdas) ----

fn visit_expr_all(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    if let ExprKind::Lambda(lam) = &e.kind {
        for p in &lam.params {
            if let Some(d) = &p.default {
                visit_expr_all(d, f);
            }
        }
        match &lam.body {
            LambdaBody::Block(b) => {
                for s in &b.stmts {
                    visit_stmt_all_exprs(s, f);
                }
            }
            LambdaBody::Expr(x) => visit_expr_all(x, f),
        }
    } else {
        walk_expr_children(e, &mut |c| visit_expr_all(c, f));
    }
}

fn visit_stmt_all_exprs(s: &Stmt, f: &mut impl FnMut(&Expr)) {
    walk_stmt_child_exprs(s, &mut |e| visit_expr_all(e, f));
    walk_stmt_child_stmts(s, &mut |c| visit_stmt_all_exprs(c, f));
}

fn visit_expr_all_mut(e: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    f(e);
    if let ExprKind::Lambda(lam) = &mut e.kind {
        for p in &mut lam.params {
            if let Some(d) = &mut p.default {
                visit_expr_all_mut(d, f);
            }
        }
        match &mut lam.body {
            LambdaBody::Block(b) => {
                for s in &mut b.stmts {
                    visit_stmt_all_exprs_mut(s, f);
                }
            }
            LambdaBody::Expr(x) => visit_expr_all_mut(x, f),
        }
    } else {
        walk_expr_children_mut(e, &mut |c| visit_expr_all_mut(c, f));
    }
}

fn visit_stmt_all_exprs_mut(s: &mut Stmt, f: &mut impl FnMut(&mut Expr)) {
    walk_stmt_child_exprs_mut(s, &mut |e| visit_expr_all_mut(e, f));
    walk_stmt_child_stmts_mut(s, &mut |c| visit_stmt_all_exprs_mut(c, f));
}

/// Run `f` on every expression in the file: function and method bodies, their
/// parameter defaults, class field initializers, and the main block.
fn for_each_file_expr(hir: &HirFile, f: &mut impl FnMut(&Expr)) {
    for def in &hir.defs {
        match def {
            Def::Function(fun) => {
                for p in &fun.params {
                    if let Some(d) = &p.default {
                        visit_expr_all(d, f);
                    }
                }
                if let Some(b) = &fun.body {
                    for s in &b.stmts {
                        visit_stmt_all_exprs(s, f);
                    }
                }
            }
            Def::Class(c) => {
                for field in &c.fields {
                    if let Some(e) = &field.init {
                        visit_expr_all(e, f);
                    }
                }
                for m in c.methods.iter().chain(&c.constructors) {
                    for p in &m.params {
                        if let Some(d) = &p.default {
                            visit_expr_all(d, f);
                        }
                    }
                    if let Some(b) = &m.body {
                        for s in &b.stmts {
                            visit_stmt_all_exprs(s, f);
                        }
                    }
                }
            }
            Def::Global(_) | Def::Local(_) => {}
        }
    }
    for s in &hir.main {
        visit_stmt_all_exprs(s, f);
    }
}

/// Inline calls to small, side-effect-safe free functions at their call sites.
///
/// A function qualifies when its body is a single `return <expr>` (no locals,
/// branches, or statements), it isn't recursive, its parameters are plain
/// (no `@`-by-reference, no defaults), and the return expression has no lambda.
/// A *call* is inlined only when every argument is **trivial** — a literal or a
/// plain variable read — so substituting a parameter (possibly used several
/// times) can't duplicate work, lose a side effect, or reorder evaluation.
///
/// This composes with constant propagation/folding: once an argument is folded
/// to a literal, `double(2 + 3)` → `double(5)` → `5 * 2` → `10`, and the call
/// (a function-entry op + dispatch) disappears entirely.
pub fn inline_calls(hir: &mut HirFile) -> usize {
    let inlinable = collect_inlinable(hir);
    if inlinable.is_empty() {
        return 0;
    }
    let mut count = 0;
    for_each_file_expr_mut(hir, &mut |e| {
        // Pull out (params, body) only when this call is eligible, to avoid
        // borrowing across the in-place replacement.
        let subst: Option<(Vec<DefId>, Expr, Vec<Expr>)> = match &e.kind {
            ExprKind::Call(call) => match &call.callee {
                Callee::Function(NameRef::Function(d)) => inlinable.get(d).and_then(|(ps, ret)| {
                    (ps.len() == call.args.len() && call.args.iter().all(is_trivial_arg))
                        .then(|| (ps.clone(), ret.clone(), call.args.clone()))
                }),
                _ => None,
            },
            _ => None,
        };
        if let Some((params, mut body, args)) = subst {
            let map: HashMap<DefId, Expr> = params.into_iter().zip(args).collect();
            substitute_params(&mut body, &map);
            *e = body;
            count += 1;
        }
    });
    count
}

/// Free functions whose body is a single `return <expr>`, keyed by `DefId`, with
/// their parameter `DefId`s and a clone of the return expression.
fn collect_inlinable(hir: &HirFile) -> HashMap<DefId, (Vec<DefId>, Expr)> {
    let mut map = HashMap::new();
    for (i, def) in hir.defs.iter().enumerate() {
        let Def::Function(f) = def else { continue };
        let Some(body) = &f.body else { continue };
        if body.stmts.len() != 1 {
            continue;
        }
        let Stmt::Return(Some(ret)) = &body.stmts[0] else {
            continue;
        };
        if f.params.iter().any(|p| p.is_by_ref || p.default.is_some()) {
            continue;
        }
        // A declared parameter or return type COERCES the value as it crosses
        // the call boundary (`function f(real r) { return r } f(12)` yields
        // `12.0`, and `=> integer` truncates). Inlining substitutes the raw
        // argument / body, bypassing that coercion, so only inline functions
        // whose params and return type impose no value coercion.
        if f.params.iter().any(|p| p.ty.as_ref().is_some_and(type_coerces))
            || f.return_type.as_ref().is_some_and(type_coerces)
        {
            continue;
        }
        let self_id = DefId(u32::try_from(i).unwrap_or(u32::MAX));
        // No direct self-recursion, and no lambda (capturing a substituted param
        // into a closure is out of this pass's scope).
        if expr_calls_fn(ret, self_id) || expr_has_lambda(ret) {
            continue;
        }
        let param_ids: Vec<DefId> = f.params.iter().map(|p| p.def).collect();
        map.insert(self_id, (param_ids, ret.clone()));
    }
    map
}

/// True when binding a value to a slot of this declared type coerces it (the
/// numeric/bool/nullable conversions `coerce_to_type` performs). Inlining must
/// not be applied across such a type, or the coercion is silently dropped.
fn type_coerces(t: &Type) -> bool {
    matches!(
        t,
        Type::Real | Type::Integer | Type::Boolean | Type::Nullable(_)
    )
}

/// A trivial argument: a literal or a plain resolved variable read. Re-evaluating
/// one is free and side-effect-free, so a parameter bound to it can be inlined
/// even when used multiple times.
fn is_trivial_arg(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Literal(_)
            | ExprKind::Name(
                NameRef::Local(_) | NameRef::Global(_) | NameRef::This | NameRef::Builtin(_)
            )
    )
}

/// Replace each `Name(Local(d))` for `d` in `map` with the bound argument.
fn substitute_params(e: &mut Expr, map: &HashMap<DefId, Expr>) {
    if let ExprKind::Name(NameRef::Local(d)) = &e.kind {
        if let Some(arg) = map.get(d) {
            *e = arg.clone();
            return;
        }
    }
    walk_expr_children_mut(e, &mut |c| substitute_params(c, map));
}

/// Whether `e` contains a direct call to the function `target`.
fn expr_calls_fn(e: &Expr, target: DefId) -> bool {
    let mut found = false;
    visit_expr_all(e, &mut |x| {
        if let ExprKind::Call(c) = &x.kind {
            if matches!(&c.callee, Callee::Function(NameRef::Function(d)) if *d == target) {
                found = true;
            }
        }
    });
    found
}

/// Whether `e` contains a lambda anywhere.
fn expr_has_lambda(e: &Expr) -> bool {
    let mut found = false;
    visit_expr_all(e, &mut |x| {
        if matches!(x.kind, ExprKind::Lambda(_)) {
            found = true;
        }
    });
    found
}

fn for_each_file_expr_mut(hir: &mut HirFile, f: &mut impl FnMut(&mut Expr)) {
    for def in &mut hir.defs {
        match def {
            Def::Function(fun) => {
                for p in &mut fun.params {
                    if let Some(d) = &mut p.default {
                        visit_expr_all_mut(d, f);
                    }
                }
                if let Some(b) = &mut fun.body {
                    for s in &mut b.stmts {
                        visit_stmt_all_exprs_mut(s, f);
                    }
                }
            }
            Def::Class(c) => {
                for field in &mut c.fields {
                    if let Some(e) = &mut field.init {
                        visit_expr_all_mut(e, f);
                    }
                }
                for m in c.methods.iter_mut().chain(c.constructors.iter_mut()) {
                    for p in &mut m.params {
                        if let Some(d) = &mut p.default {
                            visit_expr_all_mut(d, f);
                        }
                    }
                    if let Some(b) = &mut m.body {
                        for s in &mut b.stmts {
                            visit_stmt_all_exprs_mut(s, f);
                        }
                    }
                }
            }
            Def::Global(_) | Def::Local(_) => {}
        }
    }
    for s in &mut hir.main {
        visit_stmt_all_exprs_mut(s, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Callee, Stmt};
    use crate::ir::Literal::{Bool, Int, Real};
    use leek_span::Span;

    fn span() -> Span {
        Span::synthetic()
    }

    fn name(n: &str) -> Expr {
        Expr {
            kind: ExprKind::Name(NameRef::Builtin(n.to_string())),
            ty: leek_types::Type::Any,
            span: span(),
        }
    }

    #[test]
    fn folds_builtin_name_in_nested_expression() {
        let mut values = HashMap::new();
        values.insert("WEAPON_PISTOL".to_string(), Literal::Int(37));
        // `setWeapon(WEAPON_PISTOL)` in the main block.
        let mut hir = HirFile {
            defs: Vec::new(),
            items: Vec::new(),
            main: vec![Stmt::Expr(Expr {
                kind: ExprKind::Call(Box::new(crate::ir::Call {
                    callee: Callee::Function(NameRef::Builtin("setWeapon".into())),
                    args: vec![name("WEAPON_PISTOL")],
                    span: span(),
                })),
                ty: leek_types::Type::Any,
                span: span(),
            })],
        };
        let n = fold_constants(&mut hir, &values);
        assert_eq!(n, 1);
        let Stmt::Expr(Expr {
            kind: ExprKind::Call(c),
            ..
        }) = &hir.main[0]
        else {
            panic!()
        };
        assert_eq!(c.args[0].kind, ExprKind::Literal(Literal::Int(37)));
    }

    #[test]
    fn leaves_unknown_names_and_real_bindings_untouched() {
        let values = HashMap::new(); // empty → no-op fast path
        let mut hir = HirFile {
            defs: Vec::new(),
            items: Vec::new(),
            main: vec![Stmt::Expr(name("SOMETHING"))],
        };
        assert_eq!(fold_constants(&mut hir, &values), 0);
        assert_eq!(hir.main[0], Stmt::Expr(name("SOMETHING")));
    }

    // ----- fold_expressions -----

    fn lower(src: &str) -> HirFile {
        use leek_parser::ast::{AstNode, SourceFile};
        use leek_span::SourceId;
        use leek_syntax::{SyntaxNode, Version};
        let source = SourceId::new(1).unwrap();
        let parsed = leek_parser::parse(src, source, Version::V4);
        let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parses");
        crate::lower_file(&file, source).0
    }

    /// Lower `src`, fold, and return the single VarDecl initializer's kind.
    fn fold_init(src: &str) -> ExprKind {
        let mut hir = lower(src);
        fold_expressions(&mut hir);
        match &hir.main[0] {
            Stmt::VarDecl(v) => v.init.as_ref().expect("has init").kind.clone(),
            other => panic!("expected a var decl, got {other:?}"),
        }
    }

    #[test]
    fn folds_integer_arithmetic_recursively() {
        // 1 + 2 * 3  →  7  (children fold first, then the outer add)
        assert_eq!(fold_init("var x = 1 + 2 * 3\n"), ExprKind::Literal(Int(7)));
    }

    #[test]
    fn folds_real_promotion_and_string_concat() {
        assert_eq!(fold_init("var x = 1 + 2.0\n"), ExprKind::Literal(Real(3.0)));
        assert_eq!(
            fold_init("var x = \"a\" + \"b\"\n"),
            ExprKind::Literal(Literal::String("ab".into()))
        );
    }

    #[test]
    fn folds_comparisons_and_boolean_logic() {
        assert_eq!(fold_init("var x = 2 < 3\n"), ExprKind::Literal(Bool(true)));
        assert_eq!(
            fold_init("var x = 1 == 2\n"),
            ExprKind::Literal(Bool(false))
        );
        assert_eq!(
            fold_init("var x = true && false\n"),
            ExprKind::Literal(Bool(false))
        );
    }

    #[test]
    fn folds_unary_ops() {
        assert_eq!(fold_init("var x = -5\n"), ExprKind::Literal(Int(-5)));
        assert_eq!(fold_init("var x = !true\n"), ExprKind::Literal(Bool(false)));
        assert_eq!(fold_init("var x = ~0\n"), ExprKind::Literal(Int(-1)));
    }

    #[test]
    fn collapses_constant_ternary_to_taken_branch() {
        // true ? 1 : 2  →  1
        assert_eq!(fold_init("var x = true ? 1 : 2\n"), ExprKind::Literal(Int(1)));
        // false ? 1 : 2  →  2
        assert_eq!(
            fold_init("var x = false ? 1 : 2\n"),
            ExprKind::Literal(Int(2))
        );
    }

    #[test]
    fn integer_overflow_wraps_like_runtime() {
        // i64::MAX + 1 wraps to i64::MIN, matching the interpreter.
        let folded = fold_init("var x = 9223372036854775807 + 1\n");
        assert_eq!(folded, ExprKind::Literal(Int(i64::MIN)));
    }

    #[test]
    fn leaves_version_sensitive_ops_untouched() {
        // Division is real-promoting / divide-by-zero sensitive: not folded.
        assert!(matches!(
            fold_init("var x = 6 / 2\n"),
            ExprKind::Binary(BinaryOp::Div, _, _)
        ));
        // Mixed string/number equality has coercion rules: not folded.
        assert!(matches!(
            fold_init("var x = \"1\" == 1\n"),
            ExprKind::Binary(BinaryOp::Eq, _, _)
        ));
    }

    #[test]
    fn does_not_fold_expressions_with_non_literal_operands() {
        let mut hir = lower("var y = 0\nvar x = y + 1\n");
        // Only `var y = 0` and `var x = y + 1`; `y + 1` has a variable operand.
        let n = fold_expressions(&mut hir);
        assert_eq!(n, 0, "no constant subexpression to fold");
    }

    /// Folding collapses a constant expression tree to a single literal, which
    /// is exactly what shrinks the program's op budget: the `leek-charge` pass
    /// charges per expression node, so fewer nodes means fewer ops.
    // ----- propagate_const_locals -----

    /// Lower, propagate, fold, and return the main block's statements.
    fn prop_and_fold(src: &str) -> Vec<Stmt> {
        let mut hir = lower(src);
        propagate_const_locals(&mut hir);
        fold_expressions(&mut hir);
        hir.main
    }

    #[test]
    fn propagates_const_local_and_drops_decl() {
        // `var W = 5; var y = W * 2` → `W` propagated, its decl dropped,
        // `5 * 2` folded to `10`.
        let main = prop_and_fold("var W = 5\nvar y = W * 2\n");
        // The `var W = 5` decl is gone; only `var y = 10` remains.
        assert_eq!(main.len(), 1, "const decl dropped");
        let Stmt::VarDecl(v) = &main[0] else {
            panic!("expected var y")
        };
        assert_eq!(v.name, "y");
        assert_eq!(
            v.init.as_ref().unwrap().kind,
            ExprKind::Literal(Int(10)),
            "W*2 folded to 10 after propagation"
        );
    }

    #[test]
    fn reassigned_local_is_not_propagated() {
        // `x` is reassigned, so it is not constant — leave it alone.
        let mut hir = lower("var x = 5\nx = 6\nvar y = x\n");
        let n = propagate_const_locals(&mut hir);
        assert_eq!(n, 0, "reassigned local must not be propagated");
        // All three statements remain.
        assert_eq!(hir.main.len(), 3);
    }

    #[test]
    fn incremented_local_is_not_propagated() {
        let mut hir = lower("var i = 0\ni++\nvar y = i\n");
        assert_eq!(propagate_const_locals(&mut hir), 0);
    }

    #[test]
    fn local_passed_to_call_is_not_propagated() {
        // Could bind to a by-reference parameter, so be conservative.
        let mut hir = lower("var x = 5\ndebug(x)\n");
        assert_eq!(propagate_const_locals(&mut hir), 0);
    }

    #[test]
    fn coercing_declaration_is_not_propagated() {
        // `real x = 5` stores 5.0; propagating the integer literal 5 would
        // change the value's type, so it must be left alone.
        let mut hir = lower("real x = 5\nvar y = x\n");
        assert_eq!(
            propagate_const_locals(&mut hir),
            0,
            "int literal in a real slot coerces — do not propagate"
        );
    }

    #[test]
    fn const_flag_enables_branch_to_fold() {
        // `var DEBUG = false; ...` propagates so a later `DEBUG ? a : b` folds.
        let main = prop_and_fold("var DEBUG = false\nvar y = DEBUG ? 1 : 2\n");
        assert_eq!(main.len(), 1, "DEBUG decl dropped");
        let Stmt::VarDecl(v) = &main[0] else {
            panic!()
        };
        assert_eq!(v.init.as_ref().unwrap().kind, ExprKind::Literal(Int(2)));
    }

    // ----- propagate_const_globals -----

    #[test]
    fn propagates_const_global_into_function() {
        // `global MAX = 8` is read inside a function; propagate + fold so the
        // function body computes a literal, and the global's init is dropped.
        let mut hir = lower("global MAX = 8\nfunction f() { return MAX * 2 }\n");
        let n = propagate_const_globals(&mut hir);
        assert!(n >= 1, "global read replaced");
        fold_expressions(&mut hir);
        // The `global MAX = 8` initializer statement is gone from main.
        assert!(
            !hir.main
                .iter()
                .any(|s| matches!(s, Stmt::VarDecl(v) if v.is_global)),
            "global initializer dropped"
        );
        // f's body returns the folded literal 16.
        let Def::Function(f) = hir.defs.iter().find(|d| d.name() == "f").unwrap() else {
            panic!()
        };
        let body = f.body.as_ref().unwrap();
        let Stmt::Return(Some(e)) = &body.stmts[0] else {
            panic!("expected return")
        };
        assert_eq!(e.kind, ExprKind::Literal(Int(16)));
    }

    #[test]
    fn reassigned_global_is_not_propagated() {
        let mut hir = lower("global G = 1\nfunction f() { G = 2 }\nvar y = G\n");
        assert_eq!(
            propagate_const_globals(&mut hir),
            0,
            "a global written anywhere is not constant"
        );
    }

    #[test]
    fn global_written_inside_lambda_is_not_propagated() {
        // A lambda that writes the global disqualifies it (we scan lambda bodies).
        let mut hir = lower("global G = 1\nvar f = () => { G = 5 }\nvar y = G\n");
        assert_eq!(propagate_const_globals(&mut hir), 0);
    }

    // ----- dead-statement elimination -----

    #[test]
    fn drops_false_if_and_flattens_true_if() {
        let mut hir = lower("if (false) { a() }\nif (true) { b() }\n");
        eliminate_dead_statements(&mut hir);
        // `if (false)` gone; `if (true)` flattened to its block.
        assert_eq!(hir.main.len(), 1);
        assert!(matches!(hir.main[0], Stmt::Block(_)), "true-if flattened");
    }

    #[test]
    fn false_if_keeps_else_branch() {
        let mut hir = lower("if (false) { a() } else { b() }\n");
        eliminate_dead_statements(&mut hir);
        assert_eq!(hir.main.len(), 1);
        assert!(matches!(hir.main[0], Stmt::Block(_)), "else branch kept");
    }

    #[test]
    fn drops_while_false_and_unrolls_do_while_false() {
        let mut hir = lower("while (false) { a() }\ndo { b() } while (false)\n");
        eliminate_dead_statements(&mut hir);
        // while(false) removed; do/while(false) → its body (runs once).
        assert_eq!(hir.main.len(), 1, "only the do-body remains");
        assert!(matches!(hir.main[0], Stmt::Block(_)));
    }

    #[test]
    fn drops_pure_expression_statements_but_keeps_calls() {
        let mut hir = lower("5\nfoo()\n");
        eliminate_dead_statements(&mut hir);
        // The bare literal is dropped; the call (side-effecting) stays.
        assert_eq!(hir.main.len(), 1);
        assert!(matches!(hir.main[0], Stmt::Expr(_)));
    }

    #[test]
    fn const_flag_branch_eliminated_via_fixpoint() {
        // `global DEBUG = false; if (DEBUG) {...}` → propagate → fold → drop.
        let mut hir = lower("global DEBUG = false\nif (DEBUG) { crash() }\nreturn 1\n");
        optimize_hir(&mut hir);
        // No `if` left; the DEBUG init is dropped; only `return 1` remains.
        assert_eq!(hir.main.len(), 1);
        assert!(matches!(hir.main[0], Stmt::Return(_)));
    }

    // ----- optimize_hir fixpoint -----

    // ----- inlining -----

    /// Lower `src`, run the full optimization fixpoint, return main[0]'s
    /// `return` expression kind.
    fn opt_return(src: &str) -> ExprKind {
        let mut hir = lower(src);
        optimize_hir(&mut hir);
        match &hir.main[0] {
            Stmt::Return(Some(e)) => e.kind.clone(),
            other => panic!("expected a return, got {other:?}"),
        }
    }

    #[test]
    fn inlines_single_return_function_then_folds() {
        // dbl(5) → 5 * 2 → 10 (`double` is a reserved name)
        assert_eq!(
            opt_return("function dbl(x) { return x * 2 }\nreturn dbl(5)\n"),
            ExprKind::Literal(Int(10))
        );
        // multi-arg, all trivial
        assert_eq!(
            opt_return("function add(a, b) { return a + b }\nreturn add(3, 4)\n"),
            ExprKind::Literal(Int(7))
        );
    }

    #[test]
    fn inlines_with_variable_args() {
        // sq(a) with a variable arg (trivial) → a * a; the call is gone.
        let mut hir = lower("function sq(n) { return n * n }\nvar a = read()\nreturn sq(a)\n");
        inline_calls(&mut hir);
        let Stmt::Return(Some(e)) = &hir.main[1] else {
            panic!("expected return")
        };
        assert!(
            matches!(e.kind, ExprKind::Binary(BinaryOp::Mul, _, _)),
            "sq(a) inlined to a * a"
        );
    }

    #[test]
    fn does_not_inline_recursive_function() {
        let mut hir = lower(
            "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2) }\nreturn fib(5)\n",
        );
        assert_eq!(inline_calls(&mut hir), 0, "recursive function not inlined");
    }

    #[test]
    fn does_not_inline_non_trivial_argument() {
        // The argument is a compound expression (a call) — not trivial, so the
        // inlinable `id` is left alone (no duplication / reordering risk).
        let mut hir = lower("function id(x) { return x }\nreturn id(read() + 1)\n");
        assert_eq!(inline_calls(&mut hir), 0, "non-trivial arg not inlined");
    }

    #[test]
    fn fixpoint_resolves_chained_constants() {
        // A → B → C: a single linear pass would only resolve A into B; the
        // fixpoint keeps going until C is a literal too.
        let mut hir = lower("var A = 2\nvar B = A + 1\nvar C = B * 2\nreturn C\n");
        optimize_hir(&mut hir);
        // A, B, C all propagated + dropped; only `return 6` remains.
        assert_eq!(hir.main.len(), 1, "all const decls dropped");
        let Stmt::Return(Some(e)) = &hir.main[0] else {
            panic!("expected return")
        };
        assert_eq!(e.kind, ExprKind::Literal(Int(6)));
    }

    #[test]
    fn fixpoint_terminates_without_constants() {
        // No constants to propagate — must converge immediately (0 changes).
        let mut hir = lower("var x = 0\nx = x + 1\nreturn x\n");
        assert_eq!(optimize_hir(&mut hir), 0);
    }

    // ----- pure-builtin folding -----

    #[test]
    fn folds_abs_min_max_on_constants() {
        assert_eq!(fold_init("var x = abs(-5)\n"), ExprKind::Literal(Int(5)));
        assert_eq!(fold_init("var x = min(5, 12)\n"), ExprKind::Literal(Int(5)));
        assert_eq!(fold_init("var x = max(5, 12)\n"), ExprKind::Literal(Int(12)));
    }

    #[test]
    fn folds_abs_real_and_min_promotes_to_real() {
        assert_eq!(fold_init("var x = abs(-2.5)\n"), ExprKind::Literal(Real(2.5)));
        // mixed int/real → real result
        assert_eq!(
            fold_init("var x = min(5, 2.5)\n"),
            ExprKind::Literal(Real(2.5))
        );
    }

    #[test]
    fn folds_floor_ceil_and_sqrt() {
        assert_eq!(fold_init("var x = floor(3.7)\n"), ExprKind::Literal(Int(3)));
        assert_eq!(fold_init("var x = ceil(3.2)\n"), ExprKind::Literal(Int(4)));
        assert_eq!(fold_init("var x = sqrt(4.0)\n"), ExprKind::Literal(Real(2.0)));
    }

    #[test]
    fn folds_builtin_call_with_nested_constant_arg() {
        // The argument folds first (post-order), then the call.
        assert_eq!(
            fold_init("var x = abs(2 - 7)\n"),
            ExprKind::Literal(Int(5))
        );
    }

    #[test]
    fn leaves_unsafe_or_nonconst_builtins_untouched() {
        // `round` is rounding-mode dependent — not folded.
        assert!(matches!(
            fold_init("var x = round(3.5)\n"),
            ExprKind::Call(_)
        ));
        // sqrt of a negative would be NaN — not folded.
        assert!(matches!(
            fold_init("var x = sqrt(-1.0)\n"),
            ExprKind::Call(_)
        ));
        // non-constant argument — not folded (fold alone doesn't propagate `y`).
        let mut hir = lower("var y = 3\nvar x = abs(y)\n");
        fold_expressions(&mut hir);
        let Stmt::VarDecl(v) = &hir.main[1] else {
            panic!("expected var x")
        };
        assert!(matches!(
            v.init.as_ref().unwrap().kind,
            ExprKind::Call(_)
        ));
    }

    #[test]
    fn folding_reduces_expression_node_count() {
        use crate::visit::walk_expr_children;
        fn count(e: &Expr) -> usize {
            let mut n = 1;
            walk_expr_children(e, &mut |c| n += count(c));
            n
        }
        let mut hir = lower("var x = 1 + 2 * 3 - 4\n");
        let before = match &hir.main[0] {
            Stmt::VarDecl(v) => count(v.init.as_ref().unwrap()),
            _ => unreachable!(),
        };
        fold_expressions(&mut hir);
        let after = match &hir.main[0] {
            Stmt::VarDecl(v) => count(v.init.as_ref().unwrap()),
            _ => unreachable!(),
        };
        assert_eq!(before, 7, "1, 2, 3, (2*3), 4, two binops's tree = 7 nodes");
        assert_eq!(after, 1, "folds to the single literal 3");
    }
}

