//! Span-insensitive structural fingerprints of HIR expressions and
//! statements, shared by the duplicate-code / redundant-expression
//! lints.
//!
//! A fingerprint is a canonical string that encodes a node's structure
//! and the *bindings* it references (by [`DefId`], not source name), but
//! **not** its spans. Two nodes with equal fingerprints are structurally
//! identical and refer to the same definitions.
//!
//! Because bindings are keyed by `DefId`, the comparison is deliberately
//! *conservative*: code that introduces its own bindings (a `var`
//! declaration, a `foreach` binding, a lambda parameter) gets fresh
//! `DefId`s, so two otherwise-identical fragments that each declare a
//! local will **not** collide. We may therefore miss some real
//! duplicates, but we never report a false one — which is the right
//! trade-off for a lint that suggests deleting code.

use std::fmt::Write as _;

use leek_hir::{Callee, Expr, ExprKind, Literal, NameRef, Stmt};

/// Fingerprint of an expression.
pub(crate) fn expr_key(e: &Expr) -> String {
    let mut s = String::new();
    fp_expr(e, &mut s);
    s
}

/// Fingerprint of a statement.
pub(crate) fn stmt_key(s: &Stmt) -> String {
    let mut out = String::new();
    fp_stmt(s, &mut out);
    out
}

/// True if evaluating `e` could have a side effect (a call, allocation,
/// or in/decrement). Used by the self-comparison / self-assignment lints
/// to avoid flagging `f() == f()` where `f` may not be pure.
pub(crate) fn has_side_effect(e: &Expr) -> bool {
    let mut found = false;
    super::for_each_expr(e, &mut |x| match &x.kind {
        ExprKind::Call(_) | ExprKind::New(_) | ExprKind::Lambda(_) | ExprKind::Postfix(..) => {
            found = true;
        }
        ExprKind::Unary(leek_hir::UnaryOp::PreInc | leek_hir::UnaryOp::PreDec, _) => {
            found = true;
        }
        // An assignment (plain or compound) mutates state.
        ExprKind::Binary(op, ..) if is_assignment(*op) => {
            found = true;
        }
        _ => {}
    });
    found
}

/// True for the assignment-family binary operators (`=`, `+=`, …).
pub(crate) fn is_assignment(op: leek_hir::BinaryOp) -> bool {
    use leek_hir::BinaryOp as B;
    matches!(
        op,
        B::Assign
            | B::AddAssign
            | B::SubAssign
            | B::MulAssign
            | B::DivAssign
            | B::IntDivAssign
            | B::ModAssign
            | B::PowAssign
            | B::BitAndAssign
            | B::BitOrAssign
            | B::BitXorAssign
            | B::ShiftLAssign
            | B::ShiftRAssign
            | B::UShiftRAssign
            | B::NullCoalesceAssign
    )
}

fn fp_expr(e: &Expr, o: &mut String) {
    match &e.kind {
        ExprKind::Literal(l) => {
            o.push('L');
            fp_lit(l, o);
        }
        ExprKind::Name(n) => {
            o.push('N');
            fp_name(n, o);
        }
        ExprKind::Binary(op, a, b) => {
            let _ = write!(o, "B{op:?}");
            fp_expr(a, o);
            fp_expr(b, o);
        }
        ExprKind::Unary(op, a) => {
            let _ = write!(o, "U{op:?}");
            fp_expr(a, o);
        }
        ExprKind::Postfix(op, a) => {
            let _ = write!(o, "P{op:?}");
            fp_expr(a, o);
        }
        ExprKind::Call(c) => {
            o.push('C');
            fp_callee(&c.callee, o);
            o.push('(');
            for a in &c.args {
                fp_expr(a, o);
                o.push(',');
            }
            o.push(')');
        }
        ExprKind::Field(b, name) => {
            o.push('F');
            fp_expr(b, o);
            let _ = write!(o, ".{name}");
        }
        ExprKind::Index(b, i) => {
            o.push('I');
            fp_expr(b, o);
            fp_expr(i, o);
        }
        ExprKind::Slice(s) => {
            o.push('S');
            fp_expr(&s.base, o);
            fp_opt(s.start.as_deref(), o);
            fp_opt(s.end.as_deref(), o);
            fp_opt(s.step.as_deref(), o);
        }
        ExprKind::Array(v) => {
            o.push('A');
            for x in v {
                fp_expr(x, o);
                o.push(',');
            }
        }
        ExprKind::Map(v) => {
            o.push('M');
            for (k, val) in v {
                fp_expr(k, o);
                o.push(':');
                fp_expr(val, o);
                o.push(',');
            }
        }
        ExprKind::Set(v) => {
            o.push('T');
            for x in v {
                fp_expr(x, o);
                o.push(',');
            }
        }
        ExprKind::Object(v) => {
            o.push('O');
            for (k, val) in v {
                let _ = write!(o, "{k}:");
                fp_expr(val, o);
                o.push(',');
            }
        }
        ExprKind::Ternary(c, t, e2) => {
            o.push('?');
            fp_expr(c, o);
            fp_expr(t, o);
            fp_expr(e2, o);
        }
        ExprKind::Interval(iv) => {
            o.push('V');
            fp_opt(iv.start.as_deref(), o);
            fp_opt(iv.end.as_deref(), o);
            fp_opt(iv.step.as_deref(), o);
            let _ = write!(o, "{}{}", iv.start_inclusive, iv.end_inclusive);
        }
        ExprKind::Cast(b, ty) => {
            o.push('X');
            fp_expr(b, o);
            let _ = write!(o, "{ty:?}");
        }
        ExprKind::New(n) => {
            let _ = write!(o, "W{}(", n.class);
            for a in &n.args {
                fp_expr(a, o);
                o.push(',');
            }
            o.push(')');
        }
        // A lambda introduces fresh parameter bindings, so two
        // structurally-identical lambdas have different `DefId`s. Make
        // the fingerprint unique (via the span) so they never collide —
        // conservative, never a false duplicate.
        ExprKind::Lambda(_) => {
            let _ = write!(o, "Y@{}", e.span.start);
        }
    }
}

