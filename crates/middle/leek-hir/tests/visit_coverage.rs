//! Every HIR variant has a child-enumeration fixture.
//!
//! The shallow walkers in `leek_hir::visit` are the one place the
//! per-variant child list is written down; several crates used to keep
//! their own copy and quietly dropped a variant each. This test is the
//! guard for the surviving copy.
//!
//! It works in two halves. `expr_variant` / `stmt_variant` `match`
//! exhaustively with no `_` arm, so adding an `ExprKind` or `Stmt`
//! variant stops this file compiling. The coverage assertions then
//! require a fixture naming that variant, and each fixture pins the
//! number of children the walkers report — which is what a hand-rolled
//! walker gets wrong when it silently skips a subtree.
//!
//! The last section covers the file-level enumerators, whose per-variant
//! list is a different one: not the children of a node, but every root a
//! *file* can hang executable code off.

use leek_hir::{
    BinaryOp, BodyRoot, Def, Expr, ExprKind, Global, HirFile, ImportStmt, IncludeStmt, NameRef,
    Stmt, walk_expr_children, walk_stmt_child_exprs, walk_stmt_child_stmts, walk_stmts_deep,
};
use leek_parser::ast::{AstNode, SourceFile};
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxNode, Version};

fn lower(src: &str) -> HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = leek_parser::parse(src, source, Version::V4);
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parses");
    leek_hir::lower_file(&file, source).0
}

/// The variant `e` is. Exhaustive by design — see the module docs.
fn expr_variant(e: &Expr) -> &'static str {
    match &e.kind {
        ExprKind::Literal(_) => "Literal",
        ExprKind::Name(_) => "Name",
        ExprKind::Binary(..) => "Binary",
        ExprKind::Unary(..) => "Unary",
        ExprKind::Postfix(..) => "Postfix",
        ExprKind::Call(_) => "Call",
        ExprKind::Field(..) => "Field",
        ExprKind::Index(..) => "Index",
        ExprKind::Slice(_) => "Slice",
        ExprKind::Array(_) => "Array",
        ExprKind::Map(_) => "Map",
        ExprKind::Set(_) => "Set",
        ExprKind::Object(_) => "Object",
        ExprKind::Ternary(..) => "Ternary",
        ExprKind::Interval(_) => "Interval",
        ExprKind::Cast(..) => "Cast",
        ExprKind::New(_) => "New",
        ExprKind::Lambda(_) => "Lambda",
    }
}

/// The variant `s` is. Exhaustive by design — see the module docs.
fn stmt_variant(s: &Stmt) -> &'static str {
    match s {
        Stmt::Expr(_) => "Expr",
        Stmt::VarDecl(_) => "VarDecl",
        Stmt::Return(_) => "Return",
        Stmt::If(_) => "If",
        Stmt::While(_) => "While",
        Stmt::DoWhile(_) => "DoWhile",
        Stmt::For(_) => "For",
        Stmt::Foreach(_) => "Foreach",
        Stmt::Break(_) => "Break",
        Stmt::Continue(_) => "Continue",
        Stmt::Block(_) => "Block",
        Stmt::Switch(_) => "Switch",
        Stmt::Include(_) => "Include",
        Stmt::Import(_) => "Import",
        Stmt::Charge(_) => "Charge",
    }
}

/// One fixture: a program, the variant it is about, and how many
/// immediate children the walkers must report for it.
struct ExprCase {
    variant: &'static str,
    src: &'static str,
    children: usize,
}

