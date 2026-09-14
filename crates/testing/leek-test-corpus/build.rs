//! Build script: extract the upstream JUnit suite into
//! `OUT_DIR/upstream_cases.toml`, embedded at compile time.
//!
//! That is deliberately *all* it does. The official-LeekScript reference
//! dataset is not staged, regenerated or embedded here (#148): producing
//! it runs the upstream JVM suite for minutes and writes a tracked file,
//! which is the explicit `cargo run -p leek-test-corpus --
//! extract-reference` command's job, not a build's. This script must
//! stay cheap enough to run on every `cargo build` / `cargo clippy`,
//! with no JDK, no submodule and no network.

#[path = "src/extract.rs"]
mod extract;

use leek_test_cases as cases;

use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let upstream: PathBuf = manifest_dir
        .join("../../..")
        .join("official-generator/leek-wars-generator/leekscript/src/test/java/test");
    // Pristine-submodule instrumentation: shadows/extends the upstream
    // test sources (see tools/java-emitter/overlay.sh).
    let overlay: PathBuf = manifest_dir
        .join("../../..")
        .join("tools/java-emitter/overlay/src/test/java/test");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/extract.rs");
    println!("cargo:rerun-if-changed=../leek-test-cases/src/lib.rs");
    for dir in [&upstream, &overlay] {
        if dir.exists()
            && let Ok(entries) = std::fs::read_dir(dir)
        {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "java") {
                    println!("cargo:rerun-if-changed={}", p.display());
                }
            }
        }
    }

    let out_dir_path = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_path = Path::new(&out_dir_path).join("upstream_cases.toml");

    let manifest = if upstream.exists() {
        match extract::extract_all(&upstream, Some(&overlay)) {
            Ok(m) => m,
            Err(e) => {
                println!("cargo:warning=upstream extraction failed: {e}");
                cases::Manifest::empty()
            }
        }
    } else {
        println!(
            "cargo:warning=upstream tests dir not found at {}; embedding empty manifest",
            upstream.display(),
        );
        cases::Manifest::empty()
    };

    if let Err(e) = manifest.save(&out_path) {
        panic!("failed to write {}: {}", out_path.display(), e);
    }

    // Plain stdout, not `cargo:warning=`: this is progress information,
    // and `cargo clippy --workspace --all-targets` is required to be
    // silent (tools/check.sh). Only the two failure branches above warn.
    println!(
        "extracted {} upstream test cases (skipped {} calls) -> {}",
        manifest.cases.len(),
        manifest.skipped.len(),
        out_path.display(),
    );
}
