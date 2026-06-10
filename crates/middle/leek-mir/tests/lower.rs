//! HIR → MIR lowering tests. Each test builds a small HIR by hand
//! (rather than parsing source) so we exercise the lowering pass
//! in isolation. The shapes the tests check are the structural
//! contract MIR makes to its consumers — every block terminates,
//! short-circuit operators fork the CFG, etc.

use leek_hir::{
    BinaryOp, Block, Def, Expr, ExprKind, Function, HirFile, IfStmt, Literal, Stmt, Type, VarDecl,
    WhileStmt,
};
use leek_mir::{
    BinOp, Const, FunctionKind, MirProgram, Operand, Place, Rvalue, Statement, Terminator,
    lower_file,
};
use leek_span::Span;

fn span() -> Span {
    Span::synthetic()
}

fn lit_int(n: i64) -> Expr {
    Expr {
        kind: ExprKind::Literal(Literal::Int(n)),
        ty: Type::Integer,
        span: span(),
    }
}

fn lit_bool(b: bool) -> Expr {
    Expr {
        kind: ExprKind::Literal(Literal::Bool(b)),
        ty: Type::Boolean,
        span: span(),
    }
}

fn build(main: Vec<Stmt>) -> MirProgram {
    let h = HirFile {
        main,
        ..Default::default()
    };
    let (program, errs) = lower_file(&h);
    assert!(errs.is_empty(), "unexpected lowering errors: {errs:?}");
    program
}

// ---- Tests ----

#[test]
fn empty_file_yields_a_main_with_one_block() {
    let prog = build(vec![]);
    let main = prog.main().expect("main function present");
    assert_eq!(main.kind, FunctionKind::Main);
    assert_eq!(main.blocks.len(), 1);
    assert!(matches!(
        main.blocks[0].terminator,
        Terminator::Return(None)
    ));
}

#[test]
fn return_literal_emits_a_single_return_terminator() {
    let prog = build(vec![Stmt::Return(Some(lit_int(42)))]);
    let main = prog.main().unwrap();
    // Entry block ends in Return(Some(Const::Int(42))).
    let term = &main.blocks[0].terminator;
    match term {
        Terminator::Return(Some(Operand::Const(Const::Int(42)))) => {}
        other => panic!("expected Return(Some(Int 42)), got {other:?}"),
    }
}

#[test]
fn binary_add_flattens_into_temps_and_an_assign() {
    // var x = 1 + 2;
    let v = VarDecl {
        def: leek_hir::DefId(99),
        name: "x".into(),
        ty: Some(Type::Integer),
        init: Some(Expr {
            kind: ExprKind::Binary(BinaryOp::Add, Box::new(lit_int(1)), Box::new(lit_int(2))),
            ty: Type::Integer,
            span: span(),
        }),
        is_global: false,
        span: span(),
    };
    let prog = build(vec![Stmt::VarDecl(v)]);
    let main = prog.main().unwrap();
    // We expect: charge(1) for the `var x = e` store, then t0 = (1 + 2);
    // x = t0. The charge plus two assigns in the entry block, ahead of
    // an implicit Return.
    assert_eq!(main.blocks.len(), 1);
    let stmts = &main.blocks[0].statements;
    assert_eq!(
        stmts.len(),
        3,
        "expected a charge plus two assigns, got {stmts:?}"
    );
    match &stmts[0] {
        Statement::Charge(1) => {}
        other => panic!("first stmt should charge the store, got {other:?}"),
    }
    match &stmts[1] {
        Statement::Assign(_, Rvalue::Binary(BinOp::Add, _, _)) => {}
        other => panic!("second stmt should be the Add, got {other:?}"),
    }
    match &stmts[2] {
        Statement::Assign(Place::Local(_), Rvalue::Use(Operand::Local(_))) => {}
        other => panic!("third stmt should be the Use, got {other:?}"),
    }
}

#[test]
fn if_else_forks_into_three_blocks() {
    // if (true) return 1; else return 2;
    let i = IfStmt {
        cond: lit_bool(true),
        then_branch: Box::new(Stmt::Return(Some(lit_int(1)))),
        else_branch: Some(Box::new(Stmt::Return(Some(lit_int(2))))),
        soft: false,
        span: span(),
    };
    let prog = build(vec![Stmt::If(i)]);
    let main = prog.main().unwrap();
    // Entry: Branch -> then_bb, else_bb; then_bb: Return(1);
    // else_bb: Return(2); join_bb: implicit Return(None) (dead).
    assert!(main.blocks.len() >= 3);
    match &main.blocks[0].terminator {
        Terminator::Branch { .. } => {}
        other => panic!("entry must branch, got {other:?}"),
    }
    let mut returns = 0;
    for b in &main.blocks {
        if matches!(b.terminator, Terminator::Return(Some(_))) {
            returns += 1;
        }
    }
    assert_eq!(returns, 2, "both arms should end in Return");
}

