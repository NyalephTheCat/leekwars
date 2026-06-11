//! Shared MIR lowering helpers.

use leek_hir::{
    BinaryOp as HBinOp, DefId, Expr, ExprKind, Literal, NameRef, Stmt, Visibility as HirVisibility,
};
use leek_span::Span;
use leek_types::Type;

use crate::ir::{
    BasicBlock, BinOp, BlockId, Const, FunctionKind, MirFunction, Terminator, Visibility,
};

/// `true` when the source-level expression for an interval
/// endpoint should force the displayed interval into real format
/// — currently triggered by `Infinity`/`INFINITY` builtin
/// references (vs the `∞` symbol which leaves the formatting alone).
pub(crate) fn expr_forces_real(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Name(NameRef::Builtin(n)) => n == "Infinity" || n == "INFINITY",
        ExprKind::Unary(_, inner) => expr_forces_real(inner),
        _ => false,
    }
}

/// True when `def` is referenced anywhere inside `e`, descending through
/// nested lambda bodies (unlike [`captured_in_expr`], which treats a lambda
/// as a leaf until it finds one).
fn refs_def_deep(e: &Expr, def: DefId) -> bool {
    match &e.kind {
        ExprKind::Name(NameRef::Local(id)) if *id == def => true,
        ExprKind::Lambda(l) => match &l.body {
            leek_hir::LambdaBody::Expr(b) => refs_def_deep(b, def),
            leek_hir::LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt_refs_def_deep(s, def)),
        },
        _ => {
            // `walk_expr_children` doesn't surface a `Callee::Function`
            // name (it's a NameRef, not a child Expr) — check it here so
            // a captured first-class callable param (`a()`) is caught.
            if let ExprKind::Call(c) = &e.kind
                && matches!(&c.callee, leek_hir::Callee::Function(NameRef::Local(id)) if *id == def)
            {
                return true;
            }
            let mut found = false;
            leek_hir::walk_expr_children(e, &mut |c| found = found || refs_def_deep(c, def));
            found
        }
    }
}

fn stmt_refs_def_deep(s: &Stmt, def: DefId) -> bool {
    let mut found = false;
    leek_hir::walk_stmt_child_exprs(s, &mut |e| found = found || refs_def_deep(e, def));
    if !found {
        leek_hir::walk_stmt_child_stmts(s, &mut |c| found = found || stmt_refs_def_deep(c, def));
    }
    found
}

fn captured_in_expr(e: &Expr, def: DefId) -> bool {
    if let ExprKind::Lambda(l) = &e.kind {
        return match &l.body {
            leek_hir::LambdaBody::Expr(b) => refs_def_deep(b, def),
            leek_hir::LambdaBody::Block(b) => b.stmts.iter().any(|s| stmt_refs_def_deep(s, def)),
        };
    }
    let mut found = false;
    leek_hir::walk_expr_children(e, &mut |c| found = found || captured_in_expr(c, def));
    found
}

/// True when a lambda nested anywhere inside `stmts` references `def`.
/// Upstream binds any function/lambda parameter captured by a nested
/// closure through a runtime `Box` at the callee's entry; the 2-arg Box
/// ctor charges 1 op per call. Mirrors the Java backend's
/// `captured_by_nested_lambda_stmts` so the static charge matches.
pub(crate) fn captured_by_nested_lambda_stmts(stmts: &[Stmt], def: DefId) -> bool {
    fn walk(s: &Stmt, def: DefId) -> bool {
        let mut found = false;
        leek_hir::walk_stmt_child_exprs(s, &mut |e| found = found || captured_in_expr(e, def));
        if !found {
            leek_hir::walk_stmt_child_stmts(s, &mut |c| found = found || walk(c, def));
        }
        found
    }
    stmts.iter().any(|s| walk(s, def))
}

