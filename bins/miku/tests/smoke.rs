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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
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
        out.stderr.contains("unknown diagnostic code")
            && out.stderr.contains("available for"),
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

[backend.interp]
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
fn profile_table_lists_user_functions() {
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
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("work"), "stdout: {}", out.stdout);
    assert!(out.stdout.contains("self ops"), "stdout: {}", out.stdout);

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
    assert!(dir.join("target/doc/index.html").is_file(), "missing index");
    let pages: Vec<_> = std::fs::read_dir(dir.join("target/doc"))
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
fn profile_folded_format_is_flamegraph_ready() {
    let dir = scratch_dir("profile_folded");
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
        "// @version:4\n\
         function a(arr) {\n    var t = 0\n    for (var x in arr) { t = t + x }\n    return t\n}\n\
         function b(arr) { return a(arr) }\n\
         return b([0, 1, 2, 3, 4, 5, 6, 7, 8, 9])\n",
    );

    let out = miku(&["profile", "--format", "folded"], &dir);
    assert_eq!(out.status, 0, "stderr: {}", out.stderr);
    // Each line must have `name;name;... N` shape with N > 0.
    let mut saw_nested = false;
    for line in out.stdout.lines() {
        let last_space = line.rfind(' ').expect("line missing count");
        let n: u64 = line[last_space + 1..].parse().expect("count is u64");
        assert!(n > 0, "zero count on line: {line}");
        let stack_part = &line[..last_space];
        assert!(!stack_part.is_empty());
        if stack_part.contains(';') {
            saw_nested = true;
        }
    }
    assert!(
        saw_nested,
        "folded output should include at least one nested-stack line: {}",
        out.stdout
    );
    assert!(out.stdout.contains('a'), "stdout: {}", out.stdout);

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
