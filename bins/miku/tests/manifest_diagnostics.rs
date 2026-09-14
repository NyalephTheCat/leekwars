//! A bad `Miku.toml` reaches the user the way a bad `.leek` file does.
//!
//! Manifest warnings used to be `eprintln!("warning: {w}")` loops in every
//! subcommand: no code, no span, and plain text interleaved with the JSON
//! stream under `--message-format json`. These tests pin the replacement —
//! they go through the real binary, because the loops they replace were in the
//! binary and nothing below it could have caught them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch_dir(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "miku-manifest-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch dir");
    path
}

struct Output {
    status: i32,
    stdout: String,
    stderr: String,
}

fn miku(args: &[&str], cwd: &Path) -> Output {
    let out = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_miku")))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run miku");
    Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A project whose manifest is `BASE` + `extra`, with a trivial entry point.
fn project(label: &str, extra: &str) -> PathBuf {
    let dir = scratch_dir(label);
    std::fs::write(
        dir.join("Miku.toml"),
        format!("[project]\nname = \"demo\"\nversion = \"0.1.0\"\n{extra}"),
    )
    .expect("write manifest");
    std::fs::create_dir_all(dir.join("src")).expect("src dir");
    std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("write entry");
    dir
}

/// `[fight] worker_count` is a key no toolchain knows: a W0400 warning.
const UNKNOWN_FIELD: &str = "[fight]\nworker_count = 4\n";

#[test]
fn a_manifest_warning_is_json_under_message_format_json() {
    let dir = project("json", UNKNOWN_FIELD);
    let out = miku(&["check", "--message-format", "json"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);

    // Every line of the stream must be JSON — the old `eprintln!` loop wrote
    // plain text alongside it.
    let mut found = None;
    for line in out.stdout.lines().filter(|l| !l.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON: {line:?} ({e})"));
        if value["code"]["id"] == serde_json::json!("W0400") {
            found = Some(value);
        }
    }
    let diag = found.expect("the manifest warning must reach the JSON stream");
    assert_eq!(diag["severity"], serde_json::json!("warning"));
    assert_eq!(
        diag["code"]["name"],
        serde_json::json!("ManifestUnknownField")
    );
    let span = &diag["primary"]["span"];
    let start = usize::try_from(span["start"].as_u64().expect("span start")).expect("fits");
    let end = usize::try_from(span["end"].as_u64().expect("span end")).expect("fits");
    assert!(end > start, "the span must be real, got {start}..{end}");

    // …and it points at the offending key in the manifest text.
    let text = std::fs::read_to_string(dir.join("Miku.toml")).expect("read manifest");
    assert_eq!(
        &text[start..end],
        "worker_count",
        "the span must cover the key that caused the warning"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_manifest_warning_renders_with_a_code_and_a_caret() {
    let dir = project("human", UNKNOWN_FIELD);
    let out = miku(&["check", "--color", "never"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("warning[W0400]"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("fight.worker_count"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Miku.toml") && out.stderr.contains('^'),
        "the snippet must name the manifest and underline the key; stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_manifest_lint_table_governs_its_own_warnings() {
    let allowed = project(
        "allow",
        &format!("{UNKNOWN_FIELD}[lint]\nallow = [\"W0400\"]\n"),
    );
    let out = miku(&["check", "--color", "never"], &allowed);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        !out.stderr.contains("W0400"),
        "`allow` must silence it; stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&allowed).ok();

    let denied = project(
        "deny",
        &format!("{UNKNOWN_FIELD}[lint]\ndeny = [\"W0400\"]\n"),
    );
    let out = miku(&["check", "--color", "never"], &denied);
    assert_eq!(
        out.status, 1,
        "`deny` must fail the command; stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("error[W0400]"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&denied).ok();
}

#[test]
fn a_manifest_error_points_into_the_manifest_not_the_entry_file() {
    // The regression: manifest errors were built at `SourceId(1)` — the entry
    // `.leek` file — so a bad `Miku.toml` rendered a caret at byte 0 of
    // `main.leek`, pointing at code that was fine.
    let dir = project("error", "[moonbeam]\nx = 1\n");
    let out = miku(&["check", "--color", "never"], &dir);
    assert_ne!(out.status, 0, "a bad manifest must fail");
    assert!(out.stderr.contains("moonbeam"), "stderr: {}", out.stderr);
    assert!(
        !out.stderr.contains("main.leek"),
        "the error must not point at the entry file; stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unknown_lint_code_names_the_entry_that_spelled_it() {
    let dir = project("lint-typo", "[lint]\ndeny = [\"NOPE9999\"]\n");
    let out = miku(&["check", "--color", "never"], &dir);
    assert_ne!(out.status, 0, "an unknown lint code must fail the build");
    assert!(out.stderr.contains("NOPE9999"), "stderr: {}", out.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

// ---- keys that are in the schema but do nothing (DRIVER-06) ----

#[test]
fn an_ignored_key_warns_with_a_caret_and_does_not_fail_the_build() {
    let dir = project(
        "ignored",
        "[backend.native]\nenable = true\ndefault = true\ntarget = \"riscv64-unknown-linux-gnu\"\n",
    );
    let out = miku(&["check"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("W0402") && out.stderr.contains("backend.native.target"),
        "stderr: {}",
        out.stderr
    );
    // The reason, not just the fact.
    assert!(
        out.stderr.contains("cross-compilation is not supported"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_ignored_key_can_be_allowed() {
    let dir = project(
        "ignored_allow",
        "[lint]\nallow = [\"W0402\"]\n\n\
         [backend.native]\nenable = true\ndefault = true\ntarget = \"riscv64\"\n",
    );
    let out = miku(&["check"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(!out.stderr.contains("W0402"), "stderr: {}", out.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn two_default_backends_fail_the_command() {
    // Before this was validated, `default_kind` silently took the first in a
    // fixed order — so a project asking for native got java and no warning.
    let dir = project(
        "two_defaults",
        "[backend.java]\nenable = true\ndefault = true\n\n\
         [backend.native]\nenable = true\ndefault = true\n",
    );
    let out = miku(&["check"], &dir);
    // A manifest that cannot be resolved at all is a load failure (exit 2),
    // not a diagnostic against the source.
    assert_eq!(out.status, 2, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("backend.native.default")
            && out.stderr.contains("the only backend marked `default`"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_default_backend_that_is_disabled_fails_the_command() {
    let dir = project(
        "disabled_default",
        "[backend.native]\nenable = false\ndefault = true\n",
    );
    let out = miku(&["check"], &dir);
    assert_eq!(out.status, 2, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("backend.native.enable"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_removed_key_reports_as_unknown_rather_than_being_swallowed() {
    // `edition` and `java_version` parsed into fields nothing read.
    let dir = project(
        "removed_keys",
        "edition = \"2024\"\n\n[backend.java]\nenable = true\njava_version = 17\n",
    );
    let out = miku(&["check"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("project.edition") && out.stderr.contains("backend.java.java_version"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_backend_key_on_the_wrong_backend_warns() {
    let dir = project(
        "wrong_backend_key",
        "[backend.native]\nenable = true\ndefault = true\nmode = \"clean\"\n",
    );
    let out = miku(&["check"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("backend.native.mode"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_duration_shaped_test_timeout_is_an_error_not_silence() {
    let dir = project("timeout_str", "[test]\ntimeout = \"5s\"\n");
    let out = miku(&["check"], &dir);
    assert_eq!(out.status, 2, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("test.timeout") && out.stderr.contains("ops"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}