/// [`captured_by_nested_lambda_stmts`] for a lambda's own body — its
/// params get the same Box treatment when an inner lambda captures them
/// (`x -> y -> x + 1` boxes `x` at the outer lambda's entry).
pub(crate) fn captured_by_nested_lambda_body(body: &leek_hir::LambdaBody, def: DefId) -> bool {
    match body {
        leek_hir::LambdaBody::Expr(e) => captured_in_expr(e, def),
        leek_hir::LambdaBody::Block(b) => captured_by_nested_lambda_stmts(&b.stmts, def),
    }
}

// ---- Helpers ----

pub(crate) fn lit_to_const(lit: &Literal) -> Const {
    match lit {
        Literal::Int(i) => Const::Int(*i),
        Literal::Real(f) => Const::real(*f),
        Literal::BigInt(s) => Const::BigInt(s.clone()),
        Literal::String(s) => Const::String(s.clone()),
        Literal::Bool(b) => Const::Bool(*b),
        Literal::Null => Const::Null,
    }
}

pub(crate) fn hir_binop_to_mir(op: HBinOp) -> Option<BinOp> {
    Some(match op {
        HBinOp::Add => BinOp::Add,
        HBinOp::Sub => BinOp::Sub,
        HBinOp::Mul => BinOp::Mul,
        HBinOp::Div => BinOp::Div,
        HBinOp::Mod => BinOp::Mod,
        HBinOp::IntDiv => BinOp::IntDiv,
        HBinOp::Pow => BinOp::Pow,
        HBinOp::Eq => BinOp::Eq,
        HBinOp::Ne => BinOp::Ne,
        HBinOp::IdentityEq => BinOp::IdentityEq,
        HBinOp::IdentityNe => BinOp::IdentityNe,
        HBinOp::Lt => BinOp::Lt,
        HBinOp::Le => BinOp::Le,
        HBinOp::Gt => BinOp::Gt,
        HBinOp::Ge => BinOp::Ge,
        HBinOp::BitAnd => BinOp::BitAnd,
        HBinOp::BitOr => BinOp::BitOr,
        HBinOp::BitXor => BinOp::BitXor,
        HBinOp::Xor => BinOp::Xor,
        HBinOp::ShiftL => BinOp::ShiftL,
        HBinOp::ShiftR => BinOp::ShiftR,
        HBinOp::UShiftR => BinOp::UShiftR,
        HBinOp::In => BinOp::In,
        HBinOp::NotIn => BinOp::NotIn,
        HBinOp::Is => BinOp::Is,
        HBinOp::Instanceof => BinOp::Instanceof,
        // Short-circuit / assignment operators are filtered before
        // this helper is called.
        HBinOp::And
        | HBinOp::Or
        | HBinOp::NullCoalesce
        | HBinOp::Assign
        | HBinOp::AddAssign
        | HBinOp::SubAssign
        | HBinOp::MulAssign
        | HBinOp::DivAssign
        | HBinOp::IntDivAssign
        | HBinOp::ModAssign
        | HBinOp::PowAssign
        | HBinOp::BitAndAssign
        | HBinOp::BitOrAssign
        | HBinOp::BitXorAssign
        | HBinOp::ShiftLAssign
        | HBinOp::ShiftRAssign
        | HBinOp::UShiftRAssign
        | HBinOp::NullCoalesceAssign => return None,
    })
}