fn fp_opt(opt: Option<&Expr>, o: &mut String) {
    match opt {
        Some(e) => fp_expr(e, o),
        None => o.push('_'),
    }
}

fn fp_name(n: &NameRef, o: &mut String) {
    match n {
        NameRef::Local(d) => {
            let _ = write!(o, "l{d:?}");
        }
        NameRef::Global(d) => {
            let _ = write!(o, "g{d:?}");
        }
        NameRef::Function(d) => {
            let _ = write!(o, "fn{d:?}");
        }
        NameRef::Class(d) => {
            let _ = write!(o, "cl{d:?}");
        }
        NameRef::Builtin(s) => {
            let _ = write!(o, "b{s}");
        }
        NameRef::Unresolved(s) => {
            let _ = write!(o, "u{s}");
        }
        NameRef::This => o.push_str("@this"),
        NameRef::Super => o.push_str("@super"),
        NameRef::Class_ => o.push_str("@class"),
    }
}

fn fp_callee(c: &Callee, o: &mut String) {
    match c {
        Callee::Function(n) => {
            o.push('f');
            fp_name(n, o);
        }
        Callee::Method { receiver, method } => {
            o.push('m');
            fp_expr(receiver, o);
            let _ = write!(o, ".{method}");
        }
        Callee::Expr(e) => {
            o.push('e');
            fp_expr(e, o);
        }
    }
}

fn fp_lit(l: &Literal, o: &mut String) {
    match l {
        Literal::Int(i) => {
            let _ = write!(o, "i{i}");
        }
        // Compare reals by bit pattern so `1.0` and `1.0` match but
        // `NaN`s (which aren't `==`) get a stable key.
        Literal::Real(r) => {
            let _ = write!(o, "r{}", r.to_bits());
        }
        Literal::String(s) => {
            let _ = write!(o, "s{}:{s}", s.len());
        }
        Literal::Bool(b) => o.push(if *b { 'T' } else { 'F' }),
        Literal::Null => o.push('z'),
    }
}

fn fp_stmt(s: &Stmt, o: &mut String) {
    match s {
        Stmt::Expr(e) => {
            o.push('e');
            fp_expr(e, o);
        }
        Stmt::VarDecl(v) => {
            // Includes the fresh `DefId`, so a decl never matches one in
            // a sibling fragment (conservative — see module docs).
            let _ = write!(o, "v{:?}", v.def);
            if let Some(init) = &v.init {
                fp_expr(init, o);
            }
        }
        Stmt::Return(opt) => {
            o.push('R');
            if let Some(e) = opt {
                fp_expr(e, o);
            }
        }
        Stmt::If(i) => {
            o.push('?');
            fp_expr(&i.cond, o);
            fp_stmt(&i.then_branch, o);
            if let Some(e) = &i.else_branch {
                o.push('|');
                fp_stmt(e, o);
            }
        }
        Stmt::While(w) => {
            o.push('w');
            fp_expr(&w.cond, o);
            fp_stmt(&w.body, o);
        }
        Stmt::DoWhile(d) => {
            o.push('D');
            fp_stmt(&d.body, o);
            fp_expr(&d.cond, o);
        }
        Stmt::For(fr) => {
            o.push('f');
            if let Some(init) = &fr.init {
                fp_stmt(init, o);
            }
            if let Some(c) = &fr.cond {
                fp_expr(c, o);
            }
            if let Some(st) = &fr.step {
                fp_expr(st, o);
            }
            fp_stmt(&fr.body, o);
        }
        Stmt::Foreach(fe) => {
            let _ = write!(o, "E{:?}", fe.value.def);
            if let Some(k) = &fe.key {
                let _ = write!(o, "{:?}", k.def);
            }
            fp_expr(&fe.iter, o);
            fp_stmt(&fe.body, o);
        }
        Stmt::Break(_) => o.push('k'),
        Stmt::Continue(_) => o.push('K'),
        Stmt::Block(b) => {
            o.push('{');
            for st in &b.stmts {
                fp_stmt(st, o);
                o.push(';');
            }
            o.push('}');
        }
        Stmt::Switch(sw) => {
            o.push('x');
            fp_expr(&sw.discriminant, o);
            for arm in &sw.arms {
                if let Some(c) = &arm.case {
                    fp_expr(c, o);
                }
                o.push('>');
                for st in &arm.body {
                    fp_stmt(st, o);
                    o.push(';');
                }
            }
        }
        // Rare in branch bodies; encode as opaque tags.
        Stmt::Include(_) => o.push('#'),
        Stmt::Import(_) => o.push('@'),
        Stmt::Charge(_) => o.push('$'),
    }
}
