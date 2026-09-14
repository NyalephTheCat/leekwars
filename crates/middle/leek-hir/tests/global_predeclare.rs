//! File-level `global`s are pre-declared before any body is lowered, so a
//! read or a write that appears above the `global` statement still resolves
//! to `NameRef::Global` instead of falling through to `NameRef::Builtin`
//! (#53) — including across files, where `lower_files` lowers every body
//! before any main block.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use leek_hir::{
    Block, Def, DefId, Expr, ExprKind, HirFile, LowerUnit, NameRef, Stmt, lower_file, lower_files,
};
use leek_parser::{ParseFeatures, ast::AstNode, ast::SourceFile, parse_with_features};
use leek_span::{FeatureFlags, SourceId};
use leek_syntax::{SyntaxNode, Version};

fn parse_file(src: &str, id: u32) -> (SourceFile, SourceId) {
    let source = SourceId::new(id).unwrap();
    let parsed = parse_with_features(src, source, Version::V4, ParseFeatures::default());
    let file = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parses");
    (file, source)
}

fn lower(src: &str) -> HirFile {
    let (file, source) = parse_file(src, 1);
    lower_file(&file, source).0
}

/// Lower an entry file that includes one other file, as the pipeline does.
fn lower_with_include(entry: &str, included: &str) -> HirFile {
    let (entry_ast, entry_src) = parse_file(entry, 1);
    let (inc_ast, inc_src) = parse_file(included, 2);
    let (entry_path, inc_path) = (Path::new("/main.leek"), Path::new("/lib.leek"));
    let mut resolved = BTreeMap::new();
    resolved.insert(
        (PathBuf::from(entry_path), "lib".to_string()),
        PathBuf::from(inc_path),
    );
    let unit = |ast, source, path| LowerUnit {
        ast,
        source,
        path,
        version: Version::V4,
    };
    lower_files(
        unit(&entry_ast, entry_src, entry_path),
        &[unit(&inc_ast, inc_src, inc_path)],
        Some(&resolved),
        FeatureFlags::none(),
    )
    .0
}

/// The `DefId` of the `Def::Global` named `name`.
fn global(hir: &HirFile, name: &str) -> DefId {
    let i = hir
        .defs
        .iter()
        .position(|d| matches!(d, Def::Global(_)) && d.name() == name)
        .unwrap_or_else(|| panic!("no global `{name}`"));
    DefId(u32::try_from(i).unwrap())
}

/// The body of the free function named `name`.
fn body<'a>(hir: &'a HirFile, name: &str) -> &'a Block {
    hir.defs
        .iter()
        .find_map(|d| match d {
            Def::Function(f) if f.name == name => f.body.as_ref(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no function `{name}`"))
}

/// The `DefId` `e` reads as a global, if it is a global name reference.
fn global_ref(e: &Expr) -> Option<DefId> {
    match &e.kind {
        ExprKind::Name(NameRef::Global(d)) => Some(*d),
        _ => None,
    }
}

#[test]
fn global_read_before_declaration_resolves_to_global() {
    let hir = lower("function f() { return G }\nglobal G = 1\n");
    let Stmt::Return(Some(e)) = &body(&hir, "f").stmts[0] else {
        panic!("expected `return G`, got {:?}", body(&hir, "f").stmts[0])
    };
    assert_eq!(global_ref(e), Some(global(&hir, "G")));
}

#[test]
fn global_written_before_declaration_resolves_to_global() {
    let hir = lower("function f() { G = 2 }\nglobal G = 1\n");
    let Stmt::Expr(Expr {
        kind: ExprKind::Binary(_, lhs, _),
        ..
    }) = &body(&hir, "f").stmts[0]
    else {
        panic!("expected `G = 2`, got {:?}", body(&hir, "f").stmts[0])
    };
    assert_eq!(global_ref(lhs), Some(global(&hir, "G")));
}

#[test]
fn global_declared_in_an_included_file_resolves_in_the_entry() {
    // `lower_files` lowers every body before any main block, so without
    // pre-declaration this write was always name-keyed.
    let hir = lower_with_include(
        "include(\"lib\")\nfunction f() { G = 2 }\n",
        "global G = 1\n",
    );
    let Stmt::Expr(Expr {
        kind: ExprKind::Binary(_, lhs, _),
        ..
    }) = &body(&hir, "f").stmts[0]
    else {
        panic!("expected `G = 2`, got {:?}", body(&hir, "f").stmts[0])
    };
    assert_eq!(global_ref(lhs), Some(global(&hir, "G")));
}

#[test]
fn a_global_declared_only_in_a_body_is_still_one_global() {
    // `declare_global` is idempotent, so both `global G` statements bind the
    // same def — one declared in a function body, one in `main`.
    let hir = lower("function f() { global G = 2 }\nglobal G = 1\n");
    let g = global(&hir, "G");
    assert_eq!(
        hir.defs
            .iter()
            .filter(|d| matches!(d, Def::Global(_)) && d.name() == "G")
            .count(),
        1
    );
    let Stmt::VarDecl(v) = &body(&hir, "f").stmts[0] else {
        panic!(
            "expected `global G = 2`, got {:?}",
            body(&hir, "f").stmts[0]
        )
    };
    assert!(v.is_global);
    assert_eq!(v.def, g);
}
