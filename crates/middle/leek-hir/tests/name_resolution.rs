//! `NameRef::Builtin` means a *real* builtin; every other name no binding
//! claims is `NameRef::Unresolved` (#53).
//!
//! The split is a pure retag — the two tags lower and emit identically — so
//! the tests that matter are the ones pinning what each name gets tagged as,
//! plus the in-class sugar (`field` → `this.field`, `m()` → `this.m()`) that
//! used to key on `Builtin` alone and would silently die if it still did.

use leek_hir::{Callee, Def, DefId, Expr, ExprKind, HirFile, NameRef, Stmt, lower_file};
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn lower(src: &str) -> HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(src, source, Version::V4);
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parses");
    lower_file(&file, source).0
}

/// The expression of the single `return` in `main`.
fn main_return(hir: &HirFile) -> &Expr {
    match hir.main.last() {
        Some(Stmt::Return(Some(e))) => e,
        other => panic!("expected a `return` in main, got {other:?}"),
    }
}

/// The expression of the single `return` in the body of method `m` of the
/// first class in the file.
fn method_return<'a>(hir: &'a HirFile, m: &str) -> &'a Expr {
    let class = hir
        .defs
        .iter()
        .find_map(|d| match d {
            Def::Class(c) => Some(c),
            _ => None,
        })
        .expect("a class");
    let body = class
        .methods
        .iter()
        .find(|x| x.name == m)
        .and_then(|x| x.body.as_ref())
        .unwrap_or_else(|| panic!("no method `{m}`"));
    match body.stmts.last() {
        Some(Stmt::Return(Some(e))) => e,
        other => panic!("expected a `return` in `{m}`, got {other:?}"),
    }
}

fn name_of(e: &Expr) -> &NameRef {
    match &e.kind {
        ExprKind::Name(n) => n,
        other => panic!("expected a name, got {other:?}"),
    }
}

fn callee_of(e: &Expr) -> &Callee {
    match &e.kind {
        ExprKind::Call(c) => &c.callee,
        other => panic!("expected a call, got {other:?}"),
    }
}

// ---- the tag itself -------------------------------------------------------

#[test]
fn a_real_builtin_function_is_tagged_builtin() {
    let hir = lower("return abs\n");
    assert!(
        matches!(name_of(main_return(&hir)), NameRef::Builtin(n) if n == "abs"),
        "got {:?}",
        name_of(main_return(&hir))
    );
}

#[test]
fn a_real_builtin_constant_is_tagged_builtin() {
    let hir = lower("return PI\n");
    assert!(
        matches!(name_of(main_return(&hir)), NameRef::Builtin(n) if n == "PI"),
        "got {:?}",
        name_of(main_return(&hir))
    );
}

#[test]
fn a_builtin_class_is_tagged_builtin() {
    let hir = lower("return Map\n");
    assert!(
        matches!(name_of(main_return(&hir)), NameRef::Builtin(n) if n == "Map"),
        "got {:?}",
        name_of(main_return(&hir))
    );
}

#[test]
fn an_undeclared_name_is_tagged_unresolved() {
    let hir = lower("return zzz\n");
    assert!(
        matches!(name_of(main_return(&hir)), NameRef::Unresolved(n) if n == "zzz"),
        "got {:?}",
        name_of(main_return(&hir))
    );
}

#[test]
fn an_undeclared_callee_keeps_its_name() {
    // The name must survive: MIR turns the callee into `Callee::Builtin(n)`
    // and the backends emit the call by name.
    let hir = lower("return zzz(1)\n");
    assert!(
        matches!(
            callee_of(main_return(&hir)),
            Callee::Function(NameRef::Unresolved(n)) if n == "zzz"
        ),
        "got {:?}",
        callee_of(main_return(&hir))
    );
}

#[test]
fn a_builtin_callee_stays_builtin() {
    let hir = lower("return abs(1)\n");
    assert!(
        matches!(
            callee_of(main_return(&hir)),
            Callee::Function(NameRef::Builtin(n)) if n == "abs"
        ),
        "got {:?}",
        callee_of(main_return(&hir))
    );
}

// ---- the in-class sugar the split could have broken ------------------------

#[test]
fn a_bare_field_name_still_rewrites_to_this_field() {
    // A field name is not a builtin, so it is now `Unresolved` on the way in.
    // `name_expr_kind` must still rewrite it.
    let hir = lower("class A { var f; m() { return f } }\nreturn 0\n");
    let e = method_return(&hir, "m");
    let ExprKind::Field(base, field, false) = &e.kind else {
        panic!("expected `this.f`, got {:?}", e.kind)
    };
    assert_eq!(field, "f");
    assert!(
        matches!(name_of(base), NameRef::This),
        "got {:?}",
        base.kind
    );
}

#[test]
fn a_bare_method_call_still_rewrites_to_a_method_call_on_this() {
    let hir = lower("class A { g() { return 1 } m() { return g() } }\nreturn 0\n");
    let Callee::Method {
        receiver, method, ..
    } = callee_of(method_return(&hir, "m"))
    else {
        panic!(
            "expected a method call, got {:?}",
            callee_of(method_return(&hir, "m"))
        )
    };
    assert_eq!(method, "g");
    assert!(
        matches!(name_of(receiver), NameRef::This),
        "got {:?}",
        receiver.kind
    );
}

