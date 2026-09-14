//! Backend selection and output-path resolution.
//!
//! These are the decisions `miku build` / `miku run` make before any
//! compilation happens: which backend the manifest and `--backend` add up
//! to, and where the chosen backend writes. Both are string-and-path
//! plumbing with no test surface of their own — a wrong answer here sends
//! artifacts to a directory nobody looks in, or rejects a backend the
//! manifest clearly asked for.

use std::path::{Path, PathBuf};

use leek_backends::{
    LINKED, is_linked, java_clean_mode, pick_java_out_dir, pick_out_dir, resolve_backend,
    resolve_run_backend, version_from_byte,
};
use leek_manifest::{BackendKind, BackendSettings, JavaMode, Manifest};
use leek_project::Project;

// ---- fixtures ----

/// Load a `Miku.toml` body without touching the filesystem: the project
/// root is a path that need not exist, because every helper under test
/// only ever *joins* against it.
fn project_at(root: &str, toml: &str) -> Project {
    let (manifest, warnings) = leek_manifest::load_str(toml).expect("manifest parses");
    Project::from_load(leek_manifest::ManifestLoad {
        manifest,
        root: PathBuf::from(root),
        path: PathBuf::from(root).join("Miku.toml"),
        text: toml.to_string(),
        warnings,
    })
}

const MINIMAL: &str = "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n";

fn manifest_of(toml: &str) -> Manifest {
    leek_manifest::load_str(toml).expect("manifest parses").0
}

/// Every backend kind. The `match` is what keeps this honest: adding a
/// variant to [`BackendKind`] makes this function fail to compile, and
/// with it every table below.
fn position(kind: BackendKind) -> usize {
    match kind {
        BackendKind::Java => 0,
        BackendKind::Jar => 1,
        BackendKind::Native => 2,
        BackendKind::Wasm => 3,
        BackendKind::LeekScript => 4,
    }
}

const ALL_KINDS: [BackendKind; 5] = [
    BackendKind::Java,
    BackendKind::Jar,
    BackendKind::Native,
    BackendKind::Wasm,
    BackendKind::LeekScript,
];

#[test]
fn all_kinds_lists_every_variant_in_order() {
    for (i, kind) in ALL_KINDS.iter().enumerate() {
        assert_eq!(position(*kind), i, "{kind:?} out of place in ALL_KINDS");
    }
}

// ---- resolve_backend ----

#[test]
fn a_cli_backend_name_round_trips_through_every_kind() {
    let manifest = manifest_of(MINIMAL);
    for kind in ALL_KINDS {
        // `as_str` and `parse` are each other's inverse, and
        // `resolve_backend` is the only place the CLI string meets them.
        let resolved = resolve_backend(&manifest, Some(kind.as_str()))
            .unwrap_or_else(|e| panic!("resolving {kind:?}: {e}"));
        assert_eq!(resolved, kind);
    }
}

