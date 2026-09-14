//! End-to-end smoke tests for `miku`.
//!
//! Each test creates a scratch directory under the temp dir, copies
//! files in, runs the `miku` binary as a subprocess, and asserts on
//! exit code + stdout/stderr. The harness is intentionally minimal —
//! no fancy framework, no shared state — so failures show their full
//! invocation in the assertion message.

use std::path::{Path, PathBuf};
use std::process::Command;

fn miku_bin() -> PathBuf {
    let exe = env!("CARGO_BIN_EXE_miku");
    PathBuf::from(exe)
}

fn scratch_dir(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "miku-test-{label}-{}-{}",
        std::process::id(),
        random_suffix()
    ));
    if path.exists() {
        let _ = std::fs::remove_dir_all(&path);
    }
    std::fs::create_dir_all(&path).expect("create scratch dir");
    path
}

fn random_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Truncating the nanosecond count is fine — the suffix just needs to vary
    // between runs, not be a faithful timestamp.
    #[allow(clippy::cast_possible_truncation)]
    {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}

struct Output {
    status: i32,
    stdout: String,
    stderr: String,
}

fn miku(args: &[&str], cwd: &Path) -> Output {
    let out = Command::new(miku_bin())
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

fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).expect("write fixture");
}

#[test]
fn new_creates_skeleton() {
    let base = scratch_dir("new");
    let out = miku(&["new", "demo"], &base);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(base.join("demo/Miku.toml").is_file());
    assert!(base.join("demo/src/main.leek").is_file());
    assert!(base.join("demo/.gitignore").is_file());
    // One output root, one ignore line.
    assert_eq!(
        std::fs::read_to_string(base.join("demo/.gitignore")).unwrap(),
        "/build/\n"
    );

    // The skeleton should pass `miku check`.
    let check = miku(&["check"], &base.join("demo"));
    assert_eq!(check.status, 0, "stderr: {}", check.stderr);

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn explain_prints_known_code() {
    let base = scratch_dir("explain_ok");
    // Case-insensitive; needs no project.
    let out = miku(&["explain", "l0022"], &base);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("L0022") && out.stdout.contains("unused expression"),
        "stdout: {}",
        out.stdout
    );
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn explain_unknown_code_lists_available() {
    let base = scratch_dir("explain_bad");
    let out = miku(&["explain", "Z9999"], &base);
    assert_eq!(out.status, 2, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("unknown diagnostic code") && out.stderr.contains("available for"),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn run_outputs_value() {
    let dir = scratch_dir("run");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "runme"
version = "0.1.0"

[backend.native]
enable  = true
default = true
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 42;\n");

    let out = miku(&["run"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(out.stdout.trim(), "42");

    std::fs::remove_dir_all(&dir).ok();
}

/// `miku run` used to answer a native compile failure with one line —
/// `error: unsupported: integer ** with non-constant/large exponent` — and no
/// hint of where in the file it was (NATIVE-15 / #173).
#[test]
fn run_reports_an_unsupported_construct_with_a_file_and_a_line() {
    let dir = scratch_dir("run_unsupported");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "nope"
version = "0.1.0"

[backend.native]
enable  = true
default = true
"#,
    );
    // `2 ** b` with an exponent the optimiser cannot fold is outside the
    // native subset. It is on line 4.
    write(
        &dir,
        "src/main.leek",
        "// @version:4\nvar b = 3;\nfor (var i = 0; i < 1; i++) { b = b + i; }\nreturn 2 ** b;\n",
    );

    let out = miku(&["run", "--color", "never"], &dir);
    assert_eq!(out.status, 1, "stdout: {}", out.stdout);
    assert!(
        out.stderr.contains("error[E0600]"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("main.leek:4:8"),
        "no file:line:col in stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("return 2 ** b;"),
        "no source line in stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("while compiling `"),
        "no function attribution in stderr: {}",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `miku build --backend java` used to write the `.java` and report success
/// for a program the emitter had no translation for — the user found out from
/// a javac error inside generated code, or not at all (JAVA-06 / #152).
#[test]
fn build_java_reports_an_unsupported_construct_with_a_file_and_a_line() {
    let dir = scratch_dir("build_java_unsupported");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "nope"
version = "0.1.0"

[backend.java]
enable  = true
default = true
"#,
    );
    // `nosuchfn` is in neither the builtin table nor a host environment, so
    // the emitter falls back to a bare `nosuchfn(b)` — not a method on the
    // generated class. It is on line 3.
    write(
        &dir,
        "src/main.leek",
        "// @version:4\nvar b = 3;\nvar c = nosuchfn(b);\nreturn c;\n",
    );

    let out = miku(&["build", "--backend", "java", "--color", "never"], &dir);
    assert_eq!(
        out.status, 1,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        out.stderr.contains("error[E0610]"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("main.leek:3:9"),
        "no file:line:col in stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("var c = nosuchfn(b);"),
        "no source line in stderr: {}",
        out.stderr
    );
    // The known-bad translation is not left on disk claiming to be a build.
    assert!(
        !dir.join("build/java").exists() || std::fs::read_dir(dir.join("build/java")).is_err(),
        "a .java was written for a program the backend cannot translate"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_honors_file_version_pragma_over_manifest_language() {
    // Regression (#36): the project pre-scan never matched `// @version:N`,
    // so a v1 file in a `language = 4` project was lexed/parsed at v4 (where
    // `class` is a keyword) while HIR lowered it at v1.
    let dir = scratch_dir("run_v1_pragma");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name     = "oldleek"
version  = "0.1.0"
language = 4

[backend.native]
enable  = true
default = true
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:1\nvar class = 5;\nreturn class;\n",
    );

    let out = miku(&["run"], &dir);
    assert_eq!(
        out.status, 0,
        "stderr: {}\nstdout: {}",
        out.stderr, out.stdout
    );
    assert_eq!(out.stdout.trim(), "5");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn run_honors_strict_pragma() {
    // Regression (#58): `miku run` never passed strict mode to the native
    // JIT. Strict typing pins an untyped `var a = 10` slot to integer, so
    // `a += 0.5` stays `10`; non-strict it becomes `10.5`.
    let dir = scratch_dir("run_strict");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "strictleek"
version = "0.1.0"

[backend.native]
enable  = true
default = true
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4\n// @strict\nvar a = 10;\na += 0.5;\nreturn a;\n",
    );

    let out = miku(&["run"], &dir);
    assert_eq!(
        out.status, 0,
        "stderr: {}\nstdout: {}",
        out.stderr, out.stdout
    );
    assert_eq!(out.stdout.trim(), "10");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn build_java_writes_artifact() {
    let dir = scratch_dir("build");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "buildme"
version = "0.1.0"

[backend.java]
enable  = true
default = true
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 1 + 2;\n");

    let out = miku(&["build"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(dir.join("build/java/AI_0.java").is_file());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_reports_errors() {
    let dir = scratch_dir("check_err");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "checkme"
version = "0.1.0"
"#,
    );
    // A parse error — the lexer rejects `$` and the parser fails
    // on the malformed `var` chain.
    write(&dir, "src/main.leek", "var var var = $@%\n");

    let out = miku(&["check"], &dir);
    assert_ne!(
        out.status, 0,
        "expected nonzero, got 0 (stderr: {})",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_native_compat_warns_without_failing_the_check() {
    let dir = scratch_dir("check_native_compat");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "compatme"
version = "0.1.0"
"#,
    );
    // `2 ** b` with a non-constant exponent compiles for Java and not for
    // native, which is exactly what this pass is for.
    write(
        &dir,
        "src/main.leek",
        "var b = 3\nvar out = 0\nout = 2 ** b\nreturn out\n",
    );

    // Off by default: this project names no backend, so it hears nothing.
    let quiet = miku(&["check"], &dir);
    assert_eq!(quiet.status, 0, "stderr: {}", quiet.stderr);
    assert!(!quiet.stderr.contains("E0600"), "stderr: {}", quiet.stderr);

    let out = miku(&["check", "--native-compat"], &dir);
    // A warning, not an error: `miku check` is a CI gate, and a project that
    // ships to Java must not fail it over the native subset.
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("E0600"), "stderr: {}", out.stderr);
    assert!(out.stderr.contains("warning"), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("main.leek:3"),
        "the warning points at the construct: {}",
        out.stderr
    );

    // …and the opt-out silences it even when asked for both ways round.
    let off = miku(&["check", "--no-native-compat"], &dir);
    assert_eq!(off.status, 0, "stderr: {}", off.stderr);
    assert!(!off.stderr.contains("E0600"), "stderr: {}", off.stderr);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fmt_check_returns_nonzero_on_unformatted_input() {
    let dir = scratch_dir("fmt_check");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "fmtme"
version = "0.1.0"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4\nvar    x=1;\nreturn   x;\n",
    );

    let out = miku(&["fmt", "--check"], &dir);
    assert_ne!(out.status, 0, "expected reformat needed, got status 0");

    // Without --check, the formatter should rewrite in place and
    // make the file idempotent on a second pass.
    let out2 = miku(&["fmt"], &dir);
    assert_eq!(out2.status, 0, "stderr: {}", out2.stderr);
    let out3 = miku(&["fmt", "--check"], &dir);
    assert_eq!(out3.status, 0, "fmt --check should pass after fmt");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fmt_keeps_fmt_off_regions_across_runs() {
    let dir = scratch_dir("fmt_off");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "fmtoff"
version = "0.1.0"
"#,
    );
    let protected = "var    table  =  [1,  2,\n         3];";
    write(
        &dir,
        "src/main.leek",
        &format!("// @version:4\n// fmt: off\n{protected}\n// fmt: on\nvar    y=2;\nreturn y;\n"),
    );

    let out = miku(&["fmt"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    let formatted = std::fs::read_to_string(dir.join("src/main.leek")).expect("read back");
    // The markers are how the next run knows the region is off; if
    // they are dropped, `fmt --check` below reformats it (#367).
    assert!(formatted.contains("// fmt: off"), "{formatted:?}");
    assert!(formatted.contains("// fmt: on"), "{formatted:?}");
    assert!(formatted.contains(protected), "{formatted:?}");
    assert!(formatted.contains("var y = 2;"), "{formatted:?}");

    let out = miku(&["fmt", "--check"], &dir);
    assert_eq!(
        out.status, 0,
        "fmt is not idempotent over a `fmt: off` region (stderr: {})",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn lint_honors_allow_list() {
    let dir = scratch_dir("lint_allow");
    // Unused variable triggers L0001.
    write(
        &dir,
        "src/main.leek",
        "// @version:4\nvar unused_x = 1;\nreturn 0;\n",
    );

    // Without allow: lint is non-error severity by default for L0001
    // (it's a warning), so the exit code is 0 either way; but the
    // diagnostic should appear in stderr. With `allow`, the
    // diagnostic should be silenced.
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "lintme"
version = "0.1.0"
[lint]
deny = ["L0001"]
"#,
    );
    let denied = miku(&["lint"], &dir);
    assert_ne!(
        denied.status, 0,
        "expected nonzero with L0001 denied (stderr: {})",
        denied.stderr
    );

    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "lintme"
version = "0.1.0"
[lint]
allow = ["L0001"]
"#,
    );
    let allowed = miku(&["lint"], &dir);
    assert_eq!(
        allowed.status, 0,
        "expected zero with L0001 allowed (stderr: {})",
        allowed.stderr
    );
    assert!(
        !allowed.stderr.contains("L0001"),
        "L0001 should be suppressed: {}",
        allowed.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_runner_summarizes_pass_and_fail() {
    let dir = scratch_dir("test_runner");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "testme"
version = "0.1.0"
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");
    write(&dir, "tests/pass.leek", "// @version:4\nreturn 1;\n");
    // An infinite loop with a tight op budget — interpreter trips
    // TOO_MUCH_OPERATIONS, which `expect-fail` accepts.
    write(
        &dir,
        "tests/fail.leek",
        "// miku-test: expect-fail\n// miku-test: timeout 1000\n// @version:4\nwhile (true) { var x = 1; }\n",
    );

    let out = miku(&["test"], &dir);
    // Both tests should be reported as passing — pass.leek runs
    // clean, fail.leek's expected runtime error materializes.
    assert_eq!(
        out.status, 0,
        "stderr: {}\nstdout: {}",
        out.stderr, out.stdout
    );
    assert!(out.stdout.contains("PASS"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("2 passed"),
        "summary missing: {}",
        out.stdout
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_runner_typed_expectations_pass() {
    let dir = scratch_dir("test_typed_pass");
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"typed\"\nversion = \"0.1.0\"\n",
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");
    // A program meant to be rejected at compile time.
    write(
        &dir,
        "tests/compile_error.leek",
        "// miku-test: expect-compile-error: E0202\n// @version:4\nvar a = 1;\nvar a = 2;\nreturn a;\n",
    );
    write(
        &dir,
        "tests/stack_overflow.leek",
        "// miku-test: expect-runtime-error: STACKOVERFLOW\n// @version:4\nfunction f(n) { return f(n + 1); }\nreturn f(0);\n",
    );
    write(
        &dir,
        "tests/budget.leek",
        "// miku-test: expect-runtime-error: TOO_MUCH_OPERATIONS\n// miku-test: timeout 1000\n// @version:4\nwhile (true) { var x = 1; }\n",
    );
    write(
        &dir,
        "tests/output.leek",
        "// miku-test: expect-output: 3\n// @version:4\nreturn 1 + 2;\n",
    );

    let out = miku(&["test"], &dir);
    assert_eq!(
        out.status, 0,
        "stderr: {}\nstdout: {}",
        out.stderr, out.stdout
    );
    assert!(
        out.stdout.contains("4 passed, 0 failed"),
        "summary: {}",
        out.stdout
    );
    // The expected compile error is the test passing, not noise.
    assert!(!out.stderr.contains("E0202"), "stderr: {}", out.stderr);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_runner_typed_expectations_fail_when_unmet() {
    let dir = scratch_dir("test_typed_fail");
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"typed\"\nversion = \"0.1.0\"\n",
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");
    let cases = [
        (
            "wrong_code",
            "// miku-test: expect-compile-error: E0100\n// @version:4\nvar a = 1;\nvar a = 2;\nreturn a;\n",
        ),
        (
            "compiles",
            "// miku-test: expect-compile-error: E0200\n// @version:4\nreturn 1;\n",
        ),
        // Running out of the default op budget is not "the expected failure".
        (
            "budget_only",
            "// miku-test: expect-fail\n// @version:4\nwhile (true) { var x = 1; }\n",
        ),
        (
            "wrong_runtime",
            "// miku-test: expect-runtime-error: STACKOVERFLOW\n// @version:4\nreturn 1;\n",
        ),
        (
            "wrong_output",
            "// miku-test: expect-output: 4\n// @version:4\nreturn 1 + 2;\n",
        ),
        (
            "bad_timeout",
            "// miku-test: timeout lots\n// @version:4\nreturn 1;\n",
        ),
    ];
    for (name, src) in cases {
        write(&dir, &format!("tests/{name}.leek"), src);
    }

    let out = miku(&["test"], &dir);
    assert_ne!(out.status, 0, "stdout: {}", out.stdout);
    for (name, _) in cases {
        assert!(
            out.stdout.contains(&format!("FAIL tests/{name}.leek")),
            "{name} should fail\nstdout: {}",
            out.stdout
        );
    }
    assert!(
        out.stdout.contains("0 passed, 6 failed"),
        "summary: {}",
        out.stdout
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_help_documents_expectations() {
    let dir = scratch_dir("test_help");
    let out = miku(&["test", "--help"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    for directive in [
        "expect-compile-error",
        "expect-runtime-error",
        "expect-output",
        "expect-fail",
        "timeout",
    ] {
        assert!(
            out.stdout.contains(directive),
            "`miku test --help` should document {directive}: {}",
            out.stdout
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_honors_manifest_lint_levels() {
    // L0013 (double negation) ships a MachineApplicable fix.
    let dir = scratch_dir("fix_levels");
    let source = "// @version:4\nvar a = true;\nreturn !!a;\n";
    write(&dir, "src/main.leek", source);

    // `allow`ed: `miku lint` hides it, so `miku fix` must not apply it.
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"fixme\"\nversion = \"0.1.0\"\n[lint]\nallow = [\"L0013\"]\n",
    );
    let allowed = miku(&["fix"], &dir);
    assert_eq!(allowed.status, 0, "stderr: {}", allowed.stderr);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/main.leek")).unwrap(),
        source,
        "an allowed lint's suggestion must not be applied"
    );

    // `deny`ed: still a lint, not a compile error — the fix applies.
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"fixme\"\nversion = \"0.1.0\"\n[lint]\ndeny = [\"L0013\"]\n",
    );
    let denied = miku(&["fix"], &dir);
    assert_eq!(denied.status, 0, "stderr: {}", denied.stderr);
    let after = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert!(
        !after.contains("!!"),
        "a denied lint's fix should apply: {after}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_skips_files_with_compile_errors() {
    let dir = scratch_dir("fix_errors");
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"fixme\"\nversion = \"0.1.0\"\n",
    );
    // A fixable `!!a`, but the file doesn't resolve (redeclared variable).
    let broken = "// @version:4\nvar a = true;\nvar a = false;\nreturn !!a;\n";
    write(&dir, "src/main.leek", broken);
    let fixable = "// @version:4\nvar b = true;\nreturn !!b;\n";
    write(&dir, "src/ok.leek", fixable);

    let out = miku(&["fix"], &dir);
    assert_ne!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/main.leek")).unwrap(),
        broken,
        "a file with compile errors must not be rewritten"
    );
    assert!(
        out.stderr.contains("skipped 1 file") && out.stderr.contains("src/main.leek"),
        "skipped file should be reported: {}",
        out.stderr
    );
    assert!(out.stderr.contains("E0202"), "stderr: {}", out.stderr);
    assert!(
        !std::fs::read_to_string(dir.join("src/ok.leek"))
            .unwrap()
            .contains("!!"),
        "healthy files are still fixed"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fight_without_scenario_uses_manifest_fight_table() {
    let dir = scratch_dir("fight_manifest_no_scenario");
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");

    // `default_scenario` is what a bare `miku fight` runs.
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"fighter\"\nversion = \"0.1.0\"\n[fight]\ndefault_scenario = \"missing.toml\"\n",
    );
    let out = miku(&["fight"], &dir);
    assert_ne!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("default_scenario") && out.stderr.contains("missing.toml"),
        "stderr: {}",
        out.stderr
    );

    // Without a default, the scenarios in `scenarios_dir` are listed.
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"fighter\"\nversion = \"0.1.0\"\n[fight]\nscenarios_dir = \"scenarios\"\n",
    );
    write(&dir, "scenarios/duel.toml", "");
    write(&dir, "scenarios/arena.json", "{}");
    let out = miku(&["fight"], &dir);
    assert_ne!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("scenarios/duel.toml") && out.stderr.contains("scenarios/arena.json"),
        "stderr: {}",
        out.stderr
    );

    // A mistyped key inside `[fight]` is warned about (the manifest warns on
    // unknown keys in a known table), and the run still fails for want of a
    // scenario rather than silently doing something else.
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"fighter\"\nversion = \"0.1.0\"\n[fight]\ndefault_senario = \"duel.toml\"\n",
    );
    let out = miku(&["fight"], &dir);
    assert_ne!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("fight.default_senario"),
        "stderr: {}",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clean_removes_build_dir() {
    let dir = scratch_dir("clean");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "cleanme"
version = "0.1.0"

[backend.java]
enable  = true
default = true
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");

    let _ = miku(&["build"], &dir);
    assert!(
        dir.join("build").exists(),
        "build/ should exist after build"
    );

    let out = miku(&["clean"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(!dir.join("build").exists(), "build/ should be gone");

    std::fs::remove_dir_all(&dir).ok();
}

/// `miku doc` writes under the build root, so a plain `miku clean`
/// takes the generated pages with it — they used to survive in
/// `target/doc/`.
#[test]
fn clean_removes_generated_docs() {
    let dir = scratch_dir("clean-doc");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "cleandoc"
version = "0.1.0"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4
return 0;
",
    );

    let out = miku(&["doc"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        dir.join("build/doc/index.html").is_file(),
        "docs should land under the build root"
    );

    let out = miku(&["clean"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(!dir.join("build").exists(), "build/ should be gone");

    std::fs::remove_dir_all(&dir).ok();
}

/// `miku clean --doc` sweeps only the documentation.
#[test]
fn clean_doc_keeps_other_build_output() {
    let dir = scratch_dir("clean-doc-only");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "cleandoconly"
version = "0.1.0"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4
return 0;
",
    );

    let out = miku(&["doc"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    write(
        &dir,
        "build/keep.txt",
        "artifact
",
    );

    let out = miku(&["clean", "--doc"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(!dir.join("build/doc").exists(), "build/doc should be gone");
    assert!(
        dir.join("build/keep.txt").is_file(),
        "other build output should survive --doc"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `[paths].build` moves the whole output root, docs included, and
/// `miku clean` follows it.
#[test]
fn build_dir_is_configurable() {
    let dir = scratch_dir("build-dir");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "outdir"
version = "0.1.0"

[paths]
build = "out"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4
return 0;
",
    );

    let out = miku(&["doc"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        dir.join("out/doc/index.html").is_file(),
        "docs should follow [paths].build"
    );
    assert!(!dir.join("build").exists(), "nothing should use build/");

    let out = miku(&["clean"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(!dir.join("out").exists(), "out/ should be gone");

    std::fs::remove_dir_all(&dir).ok();
}

/// `[backend.<kind>].out_dir` is documented as "where each backend writes
/// its artifacts", and the leekscript emitter used to parse it and then
/// ignore it — only the java emitter consulted the manifest.
#[test]
fn backend_out_dir_from_the_manifest_is_honored() {
    let dir = scratch_dir("backend-out-dir");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "emitme"
version = "0.1.0"

[backend.leekscript]
enable  = true
default = true
out_dir = "dist"
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 1 + 2;\n");

    let out = miku(&["build"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        dir.join("dist/main.leek").is_file(),
        "[backend.leekscript].out_dir should place the emitted source; \
         stderr: {}",
        out.stderr
    );
    assert!(
        !dir.join("build/leekscript").exists(),
        "the default <build>/leekscript must not also be written"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A relative `--out-dir` is relative to the project, not to the shell's
/// working directory — otherwise the same command means two different
/// things depending on where in the project it was typed.
#[test]
fn relative_out_dir_resolves_against_the_project_root_from_a_subdirectory() {
    let dir = scratch_dir("out-dir-subdir");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "emitme"
version = "0.1.0"

[backend.leekscript]
enable  = true
default = true
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 1 + 2;\n");

    let out = miku(&["build", "--out-dir", "gen"], &dir.join("src"));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        dir.join("gen/main.leek").is_file(),
        "--out-dir gen should land at <root>/gen; stderr: {}",
        out.stderr
    );
    assert!(
        !dir.join("src/gen").exists(),
        "--out-dir must not resolve against the cwd"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `miku clean` deletes the build root wholesale, so a `paths.build`
/// that escapes the project is a manifest error, not a surprise `rm`.
#[test]
fn escaping_build_dir_is_rejected() {
    let dir = scratch_dir("build-dir-escape");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "escape"
version = "0.1.0"

[paths]
build = "../elsewhere"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4
return 0;
",
    );

    let out = miku(&["clean"], &dir);
    assert_ne!(out.status, 0, "stdout: {}", out.stdout);
    assert!(
        out.stderr.contains("paths.build"),
        "stderr should name the key: {}",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn manifest_warns_about_unknown_nested_key_but_proceeds() {
    let dir = scratch_dir("warn");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name        = "warnme"
version     = "0.1.0"
future_knob = true
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");

    let out = miku(&["check"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("future_knob"),
        "warning missing in stderr: {}",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn init_initializes_in_current_dir() {
    let base = scratch_dir("init");
    let project_dir = base.join("here");
    std::fs::create_dir_all(&project_dir).unwrap();

    let out = miku(&["init"], &project_dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(project_dir.join("Miku.toml").is_file());
    assert!(project_dir.join("src/main.leek").is_file());

    // Re-running init should fail (Miku.toml already exists).
    let again = miku(&["init"], &project_dir);
    assert_ne!(again.status, 0, "expected nonzero on re-init");

    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn json_message_format_emits_ndjson() {
    let dir = scratch_dir("json_diag");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "jsonme"
version = "0.1.0"
"#,
    );
    write(&dir, "src/main.leek", "var var var = $@%\n");

    let out = miku(&["--message-format", "json", "check"], &dir);
    assert_ne!(out.status, 0, "expected nonzero exit");
    // Each line of stdout should be a JSON object with a "code"
    // field.
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert!(
        !lines.is_empty(),
        "expected JSON output\nstdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("each line should parse as JSON");
        assert!(
            parsed.get("code").is_some(),
            "diagnostic missing `code`: {parsed}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fix_skips_maybe_incorrect_suggestions() {
    // `miku fix` is intentionally strict — only MachineApplicable
    // suggestions are applied. L0001's "remove unused variable"
    // ships as MaybeIncorrect (it can change semantics if the var's
    // RHS has side effects), so fix should leave the file alone and
    // exit 0.
    let dir = scratch_dir("fix");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "fixme"
version = "0.1.0"
"#,
    );
    let source = "// @version:4\nvar unused = 42;\nreturn 0;\n";
    write(&dir, "src/main.leek", source);

    let dry = miku(&["fix", "--dry-run"], &dir);
    assert_eq!(
        dry.status, 0,
        "no MachineApplicable fixes available — dry-run should exit 0 (stderr: {})",
        dry.stderr
    );
    let after_dry = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert_eq!(after_dry, source, "dry-run must not write");

    let applied = miku(&["fix"], &dir);
    assert_eq!(applied.status, 0, "stderr: {}", applied.stderr);
    let after = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert_eq!(
        after, source,
        "MaybeIncorrect suggestion must not be auto-applied"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn completions_emit_bash_script() {
    let dir = scratch_dir("compl");
    let out = miku(&["completions", "bash"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("_miku()") || out.stdout.contains("complete"),
        "bash completion missing markers: {}",
        out.stdout
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn junit_xml_includes_failure_node() {
    let dir = scratch_dir("junit");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "junitme"
version = "0.1.0"
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");
    write(&dir, "tests/pass.leek", "// @version:4\nreturn 1;\n");
    write(
        &dir,
        "tests/fail.leek",
        "// @version:4\nvar var var = $@%\n",
    );

    let out = miku(&["test", "--message-format", "junit"], &dir);
    assert_ne!(out.status, 0, "expected nonzero (one failing test)");
    assert!(
        out.stdout.contains("<testsuite"),
        "missing <testsuite>: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("<failure"),
        "missing <failure>: {}",
        out.stdout
    );
    // Pass-only test should NOT show up wrapped in a failure node.
    assert!(out.stdout.contains("tests/pass.leek"));
    assert!(out.stdout.contains("tests/fail.leek"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn gitignore_excludes_matching_sources() {
    let dir = scratch_dir("gitignore");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "ignme"
version = "0.1.0"
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");
    // A second source under src/ that we'll gitignore.
    write(&dir, "src/legacy.leek", "// @version:4\nvar ignored=1;\n");
    write(&dir, ".gitignore", "src/legacy.leek\n");

    let out = miku(&["fmt", "--check"], &dir);
    // legacy.leek would otherwise be reported as unformatted; if
    // gitignore is honored, fmt --check returns success.
    assert_eq!(
        out.status, 0,
        "fmt --check should ignore gitignored sources (stderr: {}, stdout: {})",
        out.stderr, out.stdout
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn unknown_top_level_key_is_an_error() {
    let dir = scratch_dir("err_top");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "errme"
version = "0.1.0"

[moonbeam]
x = 1
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");

    let out = miku(&["check"], &dir);
    assert_ne!(out.status, 0, "expected nonzero exit");
    assert!(
        out.stderr.contains("moonbeam"),
        "expected error to mention moonbeam: {}",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrate_uses_manifest_language_for_pragmaless_files() {
    // A pragma-less file in a `language = 1` project is v1 source, the
    // same as every other subcommand reads it. `migrate` used to assume v4
    // and skip it as "already at target".
    let dir = scratch_dir("migrate_manifest_lang");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name     = "migme"
version  = "0.1.0"
language = 1
"#,
    );
    write(&dir, "src/main.leek", "var x = 5\nx ^= 2\nreturn x\n");

    let out = miku(&["migrate", "--to", "v2"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);

    let after = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert!(after.contains("x **= 2"), "operator not rewritten: {after}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrate_rewrites_v1_caret_assign_to_starstar() {
    let dir = scratch_dir("migrate_v1_v2");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "migme"
version = "0.1.0"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:1\nvar x = 5\nx ^= 2\nreturn x\n",
    );

    let out = miku(&["migrate", "--to", "v2"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);

    let after = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert!(
        after.contains("// @version:2"),
        "pragma not bumped: {after}"
    );
    assert!(after.contains("x **= 2"), "operator not rewritten: {after}");
    assert!(!after.contains("^="), "stale ^=: {after}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrate_full_chain_to_v4_preserves_comments() {
    let dir = scratch_dir("migrate_chain");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "chainme"
version = "0.1.0"
"#,
    );
    let src = "// @version:1\n\
// power up\nvar x = 3\nx ^= 2\n\
// pick a slice\nvar head = subArray([1, 2, 3, 4], 0, 2)\n\
return head\n";
    write(&dir, "src/main.leek", src);

    let out = miku(&["migrate", "--to", "v4"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);

    let after = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert!(after.starts_with("// @version:4"), "pragma: {after}");
    assert!(after.contains("// power up"), "lost comment: {after}");
    assert!(after.contains("// pick a slice"), "lost comment: {after}");
    assert!(after.contains("x **= 2"));
    assert!(
        after.contains("arraySlice([1, 2, 3, 4], 0, (2) + 1)"),
        "missing semantic-preserving subArray rewrite: {after}",
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrate_dry_run_does_not_modify_file_and_returns_nonzero() {
    let dir = scratch_dir("migrate_dry");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "dryme"
version = "0.1.0"
"#,
    );
    let original = "// @version:1\nvar x = 2\nx ^= 3\nreturn x\n";
    write(&dir, "src/main.leek", original);

    let out = miku(&["migrate", "--to", "v2", "--dry-run"], &dir);
    // A dry-run that would change a file must exit non-zero.
    assert_ne!(out.status, 0, "expected non-zero exit for dry-run changes");
    assert!(
        out.stderr.contains("would migrate"),
        "expected dry-run banner: {}",
        out.stderr
    );
    let after = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert_eq!(after, original, "dry-run modified the file");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrate_v4_to_v1_downgrades_all_passes() {
    let dir = scratch_dir("migrate_downgrade");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "down"
version = "0.1.0"
"#,
    );
    let src = "// @version:4\n\
var x = 5\nx **= 2\n\
var y = 5\ny ^= 3\n\
var head = arraySlice([10, 20, 30, 40], 0, 3)\n\
return [x, y, head]\n";
    write(&dir, "src/main.leek", src);

    let out = miku(&["migrate", "--to", "v1"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);

    let after = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert!(after.starts_with("// @version:1"), "pragma: {after}");
    // **=  → ^= (power-assign in v1)
    assert!(after.contains("x ^= 2"), "power-assign downgrade: {after}");
    // ^= xor-assign → expanded long form (because in v1 the same
    // token would mean power-assign).
    assert!(after.contains("y = y ^ (3)"), "xor expansion: {after}");
    // arraySlice (exclusive end) → subArray (inclusive end)
    assert!(
        after.contains("subArray([10, 20, 30, 40], 0, (3) - 1)"),
        "subArray end fix: {after}",
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn profile_reports_unavailable() {
    // `miku profile` relied on the interpreter's per-call-stack ops profiler;
    // the interpreter backend was removed and the native JIT has no
    // equivalent, so the command must fail loudly (not pretend to profile).
    let dir = scratch_dir("profile_table");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "p"
version = "0.1.0"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4\nfunction work(arr) {\n    var t = 0\n    for (var x in arr) { t = t + x }\n    return t\n}\nreturn work([0, 1, 2, 3, 4, 5, 6, 7, 8, 9])\n",
    );

    let out = miku(&["profile"], &dir);
    assert_ne!(out.status, 0, "stdout: {}", out.stdout);
    assert!(
        out.stderr.contains("`miku profile` is unavailable"),
        "stderr: {}",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn doc_generates_html_with_signatures_and_complexity() {
    let dir = scratch_dir("doc");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "docme"
version = "0.1.0"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4\n\
         /** Sum the items. */\n\
         function sum(arr) {\n    var t = 0\n    for (var x in arr) { t = t + x }\n    return t\n}\n\
         return sum([1, 2, 3])\n",
    );

    let out = miku(&["doc"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(dir.join("build/doc/index.html").is_file(), "missing index");
    assert!(
        !dir.join("target").exists(),
        "doc must not write outside the build root"
    );
    let pages: Vec<_> = std::fs::read_dir(dir.join("build/doc"))
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    assert!(
        pages.len() >= 2,
        "expected index + at least one page, got {pages:?}"
    );

    let main_page = pages
        .iter()
        .find(|p| p.file_name().unwrap().to_string_lossy().contains("main"))
        .expect("main page");
    let html = std::fs::read_to_string(main_page).unwrap();
    assert!(html.contains("function sum"), "missing signature: {html}");
    assert!(html.contains("Sum the items"), "missing doc text: {html}");
    assert!(
        html.contains("Complexity:"),
        "missing complexity row: {html}"
    );
    assert!(html.contains("O(arr)"), "missing big-O: {html}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migrate_skips_files_already_at_target() {
    let dir = scratch_dir("migrate_noop");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "noopme"
version = "0.1.0"
"#,
    );
    let original = "// @version:4\nreturn 1\n";
    write(&dir, "src/main.leek", original);

    let out = miku(&["migrate", "--to", "v4"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("already at v4") || out.stderr.contains("0 file"),
        "expected skip note: {}",
        out.stderr
    );
    let after = std::fs::read_to_string(dir.join("src/main.leek")).unwrap();
    assert_eq!(after, original, "no-op migration should be byte-identical");

    std::fs::remove_dir_all(&dir).ok();
}

/// A scenario with two idle leeks: no AI files, ends in a draw at the turn
/// limit. Enough to exercise the `miku fight` plumbing without compiling.
const IDLE_DUEL: &str = r"seed = 1
max_turns = 2

[map]
width = 10
height = 10

[[entities]]
id = 1
team = 0
cell = 0

[[entities]]
id = 2
team = 1
cell = 99
";

#[test]
fn fight_uses_manifest_fight_table() {
    let dir = scratch_dir("fight_manifest");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "fights"
version = "0.1.0"

[fight]
default_scenario = "duel.toml"
scenarios_dir = "scenarios"
reports_dir = "out/reports"
jobs = 2
"#,
    );
    write(&dir, "scenarios/duel.toml", IDLE_DUEL);

    // No scenario argument: `[fight].default_scenario` resolved under
    // `[fight].scenarios_dir`. Bare `--report`: `[fight].reports_dir`.
    let out = miku(&["fight", "--report"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("draw"), "stdout: {}", out.stdout);

    let report = dir.join("out/reports/single.json");
    assert!(
        report.is_file(),
        "expected a report at {}; stderr: {}",
        report.display(),
        out.stderr
    );
    let json = std::fs::read_to_string(&report).unwrap();
    assert!(json.contains("\"turns\""), "report body: {json}");

    // An explicit path wins over `[fight].reports_dir`.
    let out = miku(&["fight", "--report=elsewhere/run.json"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(dir.join("elsewhere/run.json").is_file());

    std::fs::remove_dir_all(&dir).ok();
}

/// `-j/--jobs` and `[fight].jobs` (#134) pick how many threads the multi-fight
/// modes spread their fights over. The count is a speed knob and nothing else,
/// so the reports have to come out byte-identical — including the order of the
/// `cells` array, which is what every downstream comparison reads.
#[test]
fn fight_jobs_selects_workers_without_changing_the_report() {
    let dir = scratch_dir("fight_jobs");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "jobs"
version = "0.1.0"

[fight]
jobs = 3
"#,
    );
    write(&dir, "duel.toml", IDLE_DUEL);
    write(&dir, "a.leek", "return 0;\n");
    write(&dir, "b.leek", "return 1;\n");

    let sweep = |extra: &[&str], report: &str| -> String {
        let mut args = vec!["fight", "duel.toml", "--mode", "matrix"];
        args.extend(["--seeds", "1,2,3,4,5,6", "--vs", "a.leek", "--vs", "b.leek"]);
        args.extend_from_slice(extra);
        args.push(report);
        let out = miku(&args, &dir);
        assert_eq!(out.status, 0, "stderr: {}", out.stderr);
        std::fs::read_to_string(dir.join(report.trim_start_matches("--report=")))
            .expect("the report was written")
    };

    let serial = sweep(&["-j", "1"], "--report=j1.json");
    let parallel = sweep(&["--jobs", "8"], "--report=j8.json");
    // No flag: the manifest's `[fight].jobs = 3` is what is left to decide it.
    let from_manifest = sweep(&[], "--report=manifest.json");

    assert!(serial.contains("\"cells\""), "report body: {serial}");
    assert_eq!(serial, parallel, "-j changed the report");
    assert_eq!(serial, from_manifest, "[fight].jobs changed the report");

    std::fs::remove_dir_all(&dir).ok();
}

/// Regression (#66): a tournament has no hero, so the run no longer claims
/// one — the summary drops the win rate, the JSON leaves the hero totals null,
/// and the exit status gates on the games having run rather than always
/// passing.
#[test]
fn fight_tournament_reports_a_leaderboard_not_a_hero_verdict() {
    let dir = scratch_dir("fight_tournament");
    write(&dir, "duel.toml", IDLE_DUEL);
    write(&dir, "a.leek", "return 0;\n");
    write(&dir, "b.leek", "return 1;\n");

    let args = |x: &'static str| {
        vec![
            "fight",
            "duel.toml",
            "--mode",
            "tournament",
            "--entrant",
            x,
            "--entrant",
            "b.leek",
            "--report=out.json",
        ]
    };
    let out = miku(&args("a.leek"), &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(
        !out.stdout.contains("win rate") && !out.stdout.contains("losses"),
        "no hero, no win/loss summary: {}",
        out.stdout
    );
    assert!(out.stdout.contains("leaderboard"), "stdout: {}", out.stdout);

    let json = std::fs::read_to_string(dir.join("out.json")).unwrap();
    for key in [
        "\"scoring\": \"leaderboard\"",
        "\"wins\": null",
        "\"losses\": null",
        "\"win_rate\": null",
    ] {
        assert!(json.contains(key), "expected {key} in {json}");
    }

    // A tournament whose games can't be run is a failed run, not a silent
    // success.
    let out = miku(&args("missing.leek"), &dir);
    assert_eq!(out.status, 1, "stdout: {}", out.stdout);

    std::fs::remove_dir_all(&dir).ok();
}

/// Regression (#91): `[testing] games` was parsed and never read, and the
/// `--games` flag was really a seed list, so a scenario asking for three games
/// per pairing silently played one and `--seeds` was ignored in tournament
/// mode. Both now say what they mean.
#[test]
fn fight_tournament_games_and_seeds_mean_what_they_say() {
    let dir = scratch_dir("fight_tournament_games");
    write(&dir, "a.leek", "return 0;\n");
    write(&dir, "b.leek", "return 1;\n");
    write(&dir, "duel.toml", IDLE_DUEL);
    write(
        &dir,
        "games.toml",
        &format!("{IDLE_DUEL}\n[testing]\ngames = 3\n"),
    );

    // The seeds the report's cells actually played, deduplicated in order.
    let seeds_of = |name: &str| -> Vec<u64> {
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(name)).unwrap())
                .expect("the report is JSON");
        let mut seeds: Vec<u64> = json["cells"]
            .as_array()
            .expect("cells")
            .iter()
            .map(|c| c["seed"].as_u64().expect("each cell names its seed"))
            .collect();
        seeds.dedup();
        seeds
    };
    let run = |extra: &[&str], scenario: &str, report: &str| {
        let mut args = vec!["fight", scenario, "--mode", "tournament"];
        args.extend(["--entrant", "a.leek", "--entrant", "b.leek"]);
        args.extend_from_slice(extra);
        args.push(report);
        let out = miku(&args, &dir);
        assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    };

    // `[testing] games = 3`: three seeds, each played from both sides.
    run(&[], "games.toml", "--report=file-games.json");
    let seeds = seeds_of("file-games.json");
    assert_eq!(seeds.len(), 3, "three distinct seeds, got {seeds:?}");
    assert_eq!(seeds[0], 1, "the first game keeps the scenario's seed");

    // `--games 3` does the same from the command line.
    run(&["--games", "3"], "duel.toml", "--report=cli-games.json");
    assert_eq!(seeds_of("cli-games.json"), seeds);

    // `--seeds` is honoured in tournament mode, and overrides the file's games.
    run(&["--seeds", "7,8"], "games.toml", "--report=seeds.json");
    assert_eq!(seeds_of("seeds.json"), vec![7, 8]);

    // A seed list and a game count contradict each other; saying both fails
    // loudly instead of silently picking one.
    let out = miku(
        &[
            "fight",
            "duel.toml",
            "--mode",
            "tournament",
            "--entrant",
            "a.leek",
            "--entrant",
            "b.leek",
            "--games",
            "2",
            "--seeds",
            "1,2",
        ],
        &dir,
    );
    assert_ne!(out.status, 0, "stdout: {}", out.stdout);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fight_without_default_scenario_explains_itself() {
    let dir = scratch_dir("fight_no_scenario");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name = "fights"
version = "0.1.0"
"#,
    );

    let out = miku(&["fight"], &dir);
    assert_ne!(out.status, 0, "stdout: {}", out.stdout);
    assert!(
        out.stderr.contains("default_scenario"),
        "stderr: {}",
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---- include-using projects ----
//
// Every command below drives the multi-file front end. They are grouped in
// one place because the failure they guard against (DRIVER-02: a command
// that plans its pipeline *without* the include step, so included symbols
// resolve against an empty file set) is per-command, not per-project.

/// A two-file project: `src/main.leek` includes `src/helper.leek` and calls
/// a function defined there.
fn include_project(label: &str) -> PathBuf {
    let dir = scratch_dir(label);
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "included"
version = "0.1.0"
"#,
    );
    write(
        &dir,
        "src/main.leek",
        "// @version:4\ninclude(\"helper\");\nreturn twice(21);\n",
    );
    write(
        &dir,
        "src/helper.leek",
        "// @version:4\nfunction twice(x) { return x * 2; }\n",
    );
    dir
}

#[test]
fn include_project_passes_check_lint_and_fix() {
    let dir = include_project("include_check");
    for args in [&["check"][..], &["lint"][..], &["fix"][..]] {
        let out = miku(args, &dir);
        assert_eq!(out.status, 0, "miku {args:?} stderr: {}", out.stderr);
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn include_project_runs_the_included_function() {
    let dir = include_project("include_run");
    let out = miku(&["run"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    // `twice(21)` — proof the included body was compiled, not just accepted.
    assert_eq!(out.stdout.trim(), "42", "stderr: {}", out.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn include_project_builds_leekscript_that_compiles_on_its_own() {
    let dir = include_project("include_build");
    let out = miku(&["build", "--backend", "leekscript"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);

    let emitted = dir.join("build/leekscript/main.leek");
    assert!(emitted.is_file(), "stderr: {}", out.stderr);
    let text = std::fs::read_to_string(&emitted).expect("read emitted");
    assert!(
        text.contains("function twice"),
        "the included definition must be inlined into the single emitted \
         file, since leek-wars takes one file:\n{text}"
    );

    // The emitted file must itself be a valid, self-contained program.
    let round_trip = scratch_dir("include_build_rt");
    write(
        &round_trip,
        "Miku.toml",
        "[project]\nname = \"rt\"\nversion = \"0.1.0\"\n",
    );
    std::fs::create_dir_all(round_trip.join("src")).expect("src dir");
    std::fs::copy(&emitted, round_trip.join("src/main.leek")).expect("copy emitted");
    let recheck = miku(&["run"], &round_trip);
    assert_eq!(recheck.status, 0, "stderr: {}", recheck.stderr);
    assert_eq!(recheck.stdout.trim(), "42");

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&round_trip).ok();
}

#[test]
fn analyze_reports_a_complexity_row_per_function() {
    let dir = include_project("analyze");
    let out = miku(&["analyze"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("twice"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("O("), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("src/helper.leek"),
        "every project source is analyzed, not only the entry: {}",
        out.stdout
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn analyze_does_not_double_count_included_functions() {
    // `analyze` resolves includes like `check` does, so `src/main.leek`'s
    // report now also covers everything `src/helper.leek` declares. Each
    // file is still walked in its own right, so a function must appear
    // exactly once — under the file that declares it.
    let dir = include_project("analyze_dedup");
    let out = miku(&["analyze"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert_eq!(
        out.stdout.matches("twice(x)").count(),
        1,
        "`twice` is declared in src/helper.leek only:\n{}",
        out.stdout
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn doc_documents_the_declarations_of_the_file_it_is_writing() {
    // Same guard for `miku doc`: the entry's page must not sprout the
    // included file's declarations now that includes resolve.
    let dir = include_project("doc_include");
    let out = miku(&["doc"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);

    let entry_page =
        std::fs::read_to_string(dir.join("build/doc/src--main.html")).expect("entry page");
    assert!(
        !entry_page.contains("twice"),
        "`twice` is declared in src/helper.leek:\n{entry_page}"
    );
    let helper_page =
        std::fs::read_to_string(dir.join("build/doc/src--helper.html")).expect("helper page");
    assert!(helper_page.contains("twice"), "{helper_page}");
    assert!(helper_page.contains("O("), "{helper_page}");

    std::fs::remove_dir_all(&dir).ok();
}

/// A project whose `tests/` file is the one driving the front end.
/// `manifest_extra` is appended to the `[project]` table.
fn include_test_project(label: &str, manifest_extra: &str, test_body: &str) -> PathBuf {
    let dir = scratch_dir(label);
    write(
        &dir,
        "Miku.toml",
        &format!("[project]\nname    = \"included\"\nversion = \"0.1.0\"\n{manifest_extra}"),
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");
    write(
        &dir,
        "src/helper.leek",
        "// @version:4\nfunction twice(x) { return x * 2; }\n",
    );
    write(&dir, "tests/t.leek", test_body);
    dir
}

#[test]
fn test_runs_a_test_that_includes_a_helper() {
    // A test file goes through the same driver pipeline as the entry, so
    // its `include(...)` resolves and `twice` is a real call rather than an
    // unbound name the native backend refuses to compile.
    let dir = include_test_project(
        "test_include",
        "",
        "// @version:4\n// miku-test: expect-output: 42\n\
         include(\"../src/helper\");\nreturn twice(21);\n",
    );
    let out = miku(&["test"], &dir);
    assert_eq!(
        out.status, 0,
        "stdout: {}\nstderr: {}",
        out.stdout, out.stderr
    );
    assert!(
        out.stdout.contains("PASS tests/t.leek"),
        "stdout: {}",
        out.stdout
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_and_test_agree_on_a_missing_include() {
    // E0272 is an error wherever it is raised. `check` sees it in the
    // entry; `test` must see it in a test file too, instead of running a
    // program whose `include` silently did nothing.
    const MISSING: &str = "// @version:4\ninclude(\"nope\");\nreturn 1;\n";
    let dir = include_test_project("missing_include", "", MISSING);
    write(&dir, "src/main.leek", MISSING);

    let check = miku(&["check"], &dir);
    assert_ne!(check.status, 0, "stdout: {}", check.stdout);
    assert!(check.stderr.contains("E0272"), "stderr: {}", check.stderr);

    let test = miku(&["test"], &dir);
    assert_ne!(test.status, 0, "stdout: {}", test.stdout);
    assert!(test.stderr.contains("E0272"), "stderr: {}", test.stderr);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn test_honors_manifest_lint_groups_like_check() {
    // L0028 is a pedantic lint this manifest switches on and denies. It
    // has to fail a `tests/` file for the same reason it fails
    // `src/main.leek`: both plan their pipeline through the driver, which
    // merges the manifest's opt-in lint groups into the recipe params.
    const COLLAPSIBLE: &str =
        "// @version:4\nvar a = 1;\nif (a > 0) { if (a < 9) { a = 2; } }\nreturn a;\n";
    let dir = include_test_project(
        "manifest_lints",
        "\n[lint]\npedantic = true\ndeny = [\"L0028\"]\n",
        COLLAPSIBLE,
    );
    write(&dir, "src/main.leek", COLLAPSIBLE);

    let check = miku(&["check"], &dir);
    assert_ne!(check.status, 0, "stdout: {}", check.stdout);
    assert!(check.stderr.contains("L0028"), "stderr: {}", check.stderr);

    let test = miku(&["test"], &dir);
    assert_ne!(test.status, 0, "stdout: {}", test.stdout);
    assert!(test.stderr.contains("L0028"), "stderr: {}", test.stderr);

    std::fs::remove_dir_all(&dir).ok();
}

// ---- manifest location ----

#[test]
fn manifest_path_works_from_an_unrelated_directory() {
    let dir = include_project("manifest_path");
    let elsewhere = scratch_dir("manifest_path_cwd");

    let manifest = dir.join("Miku.toml");
    let out = miku(
        &[
            "--manifest-path",
            manifest.to_str().expect("utf-8"),
            "check",
        ],
        &elsewhere,
    );
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&elsewhere).ok();
}

#[test]
fn discovery_walks_up_from_a_subdirectory() {
    let dir = include_project("discover_subdir");
    // Running from `src/` (or anywhere below the root) must find the
    // ancestor manifest, the way `cargo` does.
    let out = miku(&["check"], &dir.join("src"));
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn manifest_path_to_a_directory_fails_with_a_message_naming_it() {
    // Today this is an error (DRIVER-09 / #276 owns making it work); it must
    // at least say which path it could not read rather than surfacing a bare
    // "Is a directory".
    let dir = include_project("manifest_path_dir");
    let out = miku(
        &["--manifest-path", dir.to_str().expect("utf-8"), "check"],
        &dir,
    );
    assert_ne!(out.status, 0, "stdout: {}", out.stdout);
    assert!(
        out.stderr.contains(dir.to_str().expect("utf-8")),
        "stderr: {}",
        out.stderr
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore = "DRIVER-09 (#276): --manifest-path pointing at a directory should find its Miku.toml"]
fn manifest_path_accepts_a_directory() {
    let dir = include_project("manifest_path_dir_ok");
    let out = miku(
        &["--manifest-path", dir.to_str().expect("utf-8"), "check"],
        &dir,
    );
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    std::fs::remove_dir_all(&dir).ok();
}

// ---- host libraries ----

#[test]
fn library_flag_dispatches_host_functions_through_their_class() {
    let dir = scratch_dir("library");
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn getCell();\n");

    // Without `--library`, the backend has no dispatch class for `getCell`
    // and nothing but a bare `getCell(...)` to write — which is not a method
    // on the generated class. That used to be written out and reported as a
    // successful build; it now fails and says where (JAVA-06 / #152).
    let plain = miku(&["build", "--backend", "java", "--color", "never"], &dir);
    assert_eq!(plain.status, 1, "stderr: {}", plain.stderr);
    assert!(
        plain.stderr.contains("error[E0610]") && plain.stderr.contains("`getCell`"),
        "stderr: {}",
        plain.stderr
    );
    let emitted = dir.join("build/java/AI_0.java");
    assert!(
        !emitted.exists(),
        "a .java the backend could not translate was written anyway"
    );

    let with_lib = miku(
        &["--library", "leekwars", "build", "--backend", "java"],
        &dir,
    );
    assert_eq!(with_lib.status, 0, "stderr: {}", with_lib.stderr);
    let with = std::fs::read_to_string(&emitted).expect("read emitted");
    assert!(
        with.contains("EntityClass.getCell"),
        "stdout: {}\n{with}",
        with_lib.stdout
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---- dev ----

#[test]
fn dev_pipeline_prints_a_timing_per_front_end_pass() {
    let dir = include_project("dev_pipeline");
    let out = miku(&["dev", "pipeline", "src/main.leek"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    let text = format!("{}{}", out.stdout, out.stderr);
    for step in [
        "pragma",
        "lex",
        "parse",
        "resolve",
        "type-check",
        "lower-hir",
    ] {
        assert!(text.contains(step), "no timing for `{step}`:\n{text}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// `[test] timeout` is an op budget the runner actually enforces, and the
/// per-file `// miku-test: timeout` annotation still wins over it.
#[test]
fn manifest_test_timeout_is_the_default_op_budget() {
    let dir = scratch_dir("test_timeout");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name    = "budgeted"
version = "0.1.0"

[test]
timeout = 1000
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");
    // No annotation: the manifest budget applies, so the hot loop trips
    // TOO_MUCH_OPERATIONS and `expect-fail` accepts it.
    write(
        &dir,
        "tests/hot.leek",
        "// miku-test: expect-fail\n// @version:4\nwhile (true) { var x = 1; }\n",
    );
    // A per-file annotation overrides the manifest — a generous budget here
    // means this quick test finishes normally rather than being cut off.
    write(
        &dir,
        "tests/quick.leek",
        "// miku-test: timeout 10000000\n// @version:4\nvar t = 0;\nfor (var i = 0; i < 2000; i++) { t += i; }\nreturn 1;\n",
    );

    let out = miku(&["test"], &dir);
    assert_eq!(
        out.status, 0,
        "stderr: {}\nstdout: {}",
        out.stderr, out.stdout
    );
    assert!(
        out.stdout.contains("2 passed"),
        "stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Without the manifest budget the same hot loop runs on the much larger
/// default, so `[test] timeout` really is what cut it short above.
#[test]
fn without_a_manifest_timeout_the_default_budget_applies() {
    let dir = scratch_dir("test_timeout_absent");
    write(
        &dir,
        "Miku.toml",
        "[project]\nname = \"unbudgeted\"\nversion = \"0.1.0\"\n",
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");
    // 2000 iterations is far past a 1000-op budget and far short of the
    // backend default, so this passes only when no manifest budget is set.
    write(
        &dir,
        "tests/quick.leek",
        "// @version:4\nvar t = 0;\nfor (var i = 0; i < 2000; i++) { t += i; }\nreturn 1;\n",
    );

    let out = miku(&["test"], &dir);
    assert_eq!(
        out.status, 0,
        "stderr: {}\nstdout: {}",
        out.stderr, out.stdout
    );
    assert!(out.stdout.contains("1 passed"), "stdout: {}", out.stdout);

    std::fs::remove_dir_all(&dir).ok();
}

/// `[project]`'s metadata keys are what `miku doc`'s index renders — before
/// this they parsed into fields nothing read.
#[test]
fn doc_index_renders_project_metadata() {
    let dir = scratch_dir("doc_meta");
    write(
        &dir,
        "Miku.toml",
        r#"[project]
name        = "documented"
version     = "1.2.3"
description = "A leek that fights politely."
authors     = ["Ada Lovelace"]
license     = "MIT"
repository  = "https://example.invalid/documented"
"#,
    );
    write(&dir, "src/main.leek", "// @version:4\nreturn 0;\n");

    let out = miku(&["doc"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    let index = std::fs::read_to_string(dir.join("build/doc/index.html")).expect("index.html");
    assert!(
        index.contains("A leek that fights politely."),
        "description missing:\n{index}"
    );
    assert!(index.contains("Ada Lovelace"), "author missing:\n{index}");
    assert!(index.contains("MIT"), "license missing:\n{index}");
    assert!(
        index.contains("https://example.invalid/documented"),
        "repository missing:\n{index}"
    );
    assert!(index.contains("v1.2.3"), "version missing:\n{index}");

    std::fs::remove_dir_all(&dir).ok();
}
