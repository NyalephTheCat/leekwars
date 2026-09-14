//! What `cargo build` is allowed to do on behalf of this crate (#148, #150).
//!
//! Both rules here were broken, and both broke *builds*, not tests: the
//! build script used to shell out to the upstream JVM suite (minutes,
//! rewriting a tracked 13 MB file) whenever mtimes made the dataset look
//! stale, and it declared the whole compiler as a build-dependency to
//! use four serde structs. Neither is visible from a passing test suite,
//! so the sources themselves are what is asserted.

use std::collections::BTreeSet;

/// The build script, with comment-only lines removed — the assertions
/// below are about what it *runs*, and its module doc legitimately names
/// the command that does the expensive work.
fn build_script_code() -> String {
    include_str!("../build.rs")
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A build must never run the official JVM suite (#148). It takes
/// minutes and writes into the source tree; `cargo run -p
/// leek-test-corpus -- extract-reference` is where that belongs.
#[test]
fn the_build_script_cannot_reach_the_jvm_regeneration() {
    let code = build_script_code();
    // Note `java-emitter` is *not* forbidden: the script reads the
    // emitter's overlay sources, which is the cheap half of its job.
    for forbidden in [
        "reference",
        "jvm_available",
        "regenerate",
        "Command",
        "process::",
    ] {
        assert!(
            !code.contains(forbidden),
            "build.rs mentions `{forbidden}`: a build must not regenerate the reference \
             dataset or run any external tool (#148). Keep that in the `extract-reference` \
             subcommand."
        );
    }
}

/// The build script's dependencies are paid in full codegen, on every
/// `cargo check` and `cargo clippy` of the workspace, before the script
/// can run at all. `leek-test-driver` pulled the frontend, the backends
/// and 13 Cranelift crates in here for four serde structs (#150).
#[test]
fn the_build_dependencies_stay_tiny() {
    let manifest: toml::Table =
        toml::from_str(include_str!("../Cargo.toml")).expect("parse Cargo.toml");
    let deps: BTreeSet<&str> = manifest["build-dependencies"]
        .as_table()
        .expect("[build-dependencies] table")
        .keys()
        .map(String::as_str)
        .collect();

    let allowed = BTreeSet::from(["leek-test-cases", "anyhow"]);
    let extra: Vec<&&str> = deps.difference(&allowed).collect();
    assert!(
        extra.is_empty(),
        "unexpected build-dependencies {extra:?}: every one of them is codegen'd into the \
         build-script context on every build (#150). The data model lives in the \
         dependency-free `leek-test-cases` crate precisely so this list can stay at \
         {allowed:?}."
    );
}

/// A crashed JVM run must fail the generator instead of leaving a
/// truncated dataset behind that looks like a successful refresh (#148).
#[test]
fn the_generator_does_not_mask_a_failed_jvm_run() {
    let script = include_str!("../../../../tools/java-emitter/generate-reference.sh");
    let run_line = script
        .lines()
        .find(|l| l.contains("java -cp") && l.contains("GenerateReference"))
        .expect("generate-reference.sh runs GenerateReference");
    assert!(
        !run_line.contains("|| true"),
        "the GenerateReference run is masked with `|| true`: a crashed JVM would produce a \
         truncated reference dataset and still report success (#148)"
    );
    assert!(
        script.contains("LEEK_REFERENCE_MIN_ROWS"),
        "generate-reference.sh must check the row count it produced against a floor, so a \
         partial run cannot be committed as a refresh (#148)"
    );
}