/// Each program ends in `var p = <the expression under test>`.
const EXPR_CASES: &[ExprCase] = &[
    ExprCase {
        variant: "Literal",
        src: "var p = 1\n",
        children: 0,
    },
    ExprCase {
        variant: "Name",
        src: "var y = 1\nvar p = y\n",
        children: 0,
    },
    ExprCase {
        variant: "Binary",
        src: "var p = 1 + 2\n",
        children: 2,
    },
    ExprCase {
        variant: "Unary",
        src: "var y = 1\nvar p = -y\n",
        children: 1,
    },
    ExprCase {
        variant: "Postfix",
        src: "var y = 1\nvar p = y++\n",
        children: 1,
    },
    ExprCase {
        variant: "Call",
        src: "var p = max(1, 2)\n",
        children: 2,
    },
    ExprCase {
        variant: "Field",
        src: "var y = {a: 1}\nvar p = y.a\n",
        children: 1,
    },
    ExprCase {
        variant: "Index",
        src: "var y = [1]\nvar p = y[0]\n",
        children: 2,
    },
    ExprCase {
        variant: "Slice",
        src: "var y = [1, 2, 3]\nvar p = y[0:2]\n",
        children: 3,
    },
    ExprCase {
        variant: "Array",
        src: "var p = [1, 2, 3]\n",
        children: 3,
    },
    ExprCase {
        variant: "Map",
        src: "var p = [1: 2]\n",
        children: 2,
    },
    ExprCase {
        variant: "Set",
        src: "var p = <1, 2>\n",
        children: 2,
    },
    ExprCase {
        variant: "Object",
        src: "var p = {a: 1, b: 2}\n",
        children: 2,
    },
    ExprCase {
        variant: "Ternary",
        src: "var p = true ? 1 : 2\n",
        children: 3,
    },
    ExprCase {
        variant: "Interval",
        src: "var p = [1..3]\n",
        children: 2,
    },
    ExprCase {
        variant: "Cast",
        src: "var y = 1\nvar p = y as string\n",
        children: 1,
    },
    ExprCase {
        variant: "New",
        src: "class Foo { }\nvar p = new Foo()\n",
        children: 0,
    },
    // A lambda is a **leaf** to the shallow walkers — see the module
    // docs of `leek_hir::visit`. Zero children is the contract, not an
    // omission; `walk_stmts_deep` is what crosses the boundary.
    ExprCase {
        variant: "Lambda",
        src: "var p = function() { return 1 }\n",
        children: 0,
    },
];

/// The `var p = …` initializer of the program's last main statement.
fn probe_expr(hir: &HirFile) -> &Expr {
    match hir.main.last().expect("a statement") {
        Stmt::VarDecl(v) => v.init.as_ref().expect("an initializer"),
        other => panic!(
            "fixture must end in `var p = …`, got {}",
            stmt_variant(other)
        ),
    }
}

#[test]
fn every_expr_variant_has_a_child_count_fixture() {
    let mut covered: Vec<&str> = EXPR_CASES.iter().map(|c| c.variant).collect();
    covered.sort_unstable();
    covered.dedup();
    for case in EXPR_CASES {
        let hir = lower(case.src);
        let e = probe_expr(&hir);
        assert_eq!(
            expr_variant(e),
            case.variant,
            "fixture for {} lowered to a different variant: {}",
            case.variant,
            case.src
        );
        let mut n = 0;
        walk_expr_children(e, &mut |_| n += 1);
        assert_eq!(
            n, case.children,
            "walk_expr_children reported {n} children for {}, expected {}",
            case.variant, case.children
        );
    }
    // `expr_variant`'s match is exhaustive, so this list is the full set
    // of variants; a new one fails to compile above and then shows up
    // here as a missing fixture.
    assert_eq!(
        covered.len(),
        18,
        "every ExprKind variant needs a fixture; have {covered:?}"
    );
}

struct StmtCase {
    variant: &'static str,
    src: &'static str,
    exprs: usize,
    stmts: usize,
}

