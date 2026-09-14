//! End-to-end tests for the `leekc` command line.
//!
//! Each test writes a fixture into a scratch directory, runs the real
//! binary, and asserts on exit code plus one distinctive marker per emit.
//! The harness mirrors `bins/miku/tests/smoke.rs` — no framework, so a
//! failure shows the whole invocation.
//!
//! Deliberately not covered: `--emit native --native-emit exe` shells out
//! to `cargo` to link a standalone binary, and `--native-emit object`
//! writes host object files. Both are slow and environment-dependent; the
//! native backend has its own tests.

use std::path::{Path, PathBuf};
use std::process::Command;

fn leekc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_leekc"))
}

fn scratch_dir(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "leekc-test-{label}-{}-{:?}",
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

fn leekc(args: &[&str], cwd: &Path) -> Output {
    let out = Command::new(leekc_bin())
        .args(args)
        // Keep human rendering deterministic across CI and a local terminal.
        .env("NO_COLOR", "1")
        .current_dir(cwd)
        .output()
        .expect("run leekc");
    Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A three-statement program: a global, a function, and a call.
const FIXTURE: &str =
    "// @version:4\nvar a = 1;\nfunction twice(x) { return x * 2; }\nreturn twice(a);\n";

fn fixture(label: &str) -> (PathBuf, PathBuf) {
    let dir = scratch_dir(label);
    let path = dir.join("main.leek");
    std::fs::write(&path, FIXTURE).expect("write fixture");
    (dir, path)
}

#[test]
fn each_emit_produces_its_own_artifact() {
    // One distinctive marker per emit: if two emits ever collapse onto the
    // same pipeline output, exactly one row here fails.
    for (emit, marker) in [
        ("tokens", "KwVar@14..17"),
        ("flat-cst", "SourceFile@0..78"),
        ("cst", "VarDeclStmt"),
        ("hir", "function twice"),
        ("mir", "bb0:"),
        ("java", "public class AI_0"),
        ("leekscript", "return twice(a);"),
        ("fmt", "function twice(x) {"),
    ] {
        let (dir, _) = fixture(&format!("emit-{emit}"));
        let out = leekc(&["main.leek", "--emit", emit], &dir);
        assert_eq!(out.status, 0, "--emit {emit} stderr: {}", out.stderr);
        assert!(
            out.stdout.contains(marker),
            "--emit {emit} stdout missing {marker:?}:\n{}",
            out.stdout
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn check_is_the_default_emit_and_prints_nothing_for_clean_code() {
    let (dir, _) = fixture("default-emit");
    let out = leekc(&["main.leek"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.is_empty(), "stdout: {}", out.stdout);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn emit_run_jits_the_program_and_prints_its_value() {
    let (dir, _) = fixture("run");
    let out = leekc(&["main.leek", "--emit", "run"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "2");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn emit_java_with_out_dir_writes_the_class_and_its_line_map() {
    let (dir, _) = fixture("java-out");
    let out = leekc(&["main.leek", "--emit", "java", "-o", "out"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.is_empty(), "the class went to disk, not stdout");
    assert!(out.stderr.contains("wrote"), "stderr: {}", out.stderr);
    assert!(dir.join("out/AI_0.java").is_file());
    assert!(dir.join("out/AI_0.lines").is_file());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ai_id_names_the_emitted_java_class() {
    let (dir, _) = fixture("ai-id");
    let out = leekc(&["main.leek", "--emit", "java", "--ai-id", "42"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("public class AI_42"), "{}", out.stdout);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn emit_leekscript_with_out_dir_writes_a_round_trippable_file() {
    let (dir, _) = fixture("ls-out");
    let out = leekc(&["main.leek", "--emit", "leekscript", "-o", "out"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    let emitted = dir.join("out/main.out.leek");
    assert!(emitted.is_file(), "stderr: {}", out.stderr);

    // The emitted LeekScript must itself compile.
    let recheck = leekc(&["out/main.out.leek"], &dir);
    assert_eq!(recheck.status, 0, "stderr: {}", recheck.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn compact_leekscript_is_a_single_line() {
    let (dir, _) = fixture("compact");
    let out = leekc(&["main.leek", "--emit", "leekscript", "--compact"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.trim_end().lines().count(),
        1,
        "stdout: {}",
        out.stdout
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_error_exits_one_and_allowing_its_code_exits_zero() {
    let dir = scratch_dir("severity");
    std::fs::write(
        dir.join("main.leek"),
        "// @version:4\nvar a = 1;\nvar a = 2;\nreturn a;\n",
    )
    .expect("write");

    let plain = leekc(&["main.leek"], &dir);
    assert_eq!(plain.status, 1, "stderr: {}", plain.stderr);
    assert!(plain.stderr.contains("E0202"), "stderr: {}", plain.stderr);

    let allowed = leekc(&["main.leek", "--allow", "E0202"], &dir);
    assert_eq!(allowed.status, 0, "stderr: {}", allowed.stderr);
    assert!(!allowed.stderr.contains("E0202"), "{}", allowed.stderr);

    // Demoting keeps the diagnostic but clears the failure.
    let warned = leekc(&["main.leek", "--warn", "E0202"], &dir);
    assert_eq!(warned.status, 0, "stderr: {}", warned.stderr);
    assert!(warned.stderr.contains("E0202"), "{}", warned.stderr);

    // The canonical name works wherever the id does.
    let by_name = leekc(&["main.leek", "--allow", "RedeclaredSymbol"], &dir);
    assert_eq!(by_name.status, 0, "stderr: {}", by_name.stderr);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn denying_a_warning_code_turns_a_clean_run_into_a_failure() {
    let dir = scratch_dir("deny");
    std::fs::write(
        dir.join("main.leek"),
        "// @version:4\nvar unusedVar = 1;\nreturn 2;\n",
    )
    .expect("write");

    let plain = leekc(&["main.leek"], &dir);
    assert_eq!(plain.status, 0, "a lint warning alone must not fail");

    let denied = leekc(&["main.leek", "--deny", "L0001"], &dir);
    assert_eq!(denied.status, 1, "stderr: {}", denied.stderr);
    assert!(denied.stderr.contains("error"), "{}", denied.stderr);

    std::fs::remove_dir_all(&dir).ok();
}

// ---- include-using inputs ----
//
// `leekc` plans the name-resolving emits through `leek_session`, the same
// entry point `miku` uses, so a single file's `include("helper")` is
// resolved from disk instead of being inert (DRIVER-02).

/// A two-file fixture: `main.leek` includes `helper.leek` and calls a
/// function defined there.
fn include_fixture(label: &str) -> PathBuf {
    let dir = scratch_dir(label);
    std::fs::write(
        dir.join("main.leek"),
        "// @version:4\ninclude(\"helper\");\nreturn twice(21);\n",
    )
    .expect("write entry");
    std::fs::write(
        dir.join("helper.leek"),
        "// @version:4\nfunction twice(x) { return x * 2; }\n",
    )
    .expect("write helper");
    dir
}

#[test]
fn leekc_resolves_includes_for_the_emits_that_need_names() {
    let dir = include_fixture("include-run");

    let run = leekc(&["main.leek", "--emit", "run"], &dir);
    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    // The included body was compiled, not merely accepted.
    assert_eq!(run.stdout.trim(), "42", "stderr: {}", run.stderr);

    let ls = leekc(&["main.leek", "--emit", "leekscript"], &dir);
    assert_eq!(ls.status, 0, "stderr: {}", ls.stderr);
    assert!(
        ls.stdout.contains("function twice"),
        "the included definition must be spliced into the one emitted \
         file:\n{}",
        ls.stdout
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn leekc_reports_a_missing_include_against_the_including_file() {
    let dir = scratch_dir("include-missing");
    std::fs::write(
        dir.join("main.leek"),
        "// @version:4\ninclude(\"nope\");\nreturn 1;\n",
    )
    .expect("write");

    let out = leekc(&["main.leek"], &dir);
    assert_eq!(out.status, 1, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("E0272"), "stderr: {}", out.stderr);
    assert!(out.stderr.contains("main.leek"), "stderr: {}", out.stderr);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn leekc_renders_a_diagnostic_raised_in_an_included_file_against_that_file() {
    // The included file gets its own `SourceId` and its offsets index into
    // its own text: rendering it against the entry would point at the
    // wrong line, or past the end of the entry entirely.
    let dir = scratch_dir("include-diag");
    std::fs::write(
        dir.join("main.leek"),
        "// @version:4\ninclude(\"helper\");\nreturn 1;\n",
    )
    .expect("write entry");
    std::fs::write(
        dir.join("helper.leek"),
        "// @version:4\n// padding so the offsets do not fit the entry\n\
         var dup = 1;\nvar dup = 2;\n",
    )
    .expect("write helper");

    let out = leekc(&["main.leek"], &dir);
    assert_eq!(out.status, 1, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("E0202"), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("helper.leek"),
        "the redeclaration is in helper.leek: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("var dup = 2;"),
        "the snippet must come from helper.leek's own text: {}",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unknown_severity_code_is_a_usage_error_not_a_compile_failure() {
    let (dir, _) = fixture("bad-code");
    let out = leekc(&["main.leek", "--allow", "NOPE9999"], &dir);
    assert_eq!(out.status, 2, "usage errors exit 2: {}", out.stderr);
    assert!(
        out.stderr.contains("unknown diagnostic code"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn json_message_format_writes_one_object_per_line_on_stdout() {
    let dir = scratch_dir("json");
    std::fs::write(
        dir.join("main.leek"),
        "// @version:4\nvar a = 1;\nvar a = 2;\nreturn a;\n",
    )
    .expect("write");

    let out = leekc(&["main.leek", "--message-format", "json"], &dir);
    assert_eq!(out.status, 1, "stderr: {}", out.stderr);
    let lines: Vec<&str> = out.stdout.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "stdout: {}", out.stdout);
    for line in &lines {
        assert!(line.starts_with('{') && line.ends_with('}'), "{line}");
        assert!(line.contains("\"code\""), "{line}");
        assert!(line.contains("\"severity\""), "{line}");
    }
    assert!(lines.iter().any(|l| l.contains("E0202")), "{lines:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn version_pragma_flag_overrides_the_files_own_pragma() {
    let dir = scratch_dir("version");
    std::fs::write(dir.join("main.leek"), "// @version:4\nreturn 1;\n").expect("write");

    // The Java backend bakes the language version into the `super(...)`
    // call, so it is the visible proof of which version won.
    let from_file = leekc(&["main.leek", "--emit", "java"], &dir);
    assert!(
        from_file.stdout.contains("super(1, 4)"),
        "{}",
        from_file.stdout
    );

    let overridden = leekc(
        &["main.leek", "--emit", "java", "--version-pragma", "3"],
        &dir,
    );
    assert!(
        overridden.stdout.contains("super(1, 3)"),
        "{}",
        overridden.stdout
    );

    let bad = leekc(&["main.leek", "--version-pragma", "9"], &dir);
    assert_eq!(bad.status, 2, "stderr: {}", bad.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_library_flag_dispatches_host_functions_through_their_class() {
    let dir = scratch_dir("library");
    std::fs::write(
        dir.join("main.leek"),
        "// @version:4\nvar c = getCell();\nreturn c;\n",
    )
    .expect("write");

    // Without `--library` the backend has no dispatch class for `getCell`
    // and nothing but a bare `getCell(...)` to write — which is not a method
    // on the generated class. That used to be printed as a successful
    // emission; it now fails and says where (JAVA-06 / #152).
    let plain = leekc(&["main.leek", "--emit", "java", "--no-color"], &dir);
    assert_eq!(plain.status, 1, "stderr: {}", plain.stderr);
    assert!(
        plain.stderr.contains("error[E0610]") && plain.stderr.contains("`getCell`"),
        "stderr: {}",
        plain.stderr
    );
    assert!(
        !plain.stdout.contains("EntityClass"),
        "without --library there is no dispatch class:\n{}",
        plain.stdout
    );

    let with_lib = leekc(
        &["main.leek", "--emit", "java", "--library", "leekwars"],
        &dir,
    );
    assert_eq!(with_lib.status, 0, "stderr: {}", with_lib.stderr);
    assert!(
        with_lib.stdout.contains("EntityClass.getCell"),
        "stdout: {}",
        with_lib.stdout
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unknown_library_spec_is_reported_not_ignored() {
    let (dir, _) = fixture("bad-library");
    let out = leekc(&["main.leek", "--library", "/no/such/library.lib"], &dir);
    assert_eq!(out.status, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("library"), "stderr: {}", out.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_missing_input_file_is_a_usage_error_naming_the_path() {
    let dir = scratch_dir("missing-input");
    let out = leekc(&["nope.leek"], &dir);
    assert_eq!(out.status, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("nope.leek"), "stderr: {}", out.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fmt_config_changes_the_formatter_output() {
    let dir = scratch_dir("fmt-config");
    std::fs::write(
        dir.join("main.leek"),
        "// @version:4\nif (1) { return 2; }\n",
    )
    .expect("write");
    std::fs::write(
        dir.join("Miku.toml"),
        "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n[format]\nindent = 7\n",
    )
    .expect("write manifest");

    let default = leekc(&["main.leek", "--emit", "fmt"], &dir);
    assert_eq!(default.status, 0, "stderr: {}", default.stderr);
    let configured = leekc(
        &["main.leek", "--emit", "fmt", "--fmt-config", "Miku.toml"],
        &dir,
    );
    assert_eq!(configured.status, 0, "stderr: {}", configured.stderr);
    assert_ne!(
        default.stdout, configured.stdout,
        "`--fmt-config` must reach the formatter"
    );
    assert!(
        configured.stdout.contains("       return"),
        "stdout: {:?}",
        configured.stdout
    );
    std::fs::remove_dir_all(&dir).ok();
}
