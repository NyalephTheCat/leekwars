//! Differential test: the resolver and the HIR lowerer must agree on
//! what each bare name refers to.
//!
//! The two passes resolve names independently — `Resolver` into a
//! [`ResolveTable`] the LSP navigates by, `Lowerer` into a
//! [`NameRef`] MIR and codegen read. When they disagree the compiler
//! navigates one program and emits another; #118's miscompile was
//! exactly that shape. Nothing compared them before (#194).
//!
//! The comparison is by source offset, which both sides carry:
//!
//! - a name the lowerer bound to a declaration (`Local`, `Global`,
//!   `Function`, `Class`) must have a resolver reference at the same
//!   offset;
//! - a name the lowerer left as `Builtin` or `Unresolved` must have
//!   none, and a `Builtin` must be a name the shared builtin table
//!   actually knows.

use leek_hir::visit::{walk_expr_children, walk_stmt_child_exprs, walk_stmt_child_stmts};
use leek_hir::{Block, Def, Expr, ExprKind, LambdaBody, NameRef, Stmt, lower_file};
use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::parse;
use leek_resolver::{Options, resolve_collecting};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn source_id() -> SourceId {
    SourceId::new(1).unwrap()
}

/// Every `ExprKind::Name` in the program, paired with the source offset
/// of the expression that produced it.
fn name_refs(hir: &leek_hir::HirFile) -> Vec<(u32, NameRef)> {
    let mut out = Vec::new();
    for stmt in &hir.main {
        collect_stmt(stmt, &mut out);
    }
    for def in &hir.defs {
        match def {
            Def::Function(f) => {
                if let Some(body) = &f.body {
                    collect_block(body, &mut out);
                }
            }
            Def::Global(g) => {
                if let Some(init) = &g.init {
                    collect_expr(init, &mut out);
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|(offset, _)| *offset);
    out
}

fn collect_block(block: &Block, out: &mut Vec<(u32, NameRef)>) {
    for stmt in &block.stmts {
        collect_stmt(stmt, out);
    }
}

fn collect_stmt(stmt: &Stmt, out: &mut Vec<(u32, NameRef)>) {
    walk_stmt_child_exprs(stmt, &mut |e| collect_expr(e, out));
    walk_stmt_child_stmts(stmt, &mut |s| collect_stmt(s, out));
}

fn collect_expr(expr: &Expr, out: &mut Vec<(u32, NameRef)>) {
    if let ExprKind::Name(nr) = &expr.kind {
        out.push((expr.span.start, nr.clone()));
    }
    // `walk_expr_children` treats a lambda as a leaf, by design; its body
    // has its own scope and is exactly where capture resolution can
    // drift, so descend explicitly.
    if let ExprKind::Lambda(l) = &expr.kind {
        for param in &l.params {
            if let Some(default) = &param.default {
                collect_expr(default, out);
            }
        }
        match &l.body {
            LambdaBody::Expr(e) => collect_expr(e, out),
            LambdaBody::Block(b) => collect_block(b, out),
        }
    }
    walk_expr_children(expr, &mut |e| collect_expr(e, out));
}

/// Lower and resolve `text`, then assert the two passes agree on every
/// bare name.
fn assert_agreement(text: &str) {
    let ast = SourceFile::cast(SyntaxNode::new_root(
        parse(text, source_id(), Version::V4).green,
    ))
    .expect("source file parses");

    let (hir, _) = lower_file(&ast, source_id());
    let resolved = resolve_collecting(&ast, source_id(), Version::V4, Options::default());

    for (offset, name_ref) in name_refs(&hir) {
        let recorded = resolved
            .table
            .references
            .iter()
            .any(|r| r.name_offset == offset);
        match &name_ref {
            NameRef::Local(_) | NameRef::Global(_) | NameRef::Function(_) | NameRef::Class(_) => {
                assert!(
                    recorded,
                    "HIR bound the name at offset {offset} ({name_ref:?}) but the resolver \
                     recorded no reference there\nsource: {text:?}\nrefs: {:?}",
                    resolved.table.references
                );
            }
            NameRef::Builtin(name) => {
                assert!(
                    leek_resolver::builtins::is_builtin_name(name),
                    "HIR called `{name}` a builtin but the shared table does not know it",
                );
                assert!(
                    !recorded,
                    "HIR called the name at offset {offset} a builtin, but the resolver bound \
                     it to a declaration\nsource: {text:?}",
                );
            }
            NameRef::Unresolved(name) => {
                assert!(
                    !recorded,
                    "HIR left `{name}` at offset {offset} unresolved, but the resolver bound \
                     it to a declaration\nsource: {text:?}",
                );
            }
            NameRef::This | NameRef::Super | NameRef::Class_ => {}
        }
    }
}

#[test]
fn top_level_locals_agree() {
    assert_agreement("var a = 1\nvar b = a + 1\nreturn b\n");
}

#[test]
fn globals_read_after_their_declaration_agree() {
    assert_agreement("global g = 3\nfunction f() { return g }\nreturn f()\n");
}

/// The one divergence this suite knows about, written down rather than
/// hidden: the HIR lowerer pre-declares every top-level `global` before
/// it lowers anything, so a function body above the declaration binds
/// it; the resolver declares globals in source order
/// (`declare_top_stmt` is deliberately a no-op) and records no
/// reference there. Filed as #53 (SEM-01) and #117 (SEM-03); fixing it
/// is a resolver behaviour change of its own, not part of #118.
///
/// If this test starts failing because the resolver now hoists globals,
/// delete it and fold the source into
/// [`globals_read_after_their_declaration_agree`].
#[test]
fn a_global_read_before_its_declaration_still_disagrees() {
    let text = "function f() { return g }\nglobal g = 3\nreturn f()\n";
    let ast = SourceFile::cast(SyntaxNode::new_root(
        parse(text, source_id(), Version::V4).green,
    ))
    .expect("source file parses");
    let (hir, _) = lower_file(&ast, source_id());
    let resolved = resolve_collecting(&ast, source_id(), Version::V4, Options::default());

    // `g` at offset 22, inside `f`'s body.
    let read = name_refs(&hir)
        .into_iter()
        .find(|(offset, _)| *offset == 22)
        .expect("the read of `g` is lowered");
    assert!(
        matches!(read.1, NameRef::Global(_)),
        "HIR binds the forward read: {:?}",
        read.1
    );
    assert!(
        !resolved
            .table
            .references
            .iter()
            .any(|r| r.name_offset == 22),
        "the resolver does not (yet) hoist globals: {:?}",
        resolved.table.references
    );
}

#[test]
fn function_and_class_names_agree() {
    assert_agreement(
        "function helper(x) { return x + 1 }\nclass Cat { }\n\
         var c = new Cat()\nreturn helper(1)\n",
    );
}

#[test]
fn params_and_block_scopes_agree() {
    assert_agreement(
        "function f(a, b) {\n  var c = a + b\n  if (c > 0) { var d = c\n return d }\n  return c\n}\n\
         return f(1, 2)\n",
    );
}

#[test]
fn loop_bindings_agree() {
    assert_agreement(
        "var xs = [1, 2, 3]\nvar total = 0\nfor (var i = 0; i < 3; i++) { total = total + i }\n\
         for (var v in xs) { total = total + v }\nreturn total\n",
    );
}

#[test]
fn lambda_captures_agree() {
    assert_agreement("var k = 2\nvar f = x -> x * k\nreturn f(3)\n");
}

#[test]
fn builtins_and_unknown_names_agree() {
    // `length` is a real builtin; `notAThing` is nothing at all. Neither
    // may carry a resolver reference.
    assert_agreement("var s = length([1, 2])\nvar t = notAThing\nreturn s\n");
}

#[test]
fn a_user_declaration_shadowing_a_builtin_agrees() {
    // Both passes must prefer the user's binding over the stdlib name.
    assert_agreement("var search = 1\nreturn search + 1\n");
}
