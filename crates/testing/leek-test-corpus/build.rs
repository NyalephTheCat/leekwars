//! Build script: extract the upstream JUnit suite into
//! `OUT_DIR/upstream_cases.toml`, and stage the official-LeekScript
//! reference dataset into `OUT_DIR/reference.tsv` (both embedded at
//! compile time). See `src/reference.rs` for the gated-regen policy.

#[path = "src/extract.rs"]
mod extract;
#[path = "src/reference.rs"]
mod reference;

use leek_test_driver::cases;

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
    println!("cargo:rerun-if-changed=src/reference.rs");
    println!("cargo:rerun-if-changed=../../../tools/java-emitter/generate-reference.sh");
    println!("cargo:rerun-if-changed=../../../tools/java-emitter/GenerateReference.java");
    println!("cargo:rerun-if-changed=../leek-test-driver/src/cases.rs");
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

    println!(
        "cargo:warning=extracted {} upstream test cases (skipped {} calls) -> {}",
        manifest.cases.len(),
        manifest.skipped.len(),
        out_path.display(),
    );

    // Stage the official-LeekScript reference dataset (value + ops +
    // Java per case) into OUT_DIR for `include_str!`. Gated: only
    // re-runs the JVM suite when the dataset is missing/stale and a JDK
    // + submodule are present (see `reference::prepare_embed`).
    reference::prepare_embed(manifest_dir, Path::new(&out_dir_path));
}
