//! End-to-end Pipeline composition: prove that include resolution slots
//! between parse and HIR lowering and switches the lowerer to the
//! multi-file path automatically.

use std::path::PathBuf;
use std::sync::Arc;

use leek_hir::pipeline::HirArtifact;
use leek_hir::{Def, ExprKind, Literal, Stmt};
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, pipeline_hir_from_parse, pipeline_hir_with_includes};
use leek_resolver::folder::MemFolder;
use leek_resolver::pipeline::ResolveIncludes;
use leek_span::SourceId;

fn run_pipeline(entry_path: &str, files: &[(&str, &str)]) -> Arc<leek_hir::HirFile> {
    let mut folder = MemFolder::new();
    for (p, t) in files {
        folder.insert(*p, *t);
    }
    let entry_text = files
        .iter()
        .find(|(p, _)| *p == entry_path)
        .map(|(_, t)| (*t).to_string())
        .expect("entry exists in fixture");

    let entry = SourceId::new(1).unwrap();
    let input = Input {
        source: entry,
        text: entry_text.into(),
        version_byte: 4,
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };

    let resolve_includes = ResolveIncludes::with_counter(
        Arc::new(folder),
        PathBuf::from(entry_path),
        /* start = */ 2,
    );

    let params = RecipeParams::permissive();
    let pipeline = pipeline_hir_with_includes(Box::new(resolve_includes), &params).expect("recipe");
    let run = pipeline.run(input);

    run.get::<HirArtifact>()
        .expect("HirArtifact present")
        .0
        .clone()
}

#[test]
fn step_pipeline_merges_include_decls() {
    let hir = run_pipeline(
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
fn step_pipeline_splices_main_at_include_site() {
    let hir = run_pipeline(
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
        flags: leek_pipeline::FeatureFlags::none(),
    }
}

#[test]
fn single_file_lowering_uses_input_version_not_pragma() {
    // `Input::version_byte` is the settled version (a driver already
    // applied override > pragma > default). HIR lowering used to re-read
    // the pragma, so `leekc --version-pragma 2` on a `@version:1` file was
    // lexed/parsed at v2 but lowered (v1 string escapes) at v1.
    let pipeline = pipeline_hir_from_parse(&RecipeParams::permissive()).expect("recipe");
    let run = pipeline.run(input("// @version:1\nvar s = \"a\\\"b\"\n", 2));
    let hir = run.get::<HirArtifact>().expect("HirArtifact").0.clone();
    assert_eq!(string_inits(&hir), [("s".to_string(), "a\"b".to_string())]);

    let run = pipeline.run(input("// @version:4\nvar s = \"a\\\"b\"\n", 1));
    let hir = run.get::<HirArtifact>().expect("HirArtifact").0.clone();
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
    let mut folder = MemFolder::new();
    let entry = "var e = \"a\\\"b\"\ninclude(\"inherits\")\ninclude(\"modern\")\n";
    folder.insert("/main.leek", entry);
    folder.insert("/inherits.leek", "var i = \"a\\\"b\"\n");
    folder.insert("/modern.leek", "// @version:4\nvar m = \"a\\\"b\"\n");
    let resolve_includes =
        ResolveIncludes::with_counter(Arc::new(folder), PathBuf::from("/main.leek"), 2);
    let pipeline =
        pipeline_hir_with_includes(Box::new(resolve_includes), &RecipeParams::permissive())
            .expect("recipe");
    let run = pipeline.run(input(entry, 1));
    let hir = run.get::<HirArtifact>().expect("HirArtifact").0.clone();
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
fn step_pipeline_without_resolveincludes_stays_single_file() {
    // Without `ResolveIncludes` the single-file lower path runs; the
    // `include(...)` call is preserved as a `Stmt::Include` in main.
    let input = Input {
        source: SourceId::new(1).unwrap(),
        text: "include(\"ghost\")\nfunction main() {}\n"
            .to_string()
            .into(),
        version_byte: 4,
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let pipeline = pipeline_hir_from_parse(&RecipeParams::permissive()).expect("recipe");
    let run = pipeline.run(input);
    let hir = run.get::<HirArtifact>().expect("HirArtifact").0.clone();
    let has_main = hir
        .defs
        .iter()
        .any(|d| matches!(d, Def::Function(f) if f.name == "main"));
    assert!(has_main, "main fn present");
    let has_include_stmt = hir.main.iter().any(|s| matches!(s, Stmt::Include(_)));
    assert!(has_include_stmt, "single-file path preserves Stmt::Include");
}