/// Type-of-init inference, sufficient to drive compound-assign
/// coercion (`var a = 10; a += 0.5` ⇒ keep `a` as Int). Only
/// returns `Some(...)` for shapes whose type is obvious from
/// syntax — numeric literals, simple numeric binops, container
/// literals, `new ClassName(...)`.
pub(crate) fn infer_simple_init_ty(e: &Expr) -> Option<Type> {
    match &e.kind {
        ExprKind::Literal(Literal::Int(_)) => Some(Type::Integer),
        ExprKind::Literal(Literal::Real(_)) => Some(Type::Real),
        ExprKind::Literal(Literal::Bool(_)) => Some(Type::Boolean),
        ExprKind::Literal(Literal::String(_)) => Some(Type::String),
        ExprKind::Array(_) => Some(Type::Array(Box::new(Type::Any))),
        ExprKind::Map(_) => Some(Type::Map(Box::new(Type::Any), Box::new(Type::Any))),
        ExprKind::Set(_) => Some(Type::Set(Box::new(Type::Any))),
        ExprKind::Object(_) => Some(Type::Object),
        ExprKind::Interval(_) => Some(Type::Interval),
        ExprKind::New(n) => Some(Type::ClassInstance(n.class.clone(), Vec::new())),
        ExprKind::Unary(leek_hir::UnaryOp::Neg | leek_hir::UnaryOp::Pos, x) => {
            infer_simple_init_ty(x)
        }
        ExprKind::Binary(op, l, r) => {
            use leek_hir::BinaryOp::{Add, Div, IntDiv, Mod, Mul, Pow, Sub};
            if !matches!(op, Add | Sub | Mul | Div | Mod | IntDiv | Pow) {
                return None;
            }
            let lt = infer_simple_init_ty(l)?;
            let rt = infer_simple_init_ty(r)?;
            Some(match (lt, rt) {
                (Type::Real, _) | (_, Type::Real) => Type::Real,
                (Type::Integer, Type::Integer) => Type::Integer,
                _ => return None,
            })
        }
        _ => None,
    }
}

pub(crate) fn lower_visibility(v: HirVisibility) -> Visibility {
    match v {
        HirVisibility::Public => Visibility::Public,
        HirVisibility::Protected => Visibility::Protected,
        HirVisibility::Private => Visibility::Private,
    }
}

/// A placeholder `MirFunction` used as the contents of a reserved
/// slot in `program.functions` while the real lambda body is still
/// pending. It has one block that immediately returns null so a
/// backend that somehow runs it before patching produces a
/// deterministic, harmless result.
pub(crate) fn placeholder_function(span: Span) -> MirFunction {
    MirFunction {
        def_id: None,
        kind: FunctionKind::User,
        name: "<lambda-placeholder>".into(),
        params: Vec::new(),
        return_ty: Type::Any,
        locals: Vec::new(),
        blocks: vec![BasicBlock {
            id: BlockId(0),
            statements: Vec::new(),
            statement_spans: Vec::new(),
            terminator: Terminator::Return(None),
            terminator_span: leek_span::Span::synthetic(),
        }],
        entry: BlockId(0),
        owning_class: None,
        span,
    }
}

/// Walk a HIR lambda body to find every `DefId` it references via
/// `NameRef::Local` that isn't declared inside the lambda itself.
/// Captures are returned in first-occurrence order so the slot
/// indices in the lambda's MirFunction are stable across runs.
///
/// Nested lambdas are walked too: any DefId they capture that the
/// outer lambda also doesn't declare becomes one of the outer
/// lambda's captures (the outer needs to hold it to construct the
/// inner's `MakeLambda`).
pub(crate) fn collect_lambda_captures(lam: &leek_hir::LambdaExpr) -> Vec<DefId> {
    let (caps, _) = collect_lambda_captures_full(lam);
    caps
}

/// Like `collect_lambda_captures` but also reports whether the body
/// references `this` / `super` / `Class_` — used to decide whether
/// we need to materialise an implicit `this` capture slot when a
/// lambda is lowered inside a method body.
pub(crate) fn collect_lambda_captures_full(lam: &leek_hir::LambdaExpr) -> (Vec<DefId>, bool) {
    use std::collections::HashSet;
    let mut declared: HashSet<DefId> = HashSet::new();
    for p in &lam.params {
        declared.insert(p.def);
    }
    let mut captures: Vec<DefId> = Vec::new();
    let mut seen: HashSet<DefId> = HashSet::new();
    let mut needs_this = false;
    match &lam.body {
        leek_hir::LambdaBody::Block(b) => {
            for s in &b.stmts {
                walk_stmt_captures(s, &mut declared, &mut captures, &mut seen, &mut needs_this);
            }
        }
        leek_hir::LambdaBody::Expr(e) => {
            walk_expr_captures(e, &mut declared, &mut captures, &mut seen, &mut needs_this);
        }
    }
    (captures, needs_this)
}