#[test]
fn while_builds_header_body_exit_cfg() {
    // while (true) { return 1; }
    let w = WhileStmt {
        cond: lit_bool(true),
        body: Box::new(Stmt::Return(Some(lit_int(1)))),
        span: span(),
    };
    let prog = build(vec![Stmt::While(w)]);
    let main = prog.main().unwrap();
    // Expect at minimum: entry -> header -> branch(body, exit);
    // body -> return; exit -> implicit return.
    let header_count = main
        .blocks
        .iter()
        .filter(|b| matches!(b.terminator, Terminator::Branch { .. }))
        .count();
    assert_eq!(
        header_count, 1,
        "exactly one Branch terminator (the loop header) expected"
    );
    let goto_count = main
        .blocks
        .iter()
        .filter(|b| matches!(b.terminator, Terminator::Goto(_)))
        .count();
    assert!(goto_count >= 1, "entry should goto the header");
}

#[test]
fn short_circuit_and_lowers_to_branch_with_false_fallback() {
    // var x = true && false;
    let v = VarDecl {
        def: leek_hir::DefId(7),
        name: "x".into(),
        ty: Some(Type::Boolean),
        init: Some(Expr {
            kind: ExprKind::Binary(
                BinaryOp::And,
                Box::new(lit_bool(true)),
                Box::new(lit_bool(false)),
            ),
            ty: Type::Boolean,
            span: span(),
        }),
        is_global: false,
        span: span(),
    };
    let prog = build(vec![Stmt::VarDecl(v)]);
    let main = prog.main().unwrap();
    // Entry block must end in a Branch (the lhs test). The
    // false-fallback block must contain an Assign(_, Use(Const::Bool(false))).
    match &main.blocks[0].terminator {
        Terminator::Branch { .. } => {}
        other => panic!("entry must branch on the lhs, got {other:?}"),
    }
    let saw_false_const = main.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                Statement::Assign(_, Rvalue::Use(Operand::Const(Const::Bool(false))))
            )
        })
    });
    assert!(
        saw_false_const,
        "the false-fallback arm of `&&` should assign false to the join temp"
    );
}

#[test]
fn null_coalesce_branches_on_identity_eq_null() {
    // var x = null ?? 5;
    let v = VarDecl {
        def: leek_hir::DefId(5),
        name: "x".into(),
        ty: Some(Type::Integer),
        init: Some(Expr {
            kind: ExprKind::Binary(
                BinaryOp::NullCoalesce,
                Box::new(Expr {
                    kind: ExprKind::Literal(Literal::Null),
                    ty: Type::Null,
                    span: span(),
                }),
                Box::new(lit_int(5)),
            ),
            ty: Type::Integer,
            span: span(),
        }),
        is_global: false,
        span: span(),
    };
    let prog = build(vec![Stmt::VarDecl(v)]);
    let main = prog.main().unwrap();
    let saw_id_eq = main.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                Statement::Assign(_, Rvalue::Binary(BinOp::IdentityEq, _, _))
            )
        })
    });
    assert!(
        saw_id_eq,
        "?? lowering should test the lhs against null via ==="
    );
}

#[test]
fn function_lowers_to_its_own_mir_function() {
    let mut h = HirFile::default();
    h.defs.push(Def::Function(Function {
        name: "foo".into(),
        span: span(),
        params: vec![],
        return_type: Some(Type::Integer),
        body: Some(Block {
            stmts: vec![Stmt::Return(Some(lit_int(7)))],
            span: span(),
        }),
        backend_directives: vec![],
    }));
    let (program, errs) = lower_file(&h);
    assert!(errs.is_empty(), "{errs:?}");
    // Two functions: foo + the synthetic main.
    assert_eq!(program.functions.len(), 2);
    let foo = &program.functions[0];
    assert_eq!(foo.name, "foo");
    assert_eq!(foo.kind, FunctionKind::User);
    assert!(matches!(
        foo.blocks[0].terminator,
        Terminator::Return(Some(Operand::Const(Const::Int(7))))
    ));
}

// ---- Class layout (compute_class_layouts) ----

use leek_hir::{Class, DefId, Field, MethodDef, Param, Visibility as HVis};

fn hfield(def: u32, name: &str) -> Field {
    Field {
        def: DefId(def),
        name: name.into(),
        ty: None,
        init: None,
        is_static: false,
        is_final: false,
        visibility: HVis::Public,
        span: span(),
    }
}

fn hmethod(def: u32, name: &str, arity: usize) -> MethodDef {
    let params = (0u32..)
        .take(arity)
        .map(|i| Param {
            def: DefId(5000 + def * 10 + i),
            name: format!("p{i}"),
            ty: None,
            default: None,
            is_by_ref: false,
            span: span(),
        })
        .collect();
    MethodDef {
        def: DefId(def),
        name: name.into(),
        params,
        return_type: None,
        body: None,
        is_static: false,
        visibility: HVis::Public,
        span: span(),
    }
}

