//! End-to-end include-graph + multi-file HIR-lowering tests.
//!
//! Sets up an in-memory `MemFolder`, runs the include walker, and
//! checks the merged `HirFile` carries every file's top-level
//! declarations plus the entry's main block with `Stmt::Include`
//! sites spliced in.

use std::path::{Path, PathBuf};

use leek_hir::{Def, ExprKind, Literal, LowerUnit, Stmt, lower_files};
use leek_parser::ast::{AstNode, SourceFile};
use leek_parser::parse;
use leek_resolver::folder::MemFolder;
use leek_resolver::include_graph::{ResolvedFile, build_include_graph};
use leek_span::{FeatureFlags, SourceId};
use leek_syntax::{SyntaxNode, Version};

struct Compiled {
    hir: leek_hir::HirFile,
    diagnostics: Vec<leek_diagnostics::Diagnostic>,
}

fn compile(entry: &str, files: &[(&str, &str)]) -> Compiled {
    let mut folder = MemFolder::new();
    for (p, t) in files {
        folder.insert(*p, *t);
    }
    let entry_text = files
        .iter()
        .find(|(p, _)| *p == entry)
        .map(|(_, t)| (*t).to_string())
        .expect("entry exists in fixture");

    compile_at(entry, Version::V4, &entry_text, &folder)
}

fn compile_at(entry: &str, version: Version, entry_text: &str, folder: &MemFolder) -> Compiled {
    let mut next: u32 = 1;
    let graph = build_include_graph(Path::new(entry), entry_text, version, folder, |_| {
        let id = SourceId::new(next).unwrap();
        next += 1;
        id
    });

    // Parse each ResolvedFile into an AST. Keep them around so the
    // lowerer can borrow.
    struct ParsedFile {
        path: PathBuf,
        source: SourceId,
        ast: SourceFile,
        version: Version,
    }
    let mut parsed: Vec<ParsedFile> = graph
        .files
        .iter()
        .map(|f: &ResolvedFile| {
            let parsed = parse(&f.text, f.source, f.version);
            let root = SyntaxNode::new_root(parsed.green);
            ParsedFile {
                path: f.path.clone(),
                source: f.source,
                ast: SourceFile::cast(root).expect("source file parses"),
                version: f.version,
            }
        })
        .collect();

    // Find the entry file: it's last in topological order, by
    // construction of `build_include_graph`.
    let entry_parsed = parsed.pop().expect("at least one file (entry)");

    // The remaining `parsed` slice is the includes, leaves-first.
    let includes: Vec<LowerUnit<'_>> = parsed
        .iter()
        .map(|p| LowerUnit {
            ast: &p.ast,
            source: p.source,
            path: &p.path,
            version: p.version,
        })
        .collect();
    let entry_unit = LowerUnit {
        ast: &entry_parsed.ast,
        source: entry_parsed.source,
        path: &entry_parsed.path,
        version: entry_parsed.version,
    };

    let (hir, diags) = lower_files(
        entry_unit,
        &includes,
        Some(&graph.resolved),
        FeatureFlags::none(),
    );
    let mut diagnostics = graph.diagnostics;
    diagnostics.extend(diags);
    Compiled { hir, diagnostics }
}