pub(crate) fn note_capture(
    def: DefId,
    declared: &std::collections::HashSet<DefId>,
    captures: &mut Vec<DefId>,
    seen: &mut std::collections::HashSet<DefId>,
) {
    if !declared.contains(&def) && seen.insert(def) {
        captures.push(def);
    }
}

pub(crate) fn walk_stmt_captures(
    s: &Stmt,
    declared: &mut std::collections::HashSet<DefId>,
    captures: &mut Vec<DefId>,
    seen: &mut std::collections::HashSet<DefId>,
    needs_this: &mut bool,
) {
    match s {
        Stmt::Expr(e) => walk_expr_captures(e, declared, captures, seen, needs_this),
        Stmt::VarDecl(v) => {
            // The init runs in the outer scope (before `v.def` is
            // bound for the body that follows), but Leekscript
            // permits self-recursive lambda bindings, so we declare
            // first then visit the init. The capture-discovery
            // doesn't care about order — we only track *which*
            // DefIds are bound here, not when.
            declared.insert(v.def);
            if let Some(init) = &v.init {
                walk_expr_captures(init, declared, captures, seen, needs_this);
            }
        }
        Stmt::Return(e) => {
            if let Some(e) = e {
                walk_expr_captures(e, declared, captures, seen, needs_this);
            }
        }
        Stmt::If(i) => {
            walk_expr_captures(&i.cond, declared, captures, seen, needs_this);
            walk_stmt_captures(&i.then_branch, declared, captures, seen, needs_this);
            if let Some(else_branch) = &i.else_branch {
                walk_stmt_captures(else_branch, declared, captures, seen, needs_this);
            }
        }
        Stmt::While(w) => {
            walk_expr_captures(&w.cond, declared, captures, seen, needs_this);
            walk_stmt_captures(&w.body, declared, captures, seen, needs_this);
        }
        Stmt::DoWhile(dw) => {
            walk_stmt_captures(&dw.body, declared, captures, seen, needs_this);
            walk_expr_captures(&dw.cond, declared, captures, seen, needs_this);
        }
        Stmt::For(f) => {
            if let Some(init) = &f.init {
                walk_stmt_captures(init, declared, captures, seen, needs_this);
            }
            if let Some(c) = &f.cond {
                walk_expr_captures(c, declared, captures, seen, needs_this);
            }
            if let Some(s) = &f.step {
                walk_expr_captures(s, declared, captures, seen, needs_this);
            }
            walk_stmt_captures(&f.body, declared, captures, seen, needs_this);
        }
        Stmt::Foreach(fe) => {
            walk_expr_captures(&fe.iter, declared, captures, seen, needs_this);
            if let Some(k) = &fe.key {
                declared.insert(k.def);
            }
            declared.insert(fe.value.def);
            walk_stmt_captures(&fe.body, declared, captures, seen, needs_this);
        }
        Stmt::Block(b) => {
            for s in &b.stmts {
                walk_stmt_captures(s, declared, captures, seen, needs_this);
            }
        }
        Stmt::Switch(sw) => {
            walk_expr_captures(&sw.discriminant, declared, captures, seen, needs_this);
            for arm in &sw.arms {
                if let Some(c) = &arm.case {
                    walk_expr_captures(c, declared, captures, seen, needs_this);
                }
                for s in &arm.body {
                    walk_stmt_captures(s, declared, captures, seen, needs_this);
                }
            }
        }
        Stmt::Break(_)
        | Stmt::Continue(_)
        | Stmt::Include(_)
        | Stmt::Import(_)
        | Stmt::Charge(_) => {}
    }
}