/// Build a two-class hierarchy:
///   class A { x; get(); foo(a); constructor(v) }
///   class B extends A { y; x /*override*/; get() /*override*/; foo(a,b) /*overload*/ }
/// A is `defs[0]` → DefId(0); B is `defs[1]` → DefId(1).
fn hierarchy() -> MirProgram {
    let a = Class {
        name: "A".into(),
        span: span(),
        parent: None,
        fields: vec![hfield(100, "x")],
        methods: vec![hmethod(101, "get", 0), hmethod(102, "foo", 1)],
        constructors: vec![hmethod(103, "A", 1)],
    };
    let b = Class {
        name: "B".into(),
        span: span(),
        parent: Some("A".into()),
        fields: vec![hfield(200, "y"), hfield(201, "x")],
        methods: vec![hmethod(202, "get", 0), hmethod(203, "foo", 2)],
        constructors: vec![],
    };
    let mut h = HirFile::default();
    h.defs.push(Def::Class(a));
    h.defs.push(Def::Class(b));
    let (program, errs) = lower_file(&h);
    assert!(errs.is_empty(), "unexpected lowering errors: {errs:?}");
    program
}

#[test]
fn parent_def_is_resolved() {
    let prog = hierarchy();
    let a = prog.class_by_name("A").unwrap();
    let b = prog.class_by_name("B").unwrap();
    assert_eq!(a.parent_def, None);
    assert_eq!(b.parent_def, Some(a.def_id));
}

#[test]
fn field_layout_flattens_with_inherited_first_and_override_reuses_slot() {
    let prog = hierarchy();
    let a = prog.class_by_name("A").unwrap();
    let b = prog.class_by_name("B").unwrap();

    // A only declares `x` at slot 0.
    assert_eq!(a.field_layout.len(), 1);
    assert_eq!(a.field_layout[0].name, "x");
    assert_eq!(a.field_layout[0].slot, 0);
    assert_eq!(a.field_layout[0].owner, a.def_id);

    // B: inherited `x` keeps slot 0 (owner now B, since B redeclares
    // it); `y` is appended at slot 1.
    assert_eq!(b.field_layout.len(), 2);
    let x = b.field_slot("x").unwrap();
    assert_eq!(x.slot, 0);
    assert_eq!(x.owner, b.def_id);
    let y = b.field_slot("y").unwrap();
    assert_eq!(y.slot, 1);
    assert_eq!(y.owner, b.def_id);
}

#[test]
fn vtable_overrides_in_place_and_overloads_get_new_slots() {
    let prog = hierarchy();
    let a = prog.class_by_name("A").unwrap();
    let b = prog.class_by_name("B").unwrap();

    // get/0 (slot 0, overridden by B), foo/1 (slot 1, owner A),
    // foo/2 (slot 2, owner B — distinct arity = new slot).
    assert_eq!(b.vtable.len(), 3);
    let get0 = prog.resolve_method(b, "get", Some(0)).unwrap();
    assert_eq!(get0.slot, 0);
    assert_eq!(get0.owner, b.def_id);

    let foo1 = prog.resolve_method(b, "foo", Some(1)).unwrap();
    assert_eq!(foo1.slot, 1);
    assert_eq!(foo1.owner, a.def_id);

    let foo2 = prog.resolve_method(b, "foo", Some(2)).unwrap();
    assert_eq!(foo2.slot, 2);
    assert_eq!(foo2.owner, b.def_id);
}

#[test]
fn resolve_method_any_arity_fallback_prefers_most_derived() {
    let prog = hierarchy();
    let a = prog.class_by_name("A").unwrap();
    let b = prog.class_by_name("B").unwrap();

    // No `foo` overload takes 5 args → fall back to the most-derived
    // same-name method, which is B's foo/2.
    let fallback = prog.resolve_method(b, "foo", Some(5)).unwrap();
    assert_eq!(fallback.owner, b.def_id);
    assert_eq!(fallback.user_arity, 2);

    // A in isolation only has foo/1.
    let a_fb = prog.resolve_method(a, "foo", Some(9)).unwrap();
    assert_eq!(a_fb.owner, a.def_id);
    assert_eq!(a_fb.user_arity, 1);
}

#[test]
fn select_constructor_walks_to_ancestor_and_matches_arity() {
    let prog = hierarchy();
    let a = prog.class_by_name("A").unwrap();
    let b = prog.class_by_name("B").unwrap();

    // B declares no constructor → inherits A's. A's only ctor is
    // arity 1, so any argc resolves to it.
    let a_ctor_fn = prog.select_constructor(a, 1).unwrap();
    assert_eq!(prog.select_constructor(b, 1), Some(a_ctor_fn));
    assert_eq!(prog.select_constructor(b, 0), Some(a_ctor_fn));
}