const STMT_CASES: &[StmtCase] = &[
    StmtCase {
        variant: "Expr",
        src: "var y = 1\ny++\n",
        exprs: 1,
        stmts: 0,
    },
    StmtCase {
        variant: "VarDecl",
        src: "var y = 1\n",
        exprs: 1,
        stmts: 0,
    },
    StmtCase {
        variant: "Return",
        src: "return 1\n",
        exprs: 1,
        stmts: 0,
    },
    StmtCase {
        variant: "If",
        src: "if (true) { var a = 1 } else { var b = 2 }\n",
        exprs: 1,
        stmts: 2,
    },
    StmtCase {
        variant: "While",
        src: "while (false) { var a = 1 }\n",
        exprs: 1,
        stmts: 1,
    },
    StmtCase {
        variant: "DoWhile",
        src: "do { var a = 1 } while (false)\n",
        exprs: 1,
        stmts: 1,
    },
    // `cond` + `step` on the expression side, `init` + `body` on the
    // statement side.
    StmtCase {
        variant: "For",
        src: "for (var i = 0; i < 2; i++) { var a = 1 }\n",
        exprs: 2,
        stmts: 2,
    },
    StmtCase {
        variant: "Foreach",
        src: "for (v in [1]) { var a = 1 }\n",
        exprs: 1,
        stmts: 1,
    },
    StmtCase {
        variant: "Break",
        src: "while (true) { break }\n",
        exprs: 0,
        stmts: 0,
    },
    StmtCase {
        variant: "Continue",
        src: "while (true) { continue }\n",
        exprs: 0,
        stmts: 0,
    },
    StmtCase {
        variant: "Block",
        src: "{ var a = 1 }\n",
        exprs: 0,
        stmts: 1,
    },
    // Discriminant + one case label on the expression side, the arm body
    // on the statement side.
    StmtCase {
        variant: "Switch",
        src: "var y = 1\nswitch (y) { case 1: break }\n",
        exprs: 2,
        stmts: 1,
    },
];

/// Invoke `f` on every statement in `stmts`, at any depth (lambda
/// bodies excluded — that is what [`walk_stmts_deep`] is for).
fn visit_all(stmts: &[Stmt], f: &mut dyn FnMut(&Stmt)) {
    fn go(s: &Stmt, f: &mut dyn FnMut(&Stmt)) {
        f(s);
        walk_stmt_child_stmts(s, &mut |c| go(c, f));
    }
    for s in stmts {
        go(s, f);
    }
}

#[test]
fn every_stmt_variant_has_a_child_count_fixture() {
    for case in STMT_CASES {
        let hir = lower(case.src);
        let mut checked = false;
        visit_all(&hir.main, &mut |s| {
            if checked || stmt_variant(s) != case.variant {
                return;
            }
            checked = true;
            let mut exprs = 0;
            walk_stmt_child_exprs(s, &mut |_| exprs += 1);
            let mut stmts = 0;
            walk_stmt_child_stmts(s, &mut |_| stmts += 1);
            assert_eq!(
                (exprs, stmts),
                (case.exprs, case.stmts),
                "child counts for {}",
                case.variant
            );
        });
        assert!(
            checked,
            "fixture produced no {} statement: {}",
            case.variant, case.src
        );
    }

    // `Include`, `Import` and `Charge` are not source-constructible here
    // — the parser resolves includes and only `leek-charge` synthesizes a
    // `Charge` — so they are built directly. All three are childless, and
    // the walkers must still route them somewhere rather than fall off a
    // `_` arm.
    let span = Span::new(SourceId::new(1).unwrap(), 0, 0);
    let synthetic = [
        Stmt::Include(IncludeStmt {
            path: "x".into(),
            span,
        }),
        Stmt::Import(ImportStmt {
            path: "x".into(),
            span,
        }),
        Stmt::Charge(3),
    ];
    for s in &synthetic {
        let mut n = 0;
        walk_stmt_child_exprs(s, &mut |_| n += 1);
        walk_stmt_child_stmts(s, &mut |_| n += 1);
        assert_eq!(n, 0, "{} must report no children", stmt_variant(s));
    }

    let mut covered: Vec<&str> = STMT_CASES.iter().map(|c| c.variant).collect();
    covered.extend(synthetic.iter().map(stmt_variant));
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        covered.len(),
        15,
        "every Stmt variant needs a fixture; have {covered:?}"
    );
}

