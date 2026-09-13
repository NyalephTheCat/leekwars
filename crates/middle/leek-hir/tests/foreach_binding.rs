//! A bare foreach binding (`for (x in arr)`, no `var`) stores into whatever
//! `x` names at that point — resolved like an assignment l-value — never into
//! an arbitrary `DefId` (#86).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use leek_hir::{
    Def, DefId, ExprKind, ForeachStmt, HirFile, LambdaBody, LowerUnit, NameRef, Stmt, lower_file,
    lower_files,
};
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::{FeatureFlags, SourceId};
use leek_syntax::{SyntaxNode, Version};

fn parse_file(src: &str, id: u32) -> (SourceFile, SourceId) {
    let source = SourceId::new(id).unwrap();
    let parsed = parse(src, source, Version::V4);
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parses");
    (file, source)
}

fn lower(src: &str) -> HirFile {
    let (file, source) = parse_file(src, 1);
    lower_file(&file, source).0
}

/// The `DefId` of the first def named `name` matching `pred`.
fn def_named(hir: &HirFile, name: &str, pred: impl Fn(&Def) -> bool) -> DefId {
    let i = hir
        .defs
        .iter()
        .position(|d| d.name() == name && pred(d))
        .unwrap_or_else(|| panic!("no def `{name}`"));
    DefId(u32::try_from(i).unwrap())
}

fn global(hir: &HirFile, name: &str) -> DefId {
    def_named(hir, name, |d| matches!(d, Def::Global(_)))
}

fn local(hir: &HirFile, name: &str) -> DefId {
    def_named(hir, name, |d| matches!(d, Def::Local(_)))
}

/// The first foreach in `stmts`, searching nested statements.
fn find_foreach(stmts: &[Stmt]) -> Option<&ForeachStmt> {
    stmts.iter().find_map(|s| match s {
        Stmt::Foreach(fe) => Some(fe),
        Stmt::Block(b) => find_foreach(&b.stmts),
        Stmt::VarDecl(v) => match v.init.as_ref().map(|e| &e.kind) {
            Some(ExprKind::Lambda(l)) => match &l.body {
                LambdaBody::Block(b) => find_foreach(&b.stmts),
                LambdaBody::Expr(_) => None,
            },
            _ => None,
        },
        _ => None,
    })
}

fn main_foreach(hir: &HirFile) -> &ForeachStmt {
    find_foreach(&hir.main).expect("foreach in main")
}

fn function_foreach<'a>(hir: &'a HirFile, name: &str) -> &'a ForeachStmt {
    hir.defs
        .iter()
        .find_map(|d| match d {
            Def::Function(f) if f.name == name => find_foreach(&f.body.as_ref()?.stmts),
            _ => None,
        })
        .expect("foreach in function")
}

#[test]
fn declared_binding_is_a_fresh_local() {
    let hir = lower("var x = 0\nfor (var x in [1]) {}\n");
    let fe = main_foreach(&hir);
    let d = fe.value.local_def().expect("local");
    assert!(fe.value.is_new);
    assert_ne!(d, local(&hir, "x"), "not the outer `x`");
}

#[test]
fn bare_binding_over_a_global() {
    let hir = lower("global g = 0\nfor (g in [1, 2]) {}\n");
    let fe = main_foreach(&hir);
    assert!(!fe.value.is_new);
    assert_eq!(fe.value.global_def(), Some(global(&hir, "g")));
}

#[test]
fn bare_key_and_value_over_globals() {
    let hir = lower("global k = 0\nglobal v = 0\nfor (k : v in [1: 2]) {}\n");
    let fe = main_foreach(&hir);
    let key = fe.key.as_ref().expect("key");
    assert_eq!(key.global_def(), Some(global(&hir, "k")));
    assert_eq!(fe.value.global_def(), Some(global(&hir, "v")));
}

#[test]
fn bare_binding_over_a_global_from_a_function() {
    let hir = lower("global g = 0\nfunction f() { for (g in [1]) {} }\n");
    let fe = function_foreach(&hir, "f");
    assert_eq!(fe.value.global_def(), Some(global(&hir, "g")));
}

#[test]
fn bare_binding_over_a_function_scope_local() {
    let hir = lower("function f() { var x = 0\nfor (x in [1]) {} }\n");
    let fe = function_foreach(&hir, "f");
    assert_eq!(fe.value.local_def(), Some(local(&hir, "x")));
}

#[test]
fn bare_binding_over_a_captured_local() {
    let hir = lower("var x = 0\nvar f = function() { for (x in [1]) {} }\n");
    let fe = main_foreach(&hir);
    assert_eq!(fe.value.local_def(), Some(local(&hir, "x")));
}

#[test]
fn bare_binding_over_a_class_field() {
    let hir = lower("class A { x\nm() { for (x in [1]) {} } }\n");
    let fe = hir
        .defs
        .iter()
        .find_map(|d| match d {
            Def::Class(c) => find_foreach(&c.methods.first()?.body.as_ref()?.stmts),
            _ => None,
        })
        .expect("foreach in method");
    let ExprKind::Field(base, name, _) = &fe.value.target.kind else {
        panic!("expected `this.x`, got {:?}", fe.value.target.kind)
    };
    assert_eq!(name, "x");
    assert!(matches!(base.kind, ExprKind::Name(NameRef::This)));
}

#[test]
fn bare_binding_over_a_global_declared_later_is_name_keyed() {
    // `f` is lowered before `global g` is declared: the write is by name,
    // exactly like `g = 1` would be there — not `DefId(0)`.
    let hir = lower("function f() { for (g in [1]) {} }\nglobal g = 0\n");
    let fe = function_foreach(&hir, "f");
    assert_eq!(
        fe.value.target.kind,
        ExprKind::Name(NameRef::Builtin("g".into()))
    );
}

#[test]
fn bare_binding_over_an_include_level_global() {
    let (entry, entry_src) = parse_file(
        "include(\"lib\")\nfor (g in [1]) {}\nfunction f() { for (g in [2]) {} }\n",
        1,
    );
    let (lib, lib_src) = parse_file("global g = 0\n", 2);
    let (entry_path, lib_path) = (Path::new("/main.leek"), Path::new("/lib.leek"));
    let mut resolved = BTreeMap::new();
    resolved.insert(
        (PathBuf::from(entry_path), "lib".to_string()),
        PathBuf::from(lib_path),
    );
    let unit = |ast, source, path| LowerUnit {
        ast,
        source,
        path,
        version: Version::V4,
    };
    let (hir, _) = lower_files(
        unit(&entry, entry_src, entry_path),
        &[unit(&lib, lib_src, lib_path)],
        Some(&resolved),
        FeatureFlags::none(),
    );
    // The entry's main block is lowered after the included one declared `g`.
    assert_eq!(
        main_foreach(&hir).value.global_def(),
        Some(global(&hir, "g"))
    );
    // Function bodies are lowered before every main block, so `g` is still
    // undeclared there: the binding is the name-keyed global.
    assert_eq!(
        function_foreach(&hir, "f").value.target.kind,
        ExprKind::Name(NameRef::Builtin("g".into()))
    );
}
