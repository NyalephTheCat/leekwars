//! End-to-end Pipeline composition: prove that include resolution slots
//! between parse and HIR lowering and switches the lowerer to the
//! multi-file path automatically.

use std::path::PathBuf;
use std::sync::Arc;

use leek_hir::pipeline::HirArtifact;
use leek_hir::{Def, Stmt};
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