#[test]
fn a_bare_static_method_call_still_rewrites_to_a_method_call_on_class() {
    let hir = lower("class A { static g() { return 1 } m() { return g() } }\nreturn 0\n");
    let Callee::Method {
        receiver, method, ..
    } = callee_of(method_return(&hir, "m"))
    else {
        panic!(
            "expected a method call, got {:?}",
            callee_of(method_return(&hir, "m"))
        )
    };
    assert_eq!(method, "g");
    assert!(
        matches!(name_of(receiver), NameRef::Class_),
        "got {:?}",
        receiver.kind
    );
}

#[test]
fn an_arity_miss_on_a_builtin_named_method_still_falls_through_to_the_builtin() {
    let hir = lower(
        "class A { sqrt() { return 1 } sqrt(x, y) { return 2 } m() { return sqrt(25) } }\nreturn 0\n",
    );
    assert!(
        matches!(
            callee_of(method_return(&hir, "m")),
            Callee::Function(NameRef::Builtin(n)) if n == "sqrt"
        ),
        "got {:?}",
        callee_of(method_return(&hir, "m"))
    );
}

#[test]
fn an_arity_miss_on_a_non_builtin_named_method_falls_through_unresolved() {
    let hir = lower(
        "class A { zzz() { return 1 } zzz(x, y) { return 2 } m() { return zzz(25) } }\nreturn 0\n",
    );
    assert!(
        matches!(
            callee_of(method_return(&hir, "m")),
            Callee::Function(NameRef::Unresolved(n)) if n == "zzz"
        ),
        "got {:?}",
        callee_of(method_return(&hir, "m"))
    );
}

// ---- `instanceof` ---------------------------------------------------------

/// The RHS of the single `instanceof` in `main`'s `return`.
fn instanceof_rhs(hir: &HirFile) -> &Expr {
    match &main_return(hir).kind {
        ExprKind::Binary(_, _, rhs) => rhs,
        other => panic!("expected a binary expression, got {other:?}"),
    }
}

#[test]
fn instanceof_against_a_global_resolves_to_that_global() {
    // `resolve_class_or_builtin` used to tag *everything* that wasn't a
    // user class as `Builtin`, so a `global` reached its own (name-keyed)
    // slot without any `DefId`-keyed pass seeing the use (#53).
    let hir = lower("global G = 1\nvar x = 1\nreturn x instanceof G\n");
    let i = hir
        .defs
        .iter()
        .position(|d| matches!(d, Def::Global(_)) && d.name() == "G")
        .expect("a global `G`");
    assert_eq!(
        name_of(instanceof_rhs(&hir)),
        &NameRef::Global(DefId(u32::try_from(i).unwrap())),
    );
}

#[test]
fn instanceof_against_a_user_class_still_resolves_to_that_class() {
    let hir = lower("class A {}\nvar x = new A()\nreturn x instanceof A\n");
    assert!(
        matches!(name_of(instanceof_rhs(&hir)), NameRef::Class(_)),
        "got {:?}",
        name_of(instanceof_rhs(&hir))
    );
}

#[test]
fn instanceof_against_a_builtin_class_is_still_builtin() {
    let hir = lower("var x = []\nreturn x instanceof Array\n");
    assert!(
        matches!(name_of(instanceof_rhs(&hir)), NameRef::Builtin(n) if n == "Array"),
        "got {:?}",
        name_of(instanceof_rhs(&hir))
    );
}

#[test]
fn instanceof_against_an_unknown_name_is_unresolved() {
    let hir = lower("var x = 1\nreturn x instanceof Zzz\n");
    assert!(
        matches!(name_of(instanceof_rhs(&hir)), NameRef::Unresolved(n) if n == "Zzz"),
        "got {:?}",
        name_of(instanceof_rhs(&hir))
    );
}

#[test]
fn a_local_named_like_a_builtin_class_does_not_shadow_it_in_instanceof() {
    // `resolve_class_or_builtin` deliberately skips locals.
    let hir = lower("var Map = 1\nvar x = 1\nreturn x instanceof Map\n");
    assert!(
        matches!(name_of(instanceof_rhs(&hir)), NameRef::Builtin(n) if n == "Map"),
        "got {:?}",
        name_of(instanceof_rhs(&hir))
    );
}

// ---- globals are never left to the fallback --------------------------------

/// A `global` can be declared *below* every use of it and still bind them,
/// which makes it the name most at risk of falling through to the
/// `Builtin`/`Unresolved` fallback. Lowering registers every `global` before
/// it lowers any expression, so it never does — `propagate_const_globals`
/// relies on that to key its disqualification rules on `DefId` alone (#53).
#[test]
fn a_global_is_never_tagged_unresolved() {
    // Read above its own `global` statement.
    let hir = lower("var y = G\nglobal G = 1\nreturn G\n");
    assert!(
        matches!(name_of(main_return(&hir)), NameRef::Global(_)),
        "got {:?}",
        name_of(main_return(&hir))
    );
    // An overloaded bodiless signature's parameter default is the last
    // expression lowering resolves before the globals are registered: the
    // body pass refills only the last same-named declaration, so pass 1 is
    // the only place the earlier overload's default is ever lowered.
    let hir = lower("global G = 1\nfunction f(x = G);\nfunction f(a, b);\nreturn G\n");
    let Some(Def::Function(f)) = hir
        .defs
        .iter()
        .find(|d| matches!(d, Def::Function(f) if f.params.len() == 1))
    else {
        panic!("expected the one-parameter overload")
    };
    let default = f.params[0].default.as_ref().expect("has a default");
    assert!(
        matches!(name_of(default), NameRef::Global(_)),
        "got {:?}",
        name_of(default)
    );
}
