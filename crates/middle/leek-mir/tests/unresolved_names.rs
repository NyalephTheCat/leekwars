//! `NameRef::Unresolved` lowers exactly like `NameRef::Builtin` (#53).
//!
//! HIR splits the two tags — `Builtin` is a real builtin, anything else no
//! binding claims is `Unresolved` — and the split is only safe because MIR
//! treats them identically. Each test below fails loudly if an `Unresolved`
//! arm is left behind: a read would become `Const::Null`, a write would raise
//! `lowering_unsupported`, and a call would lose its callee name.

use leek_hir::{
    BinaryOp, Call, Callee, DefId, Expr, ExprKind, HirFile, Literal, NameRef, Stmt, Type,
};
use leek_mir::{
    Callee as MirCallee, Const, MirProgram, Operand, Place, Rvalue, Statement, lower_file,
};
use leek_span::Span;

fn span() -> Span {
    Span::synthetic()
}

fn name(nr: NameRef) -> Expr {
    Expr {
        kind: ExprKind::Name(nr),
        ty: Type::Any,
        span: span(),
    }
}

fn lit_int(n: i64) -> Expr {
    Expr {
        kind: ExprKind::Literal(Literal::Int(n)),
        ty: Type::Integer,
        span: span(),
    }
}

/// Lower `main`, asserting the lowering raised no errors.
fn build(main: Vec<Stmt>) -> MirProgram {
    let h = HirFile {
        main,
        ..Default::default()
    };
    let (program, errs) = lower_file(&h);
    assert!(errs.is_empty(), "unexpected lowering errors: {errs:?}");
    program
}

/// Every `Assign` statement of `main`, in order.
fn assigns(prog: &MirProgram) -> Vec<(&Place, &Rvalue)> {
    prog.main()
        .expect("main")
        .blocks
        .iter()
        .flat_map(|b| &b.statements)
        .filter_map(|s| match s {
            Statement::Assign(p, rv) => Some((p, rv)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_write_to_an_unresolved_name_is_a_name_keyed_global_store() {
    // `zzz = 5` where nothing declares `zzz`. Without the `Unresolved` arm
    // in `lower_place` this falls to the catch-all and raises
    // "assignment target is not an l-value MIR knows how to model".
    let prog = build(vec![Stmt::Expr(Expr {
        kind: ExprKind::Binary(
            BinaryOp::Assign,
            Box::new(name(NameRef::Unresolved("zzz".into()))),
            Box::new(lit_int(5)),
        ),
        ty: Type::Integer,
        span: span(),
    })]);
    assert!(
        assigns(&prog)
            .iter()
            .any(|(p, _)| matches!(p, Place::Global(DefId(0), n) if n == "zzz")),
        "no name-keyed global store, got {:?}",
        assigns(&prog)
    );
}

#[test]
fn a_read_of_an_unresolved_name_is_a_builtin_ref_not_null() {
    // `return zzz`. A `Const::Null` here would silently swallow every read
    // of a global declared in another file or of an implicit global.
    let prog = build(vec![Stmt::Return(Some(name(NameRef::Unresolved(
        "zzz".into(),
    ))))]);
    let rvalues: Vec<&Rvalue> = assigns(&prog).into_iter().map(|(_, rv)| rv).collect();
    assert!(
        rvalues
            .iter()
            .any(|rv| matches!(rv, Rvalue::BuiltinRef(n) if n == "zzz")),
        "expected a BuiltinRef(\"zzz\"), got {rvalues:?}"
    );
    assert!(
        !rvalues
            .iter()
            .any(|rv| matches!(rv, Rvalue::Use(Operand::Const(Const::Null)))),
        "the read lowered to null: {rvalues:?}"
    );
}

#[test]
fn a_call_to_an_unresolved_name_keeps_the_callee_name() {
    // `zzz()`. The old arm mapped every unresolved callee to
    // `Callee::Builtin("?")`, which loses the name the backends emit.
    let prog = build(vec![Stmt::Return(Some(Expr {
        kind: ExprKind::Call(Box::new(Call {
            callee: Callee::Function(NameRef::Unresolved("zzz".into())),
            args: vec![],
            callee_span: span(),
            span: span(),
        })),
        ty: Type::Any,
        span: span(),
    }))]);
    let callees: Vec<&MirCallee> = prog
        .main()
        .expect("main")
        .blocks
        .iter()
        .flat_map(|b| &b.statements)
        .filter_map(|s| match s {
            Statement::Call { call, .. } => Some(&call.callee),
            _ => None,
        })
        .collect();
    assert_eq!(
        callees,
        vec![&MirCallee::Builtin("zzz".into())],
        "the callee name was lost",
    );
}

#[test]
fn an_unresolved_receiver_is_charge_exempt_like_a_builtin_one() {
    // `Zzz.field` — the builtin-class exemption (no `Charge(1)`) is keyed on
    // the base being a bare unclaimed name, so it must accept both tags or
    // the op counts drift.
    let charges = |nr: NameRef| {
        let prog = build(vec![Stmt::Return(Some(Expr {
            kind: ExprKind::Field(Box::new(name(nr)), "field".into(), false),
            ty: Type::Any,
            span: span(),
        }))]);
        prog.main()
            .expect("main")
            .blocks
            .iter()
            .flat_map(|b| &b.statements)
            .filter(|s| matches!(s, Statement::Charge(_)))
            .count()
    };
    assert_eq!(
        charges(NameRef::Unresolved("Zzz".into())),
        charges(NameRef::Builtin("Zzz".into())),
    );
}