#[test]
fn an_unknown_cli_backend_names_the_alternatives() {
    let manifest = manifest_of(MINIMAL);
    let err = resolve_backend(&manifest, Some("jarr"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("jarr"), "{err}");
    for kind in ALL_KINDS {
        assert!(
            err.contains(kind.as_str()),
            "error should list `{}`: {err}",
            kind.as_str()
        );
    }
}

#[test]
fn a_manifest_with_no_backend_table_says_how_to_pick_one() {
    let err = resolve_backend(&manifest_of(MINIMAL), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("default = true"), "{err}");
    assert!(err.contains("--backend"), "{err}");
}

#[test]
fn the_cli_override_beats_the_manifest_default() {
    let manifest = manifest_of(
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n[backend.java]\nenable = true\ndefault = true\n",
    );
    assert_eq!(resolve_backend(&manifest, None).unwrap(), BackendKind::Java);
    assert_eq!(
        resolve_backend(&manifest, Some("native")).unwrap(),
        BackendKind::Native,
        "--backend must win over `[backend.java].default`"
    );
}

#[test]
fn an_explicit_default_beats_declaration_and_enable_order() {
    // `java` is enabled and comes first in the fallback order, but
    // `leekscript` claims the default.
    let manifest = manifest_of(
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\
         [backend.java]\nenable = true\n\
         [backend.leekscript]\nenable = true\ndefault = true\n",
    );
    assert_eq!(
        resolve_backend(&manifest, None).unwrap(),
        BackendKind::LeekScript
    );
}

#[test]
fn without_a_default_the_first_enabled_backend_wins_in_kind_order() {
    // Declaration order in the TOML is deliberately the reverse of the
    // java → jar → native → wasm → leekscript fallback order, so a
    // resolver that walked the file instead of the fixed order would
    // answer `leekscript` here.
    let manifest = manifest_of(
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\
         [backend.leekscript]\nenable = true\n\
         [backend.native]\nenable = true\n\
         [backend.java]\nenable = true\n",
    );
    assert_eq!(resolve_backend(&manifest, None).unwrap(), BackendKind::Java);

    let no_java = manifest_of(
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n\
         [backend.leekscript]\nenable = true\n\
         [backend.native]\nenable = true\n",
    );
    assert_eq!(
        resolve_backend(&no_java, None).unwrap(),
        BackendKind::Native
    );

    let only_leekscript = manifest_of(
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n[backend.leekscript]\nenable = true\n",
    );
    assert_eq!(
        resolve_backend(&only_leekscript, None).unwrap(),
        BackendKind::LeekScript,
        "leekscript is the last rung of the fallback order, not absent from it"
    );
}

#[test]
fn a_declared_but_disabled_backend_is_not_a_default() {
    let manifest = manifest_of(
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n[backend.java]\nenable = false\n",
    );
    assert!(resolve_backend(&manifest, None).is_err());
}

// ---- resolve_run_backend ----

#[test]
fn miku_run_accepts_only_the_native_backend() {
    assert!(resolve_run_backend(None).is_ok());

    for kind in ALL_KINDS {
        let result = resolve_run_backend(Some(kind.as_str()));
        if kind == BackendKind::Native {
            assert!(result.is_ok(), "`--backend native` is the JIT path");
            continue;
        }
        let err = result.expect_err("only native may be named").to_string();
        assert!(
            err.contains(&format!("miku build --backend {}", kind.as_str())),
            "{kind:?} should be redirected to `miku build`: {err}"
        );
    }
}

#[test]
fn miku_run_rejects_a_misspelled_backend_differently_from_a_real_one() {
    // The `Some("native")` arm is a raw string compare, so it can drift
    // out of step with `BackendKind::parse`. Wrong case is the cheapest
    // probe: `parse` rejects it, and so must this.
    let err = resolve_run_backend(Some("Native")).unwrap_err().to_string();
    assert!(err.contains("unknown backend"), "{err}");
    assert!(
        !err.contains("miku build"),
        "an unparseable name is not a wrong-backend hint: {err}"
    );
}

// ---- output directories ----

fn settings_with_out_dir(dir: Option<&str>) -> BackendSettings {
    BackendSettings {
        out_dir: dir.map(PathBuf::from),
        ..BackendSettings::default()
    }
}

#[test]
fn java_out_dir_prefers_cli_then_settings_then_the_build_dir() {
    let project = project_at("/proj", MINIMAL);
    let none = BackendSettings::default();
    let from_settings = settings_with_out_dir(Some("gen/java"));

    assert_eq!(
        pick_java_out_dir(&project, None, &none),
        PathBuf::from("/proj/build/java"),
        "no override: <build>/java"
    );
    assert_eq!(
        pick_java_out_dir(&project, None, &from_settings),
        PathBuf::from("/proj/gen/java"),
        "a relative `[backend.java].out_dir` resolves against the project root"
    );
    assert_eq!(
        pick_java_out_dir(&project, Some(Path::new("cli")), &from_settings),
        PathBuf::from("/proj/cli"),
        "--out-dir beats the manifest"
    );
}

#[test]
fn an_absolute_out_dir_is_taken_verbatim_from_either_source() {
    let project = project_at("/proj", MINIMAL);
    assert_eq!(
        pick_java_out_dir(
            &project,
            Some(Path::new("/abs/cli")),
            &BackendSettings::default()
        ),
        PathBuf::from("/abs/cli")
    );
    assert_eq!(
        pick_java_out_dir(
            &project,
            None,
            &settings_with_out_dir(Some("/abs/settings"))
        ),
        PathBuf::from("/abs/settings")
    );
}

#[test]
fn the_default_java_out_dir_follows_a_custom_paths_build() {
    let project = project_at(
        "/proj",
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n[paths]\nbuild = \"target\"\n",
    );
    assert_eq!(
        pick_java_out_dir(&project, None, &BackendSettings::default()),
        PathBuf::from("/proj/target/java")
    );
    // …but an explicit out_dir is root-relative, not build-relative.
    assert_eq!(
        pick_java_out_dir(&project, None, &settings_with_out_dir(Some("gen"))),
        PathBuf::from("/proj/gen")
    );
}

#[test]
fn every_backend_resolves_a_relative_out_dir_against_the_project_root() {
    // The regression this pins: `miku build --out-dir out` run from a
    // subdirectory must write to <root>/out whatever `--backend` says.
    // Java went through `pick_java_out_dir` while the leekscript and
    // native emitters hand-rolled it against the process CWD, and both
    // ignored `[backend.<kind>].out_dir` entirely.
    let project = project_at("/proj", MINIMAL);
    let cli = Some(Path::new("out"));
    for (label, default) in [
        ("java", project.build_dir().join("java")),
        ("leekscript", project.build_dir().join("leekscript")),
        ("native", project.root.join("demo")),
    ] {
        assert_eq!(
            pick_out_dir(&project, cli, &BackendSettings::default(), default.clone()),
            PathBuf::from("/proj/out"),
            "{label}: relative --out-dir is root-relative"
        );
        assert_eq!(
            pick_out_dir(&project, None, &BackendSettings::default(), default.clone()),
            default,
            "{label}: no override falls back to the backend default"
        );
        assert_eq!(
            pick_out_dir(
                &project,
                None,
                &settings_with_out_dir(Some("dist")),
                default
            ),
            PathBuf::from("/proj/dist"),
            "{label}: `[backend.<kind>].out_dir` is honoured"
        );
    }
}

// ---- java_clean_mode ----

#[test]
fn clean_mode_is_the_or_of_the_flag_and_the_manifest() {
    let modes = [
        (None, false),
        (Some(JavaMode::Exact), false),
        (Some(JavaMode::Clean), true),
    ];
    for (java_mode, from_manifest) in modes {
        let settings = BackendSettings {
            java_mode,
            ..BackendSettings::default()
        };
        assert_eq!(
            java_clean_mode(false, &settings),
            from_manifest,
            "--clean absent, java_mode = {java_mode:?}"
        );
        assert!(
            java_clean_mode(true, &settings),
            "--clean must win over java_mode = {java_mode:?}"
        );
    }
}

// ---- linked backends ----

#[test]
fn linked_lists_exactly_the_backends_this_build_can_run() {
    // Consumed by leek-test-driver (crates/testing/leek-test-driver/src/backends/mod.rs),
    // which skips a backend's cases when it is absent — a stale entry
    // here turns into a suite that tries to run a backend that is not
    // linked, and a missing one silently drops coverage.
    assert_eq!(LINKED, [BackendKind::Java, BackendKind::Native]);

    for kind in ALL_KINDS {
        assert_eq!(
            is_linked(kind),
            LINKED.contains(&kind),
            "is_linked disagrees with LINKED for {kind:?}"
        );
    }
}

// ---- version_from_byte ----

#[test]
fn version_from_byte_maps_the_pipeline_byte_to_a_language_version() {
    for byte in 1u8..=4 {
        assert_eq!(
            version_from_byte(byte),
            leek_syntax::Version::from_byte(byte),
            "byte {byte}"
        );
    }
}