/// The deep walker's whole reason to exist: it reports statements inside
/// a lambda body, which the shallow pair never reaches.
#[test]
fn walk_stmts_deep_crosses_lambda_bodies() {
    let hir = lower("var f = function() { var inner = 1 return inner }\n");
    let decl = hir.main.first().expect("a statement");

    let mut shallow = 0;
    walk_stmt_child_stmts(decl, &mut |_| shallow += 1);
    assert_eq!(shallow, 0, "the shallow walker must stop at the lambda");

    let mut names: Vec<&'static str> = Vec::new();
    walk_stmts_deep(decl, &mut |s| names.push(stmt_variant(s)));
    // The `var f` declaration itself, plus `var inner` and `return inner`
    // from inside the lambda body.
    assert_eq!(names, ["VarDecl", "VarDecl", "Return"]);
}

// ---------------------------------------------------------------------------
// File-level enumerators
//
// The walkers above take a tree; these take a *file* and have to know every
// place executable code can hide in one. The fixtures below pin the two
// things a hand-rolled version gets wrong: which roots exist at all (#253),
// and where the lambda boundary sits.
// ---------------------------------------------------------------------------

/// The builtin `e` reassigns, when `e` is `<builtin> = …` — the shape the
/// Java backend hunts for to know which builtins a file shadows.
///
/// Both name tags count: whether a name lowers to `Builtin` or `Unresolved`
/// depends on which library was registered before lowering ran, so a
/// name-keyed analysis must accept either (see `builtin_or_unresolved`).
fn reassigned_builtin(e: &Expr) -> Option<&str> {
    let ExprKind::Binary(BinaryOp::Assign, lhs, _) = &e.kind else {
        return None;
    };
    match &lhs.kind {
        ExprKind::Name(NameRef::Builtin(n) | NameRef::Unresolved(n)) => Some(n),
        _ => None,
    }
}

/// Every name `walk_file_exprs` sees reassigned in `hir`, in walk order.
fn reassigned_builtins(hir: &HirFile) -> Vec<String> {
    let mut seen = Vec::new();
    leek_hir::walk_file_exprs(hir, &mut |e| {
        if let Some(name) = reassigned_builtin(e) {
            seen.push(name.to_string());
        }
    });
    seen
}

/// The name of every `var`/`global` declaration `f` reports.
fn decl_names(f: impl FnOnce(&mut dyn FnMut(&Stmt))) -> Vec<String> {
    let mut names = Vec::new();
    let mut push = |s: &Stmt| {
        if let Stmt::VarDecl(v) = s {
            names.push(v.name.clone());
        }
    };
    f(&mut push);
    names
}

/// One builtin reassignment in each executable position a file has, each
/// shadowing a *different* builtin so a walker that reaches five of the six
/// says which position it missed.
const REASSIGNED_BUILTINS: &str = "\
global g = (getType = 1)
function fun() { typeOf = 2 }
class Foo {
    static integer sf = (isNull = 3)
    integer inst = (clone = 4)
    constructor() { length = 5 }
    m() { count = 6 }
}
";

/// The regression #253 names: a builtin reassigned inside a class method, a
/// constructor, an instance-field initialiser, a static-field initialiser or
/// a global initialiser is code like any other, and the file-level
/// expression walk has to reach all of it. Three walkers in the Java backend
/// each enumerated a different subset of these roots.
#[test]
fn walk_file_exprs_reaches_a_builtin_reassignment_in_every_position() {
    let hir = lower(REASSIGNED_BUILTINS);
    let mut seen = reassigned_builtins(&hir);
    seen.sort();
    assert_eq!(
        seen,
        ["clone", "count", "getType", "isNull", "length", "typeOf"],
        "a reassignment position went unwalked"
    );
}