pub(crate) fn walk_expr_captures(
    e: &Expr,
    declared: &mut std::collections::HashSet<DefId>,
    captures: &mut Vec<DefId>,
    seen: &mut std::collections::HashSet<DefId>,
    needs_this: &mut bool,
) {
    match &e.kind {
        ExprKind::Literal(_) => {}
        ExprKind::Name(NameRef::Local(def)) => {
            note_capture(*def, declared, captures, seen);
        }
        ExprKind::Name(NameRef::This | NameRef::Super | NameRef::Class_) => {
            *needs_this = true;
        }
        ExprKind::Name(_) => {}
        ExprKind::Binary(_, l, r) => {
            walk_expr_captures(l, declared, captures, seen, needs_this);
            walk_expr_captures(r, declared, captures, seen, needs_this);
        }
        ExprKind::Unary(_, x) | ExprKind::Postfix(_, x) => {
            walk_expr_captures(x, declared, captures, seen, needs_this);
        }
        ExprKind::Call(c) => {
            match &c.callee {
                leek_hir::Callee::Function(NameRef::Local(def)) => {
                    note_capture(*def, declared, captures, seen);
                }
                leek_hir::Callee::Function(NameRef::This | NameRef::Super | NameRef::Class_) => {
                    *needs_this = true;
                }
                leek_hir::Callee::Function(_) => {}
                leek_hir::Callee::Method { receiver, .. } => {
                    walk_expr_captures(receiver, declared, captures, seen, needs_this);
                }
                leek_hir::Callee::Expr(e) => {
                    walk_expr_captures(e, declared, captures, seen, needs_this);
                }
            }
            for a in &c.args {
                walk_expr_captures(a, declared, captures, seen, needs_this);
            }
        }
        ExprKind::Field(b, ..) => walk_expr_captures(b, declared, captures, seen, needs_this),
        ExprKind::Index(b, i) => {
            walk_expr_captures(b, declared, captures, seen, needs_this);
            walk_expr_captures(i, declared, captures, seen, needs_this);
        }
        ExprKind::Slice(s) => {
            walk_expr_captures(&s.base, declared, captures, seen, needs_this);
            if let Some(x) = &s.start {
                walk_expr_captures(x, declared, captures, seen, needs_this);
            }
            if let Some(x) = &s.end {
                walk_expr_captures(x, declared, captures, seen, needs_this);
            }
            if let Some(x) = &s.step {
                walk_expr_captures(x, declared, captures, seen, needs_this);
            }
        }
        ExprKind::Array(items) => {
            for x in items {
                walk_expr_captures(x, declared, captures, seen, needs_this);
            }
        }
        ExprKind::Set(items) => {
            for x in items {
                walk_expr_captures(&x.start, declared, captures, seen, needs_this);
                if let Some(end) = &x.end {
                    walk_expr_captures(end, declared, captures, seen, needs_this);
                }
            }
        }
        ExprKind::Map(pairs) => {
            for (k, v) in pairs {
                walk_expr_captures(k, declared, captures, seen, needs_this);
                walk_expr_captures(v, declared, captures, seen, needs_this);
            }
        }
        ExprKind::Object(pairs) => {
            for (_, v) in pairs {
                walk_expr_captures(v, declared, captures, seen, needs_this);
            }
        }
        ExprKind::Ternary(c, t, f) => {
            walk_expr_captures(c, declared, captures, seen, needs_this);
            walk_expr_captures(t, declared, captures, seen, needs_this);
            walk_expr_captures(f, declared, captures, seen, needs_this);
        }
        ExprKind::Interval(iv) => {
            if let Some(x) = &iv.start {
                walk_expr_captures(x, declared, captures, seen, needs_this);
            }
            if let Some(x) = &iv.end {
                walk_expr_captures(x, declared, captures, seen, needs_this);
            }
            if let Some(x) = &iv.step {
                walk_expr_captures(x, declared, captures, seen, needs_this);
            }
        }
        ExprKind::Cast(x, _) => walk_expr_captures(x, declared, captures, seen, needs_this),
        ExprKind::New(n) => {
            for a in &n.args {
                walk_expr_captures(a, declared, captures, seen, needs_this);
            }
        }
        ExprKind::Lambda(inner) => {
            // Nested lambda has its own scope; compute its captures
            // recursively, then propagate any that aren't bound in
            // *our* scope — we'll need them to build the inner's
            // MakeLambda when we lower this expression.
            let (inner_caps, inner_needs_this) = collect_lambda_captures_full(inner);
            for c in inner_caps {
                note_capture(c, declared, captures, seen);
            }
            if inner_needs_this {
                *needs_this = true;
            }
        }
    }
}