/// Every string literal value in the merged main block's var inits.
fn string_inits(hir: &leek_hir::HirFile) -> Vec<(String, String)> {
    hir.main
        .iter()
        .filter_map(|s| match s {
            Stmt::VarDecl(v) => match v.init.as_ref().map(|e| &e.kind) {
                Some(ExprKind::Literal(Literal::String(s))) => Some((v.name.clone(), s.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn included_files_lower_at_their_own_version() {
    // v1 keeps the backslash of `\"` inside a double-quoted string
    // (`length("a\"b") == 4`), v2+ unescapes it. Before the fix every
    // file of an include program was lowered at v4.
    let mut folder = MemFolder::new();
    let entry_text = "var e = \"a\\\"b\"\ninclude(\"inherits\")\ninclude(\"modern\")\n";
    folder.insert("/main.leek", entry_text);
    // No pragma: inherits the entry's v1.
    folder.insert("/inherits.leek", "var i = \"a\\\"b\"\n");
    // Explicit pragma: lowered at v4 even inside a v1 program.
    folder.insert("/modern.leek", "// @version:4\nvar m = \"a\\\"b\"\n");

    let c = compile_at("/main.leek", Version::V1, entry_text, &folder);
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let inits = string_inits(&c.hir);
    let get = |name: &str| {
        inits.iter().find(|(n, _)| n == name).map_or_else(
            || panic!("{name} missing from {inits:?}"),
            |(_, s)| s.clone(),
        )
    };
    assert_eq!(get("e"), "a\\\"b", "entry lowers at v1");
    assert_eq!(get("i"), "a\\\"b", "pragma-less include inherits v1");
    assert_eq!(get("m"), "a\"b", "pragma include lowers at its own v4");
}

fn fn_names(hir: &leek_hir::HirFile) -> Vec<String> {
    hir.defs
        .iter()
        .filter_map(|d| match d {
            Def::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect()
}

fn class_names(hir: &leek_hir::HirFile) -> Vec<String> {
    hir.defs
        .iter()
        .filter_map(|d| match d {
            Def::Class(c) => Some(c.name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn included_functions_visible_in_entry() {
    let c = compile(
        "/main.leek",
        &[
            ("/main.leek", "include(\"util\")\nfunction main() {}"),
            ("/util.leek", "function helper() {}"),
        ],
    );
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let fns = fn_names(&c.hir);
    assert!(fns.contains(&"helper".to_string()), "fns: {fns:?}");
    assert!(fns.contains(&"main".to_string()), "fns: {fns:?}");
}

#[test]
fn included_class_visible_in_entry() {
    let c = compile(
        "/main.leek",
        &[
            ("/main.leek", "include(\"models\")\nvar x = 1\n"),
            ("/models.leek", "class Cat {}\nclass Dog {}\n"),
        ],
    );
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let classes = class_names(&c.hir);
    assert!(classes.contains(&"Cat".to_string()), "{classes:?}");
    assert!(classes.contains(&"Dog".to_string()), "{classes:?}");
}

#[test]
fn included_main_statements_splice_at_include_site() {
    let c = compile(
        "/main.leek",
        &[
            (
                "/main.leek",
                "var before = 1\ninclude(\"side\")\nvar after = 2\n",
            ),
            ("/side.leek", "var injected = 99\n"),
        ],
    );
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    // The merged main should have three VarDecls in order:
    // before, injected, after.
    let names: Vec<String> = c
        .hir
        .main
        .iter()
        .filter_map(|s| match s {
            Stmt::VarDecl(v) => Some(v.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["before", "injected", "after"]);
}

#[test]
fn missing_include_reports_diagnostic() {
    let c = compile("/main.leek", &[("/main.leek", "include(\"ghost\")\n")]);
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == leek_diagnostics::codes::INCLUDE_NOT_FOUND),
        "expected INCLUDE_NOT_FOUND, got {:?}",
        c.diagnostics
    );
}

#[test]
fn circular_include_reports_diagnostic() {
    let c = compile(
        "/a.leek",
        &[
            ("/a.leek", "include(\"b\")\n"),
            ("/b.leek", "include(\"a\")\n"),
        ],
    );
    assert!(
        c.diagnostics
            .iter()
            .any(|d| d.code == leek_diagnostics::codes::CIRCULAR_INCLUDE),
        "expected CIRCULAR_INCLUDE, got {:?}",
        c.diagnostics
    );
}

#[test]
fn diamond_include_dedupes_main_splice() {
    let c = compile(
        "/main.leek",
        &[
            ("/main.leek", "include(\"a\")\ninclude(\"b\")\n"),
            ("/a.leek", "var shared = 1\n"),
            ("/b.leek", "include(\"a\")\nvar bonus = 2\n"),
        ],
    );
    assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    let names: Vec<String> = c
        .hir
        .main
        .iter()
        .filter_map(|s| match s {
            Stmt::VarDecl(v) => Some(v.name.clone()),
            _ => None,
        })
        .collect();
    // `shared` should appear exactly once (per logical-merge dedupe);
    // `bonus` comes from `b`. Final order: shared (via a), bonus
    // (via b's main after b's own include of a is deduped).
    assert_eq!(names, ["shared", "bonus"]);
}