/// Lowering keeps a file-scope `global x = e` in the main block, as a
/// `VarDecl` with `is_global` set, and leaves `Global::init` empty — which is
/// why the fixture above reaches `getType` through `BodyRoot::Main`. The slot
/// is part of the IR all the same, so `BodyRoot::GlobalInit` has to walk it;
/// build that shape by hand, as the synthetic statements above do.
#[test]
fn a_populated_global_init_slot_is_walked() {
    let mut hir = lower("var p = (getType = 1)\n");
    let Stmt::VarDecl(v) = hir.main.remove(0) else {
        panic!("fixture is a var declaration")
    };
    assert!(hir.main.is_empty(), "nothing left in the main block");
    hir.defs.push(Def::Global(Global {
        name: "g".into(),
        ty: None,
        init: Some(v.init.expect("an initializer")),
        span: v.span,
    }));
    assert_eq!(reassigned_builtins(&hir), ["getType"]);
}

/// A file with one root of every kind. `bare` has no initialiser and so is
/// not a root; `sf`/`inst` are declared in that order and must be reported in
/// it.
const EVERY_ROOT: &str = "\
global bare
function fun() { return 1 }
class Foo {
    static integer sf = 1
    integer inst = 2
    constructor() { }
    m() { return 3 }
    n() { return 4 }
}
var top = 5
";

fn root_label(root: BodyRoot<'_>) -> String {
    match root {
        BodyRoot::Main(_) => "main".to_string(),
        BodyRoot::Function(f) => format!("function {}", f.name),
        BodyRoot::Method(c, m) => format!("method {}.{}", c.name, m.name),
        BodyRoot::Constructor(c, m) => format!("constructor {}/{}", c.name, m.params.len()),
        BodyRoot::FieldInit(c, f) => format!("field {}.{}", c.name, f.name),
        BodyRoot::GlobalInit(g) => format!("global {}", g.name),
    }
}

/// The enumeration order is part of the contract — downstream sets are built
/// from it and must be reproducible — so it is pinned here rather than left
/// to whatever `defs` iteration happens to produce.
#[test]
fn walk_file_bodies_enumerates_every_root_in_a_fixed_order() {
    let hir = lower(EVERY_ROOT);
    let mut roots = Vec::new();
    leek_hir::walk_file_bodies(&hir, &mut |root| roots.push(root_label(root)));
    assert_eq!(
        roots,
        [
            "main",
            "function fun",
            "field Foo.sf",
            "field Foo.inst",
            "constructor Foo/0",
            "method Foo.m",
            "method Foo.n",
        ],
        "a body-less `global bare` is not a root, and the order is fixed"
    );
}

/// The lambda boundary, at file scope: the binding is a statement of the
/// method body and both walks report it, but the lambda's own statements
/// belong to its own scope and only the `_deep` walk crosses into them.
#[test]
fn a_lambda_body_inside_a_method_is_deep_only() {
    let hir =
        lower("class Foo { m() { var x = function() { var inner = 1 return inner } return x } }\n");
    assert_eq!(
        decl_names(|f| leek_hir::walk_file_stmts(&hir, &mut |s| f(s))),
        ["x"],
        "the binding, never the lambda's own statements"
    );
    assert_eq!(
        decl_names(|f| leek_hir::walk_file_stmts_deep(&hir, &mut |s| f(s))),
        ["x", "inner"],
    );
}

/// A parameter default is a real expression position — code runs there — and
/// every hand-rolled walker in the Java backend skipped it.
#[test]
fn a_parameter_default_is_walked() {
    let hir = lower(
        "function fun(g = (getType = 1), h = function() { var fromDefault = 2 }) { return 0 }\n",
    );
    assert_eq!(
        reassigned_builtins(&hir),
        ["getType"],
        "the default expression itself"
    );
    assert_eq!(
        decl_names(|f| leek_hir::walk_file_stmts_deep(&hir, &mut |s| f(s))),
        ["fromDefault"],
        "a statement inside a lambda in a default"
    );
    assert!(
        decl_names(|f| leek_hir::walk_file_stmts(&hir, &mut |s| f(s))).is_empty(),
        "the shallow walk still stops at the lambda"
    );
}
