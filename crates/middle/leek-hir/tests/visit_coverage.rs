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

use leek_hir::{
    Expr, ExprKind, HirFile, ImportStmt, IncludeStmt, Stmt, walk_expr_children,
    walk_stmt_child_exprs, walk_stmt_child_stmts, walk_stmts_deep,
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
