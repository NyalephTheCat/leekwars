//! End-to-end whole-program lowering: prove that include resolution
//! slots between parse and HIR lowering and switches the lowerer to the
//! multi-file path automatically.

use std::sync::Arc;

use leek_db::queries::{OptLevel, lower_hir_query, lower_program};
use leek_hir::{Def, ExprKind, Literal, Stmt};
use leek_project::Input;
use leek_span::SourceId;

/// Lower `files` as one program reached from `entry_path`.
fn lower_closure(entry_path: &str, files: &[(&str, &str)]) -> Arc<leek_hir::HirFile> {
    lower_closure_at(entry_path, files, 4)
}

fn lower_closure_at(
    entry_path: &str,
    files: &[(&str, &str)],
    default_version: u8,
) -> Arc<leek_hir::HirFile> {
    let mut db = leek_db::LeekDb::default();
    let (set, entry) = leek_db::testing::workspace(
        &mut db,
        entry_path,
        files,
        (default_version, false),
        leek_span::FeatureFlags::from_env().to_bits(),
    );
    let version = leek_syntax::query::version_from_byte(default_version);
    lower_program(&db, set, entry, version, OptLevel::O0).hir
}

/// Lower one file on its own, with no include resolution at all.
fn lower_alone(input: &Input) -> Arc<leek_hir::HirFile> {
    let db = leek_db::LeekDb::default();
    let file = leek_db::input_file(&db, String::new(), input);
    lower_hir_query(&db, file).hir
}

#[test]
fn whole_program_lowering_merges_include_decls() {
    let hir = lower_closure(
        "/main.leek",
        &[
            ("/main.leek", "include(\"util\")\nfunction main() {}"),
            ("/util.leek", "function helper() {}"),
        ],
    );
    let fns: Vec<_> = hir
        .defs
        .iter()
        .filter_map(|d| match d {
            Def::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    assert!(fns.contains(&"helper".to_string()), "got {fns:?}");
    assert!(fns.contains(&"main".to_string()), "got {fns:?}");
}

#[test]
fn whole_program_lowering_splices_main_at_include_site() {
    let hir = lower_closure(
        "/main.leek",
        &[
            (
                "/main.leek",
                "var before = 1\ninclude(\"side\")\nvar after = 2\n",
            ),
            ("/side.leek", "var injected = 99\n"),
        ],
    );
    let names: Vec<String> = hir
        .main
        .iter()
        .filter_map(|s| match s {
            Stmt::VarDecl(v) => Some(v.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["before", "injected", "after"]);
}

/// String-literal values of `var` inits in the lowered main block.
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

fn input(text: &str, version_byte: u8) -> Input {
    Input {
        source: SourceId::new(1).unwrap(),
        text: text.to_string().into(),
        version_byte,
        strict: false,
        flags: leek_span::FeatureFlags::none(),
    }
}

#[test]
fn single_file_lowering_uses_input_version_not_pragma() {
    // `Input::version_byte` is the settled version (a driver already
    // applied override > pragma > default). HIR lowering used to re-read
    // the pragma, so `leekc --version-pragma 2` on a `@version:1` file was
    // lexed/parsed at v2 but lowered (v1 string escapes) at v1.
    let hir = lower_alone(&input("// @version:1\nvar s = \"a\\\"b\"\n", 2));
    assert_eq!(string_inits(&hir), [("s".to_string(), "a\"b".to_string())]);

    let hir = lower_alone(&input("// @version:4\nvar s = \"a\\\"b\"\n", 1));
    assert_eq!(
        string_inits(&hir),
        [("s".to_string(), "a\\\"b".to_string())]
    );
}

#[test]
fn include_pipeline_lowers_each_file_at_its_version() {
    // A v1 program: the pragma-less include inherits v1 (lexed, parsed and
    // lowered at v1), the pragma'd include keeps its own v4. Previously the
    // include graph defaulted pragma-less files to v4 and `lower_files`
    // lowered every file at v4.
    let entry = "var e = \"a\\\"b\"\ninclude(\"inherits\")\ninclude(\"modern\")\n";
    let hir = lower_closure_at(
        "/main.leek",
        &[
            ("/main.leek", entry),
            ("/inherits.leek", "var i = \"a\\\"b\"\n"),
            ("/modern.leek", "// @version:4\nvar m = \"a\\\"b\"\n"),
        ],
        1,
    );
    assert_eq!(
        string_inits(&hir),
        [
            ("e".to_string(), "a\\\"b".to_string()),
            ("i".to_string(), "a\\\"b".to_string()),
            ("m".to_string(), "a\"b".to_string()),
        ]
    );
}

#[test]
fn lowering_one_file_alone_stays_single_file() {
    // With no include closure the single-file lower path runs; the
    // `include(...)` call is preserved as a `Stmt::Include` in main.
    let input = Input {
        source: SourceId::new(1).unwrap(),
        text: "include(\"ghost\")\nfunction main() {}\n"
            .to_string()
            .into(),
        version_byte: 4,
        strict: false,
        flags: leek_span::FeatureFlags::from_env(),
    };
    let hir = lower_alone(&input);
    let has_main = hir
        .defs
        .iter()
        .any(|d| matches!(d, Def::Function(f) if f.name == "main"));
    assert!(has_main, "main fn present");
    let has_include_stmt = hir.main.iter().any(|s| matches!(s, Stmt::Include(_)));
    assert!(has_include_stmt, "single-file path preserves Stmt::Include");
}
