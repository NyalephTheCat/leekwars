//! `miku test` — run every `.leek` file under `tests/` through the
//! native JIT.
//!
//! Recognized `// miku-test:` directives in the leading comment block:
//! ```text
//! // miku-test: expect-pass                   (default if absent)
//! // miku-test: expect-output: <text>         (runs clean; result prints as <text>)
//! // miku-test: expect-compile-error: <CODE>  (compilation reports error CODE)
//! // miku-test: expect-runtime-error: <CODE>  (the run fails with CODE, e.g.
//! //                                           STACKOVERFLOW, TOO_MUCH_OPERATIONS)
//! // miku-test: expect-fail                   (any runtime error; op-budget
//! //                                           exhaustion only with `timeout`)
//! // miku-test: timeout <ops>                 (op budget — integer)
//! ```
//! An unknown, malformed, conflicting or misplaced (after the first code
//! line) directive fails the test instead of being silently ignored.
//!
//! Output:
//! - `--message-format human` (default): one `PASS`/`FAIL` line per
//!   test, plus a final summary on stdout.
//! - `--message-format junit`: a JUnit-style XML report on stdout
//!   (or written to `[test].junit_xml` if the manifest set it).
//!   Per-test `PASS`/`FAIL` lines are suppressed; the human summary
//!   still prints to stderr unless `--quiet`.
//! - `--message-format json`: same as human for tests, but per-file
//!   compile diagnostics are emitted as NDJSON through the reporter.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use leek_backend_native::{NativeArtifact, NativeError};
use leek_hir::pipeline::HirArtifact;
use leek_span::SourceId;

use leek_diagnostics::{Code, Reporter, Severity};
use leek_pipeline::Input;
use leek_project::Project;

use crate::cli::{ColorWhen, MessageFormat, Test};
use crate::util::reporter_from_cli;

/// The runtime error the native backend records when the op budget runs out.
const BUDGET_EXHAUSTED: &str = "TOO_MUCH_OPERATIONS";

pub fn run(
    args: &Test,
    manifest_path: Option<&Path>,
    color: ColorWhen,
    format: MessageFormat,
    quiet: bool,
) -> Result<ExitCode> {
    let project = Project::discover(manifest_path)?;
    for w in &project.warnings {
        eprintln!("warning: {w}");
    }
    let reporter = reporter_from_cli(color, format, &project.manifest.lint)?;

    let tests = project.walk_tests();
    if tests.is_empty() {
        if !quiet {
            eprintln!("miku: no tests found in {}", project.tests_dir().display());
        }
        if matches!(format, MessageFormat::Junit) {
            write_junit_output(&project, &[])?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let mut records: Vec<TestRecord> = Vec::new();
    for (next_source, path) in (1_u32..).zip(&tests) {
        let source = SourceId::new(next_source).unwrap();
        let start = Instant::now();
        let outcome = run_one(&project, &reporter, source, path)?;
        let duration = start.elapsed();

        let rel = display_relative(&project.root, path);
        let is_pass = matches!(outcome, TestOutcome::Pass);
        let reason = match &outcome {
            TestOutcome::Pass => None,
            TestOutcome::Fail(r) => Some(r.clone()),
        };

        // Per-test line for the human formats only — JUnit suppresses
        // it so stdout stays valid XML.
        if !quiet && !matches!(format, MessageFormat::Junit) {
            if is_pass {
                println!("PASS {}", rel.display());
            } else {
                println!(
                    "FAIL {} — {}",
                    rel.display(),
                    reason.as_deref().unwrap_or("")
                );
            }
        }

        records.push(TestRecord {
            name: rel.display().to_string(),
            duration_s: duration.as_secs_f64(),
            failure: reason,
        });

        if !is_pass && args.fail_fast {
            break;
        }
    }

    let passed = records.iter().filter(|r| r.failure.is_none()).count();
    let failed = records.len() - passed;

    if matches!(format, MessageFormat::Junit) {
        write_junit_output(&project, &records)?;
        if !quiet {
            eprintln!(
                "miku test: {passed} passed, {failed} failed ({} total)",
                records.len()
            );
        }
    } else if !quiet {
        println!(
            "\nmiku test: {passed} passed, {failed} failed ({} total)",
            records.len()
        );
    }

    Ok(if failed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

#[derive(Debug, PartialEq, Eq)]
enum TestOutcome {
    Pass,
    Fail(String),
}

struct TestRecord {
    name: String,
    duration_s: f64,
    /// `None` = passing.
    failure: Option<String>,
}

fn run_one(
    project: &Project,
    reporter: &Reporter,
    source: SourceId,
    path: &Path,
) -> Result<TestOutcome> {
    let (src, text) = project.pipeline_input(source, path)?;
    let input = Input::from(src);
    let annotations = parse_annotations(&text);
    if !annotations.problems.is_empty() {
        return Ok(TestOutcome::Fail(format!(
            "invalid miku-test directive: {}",
            annotations.problems.join("; ")
        )));
    }

    let pipeline =
        leek_recipes::pipeline(leek_recipes::Target::Linted, &leek_recipes::driver_params())
            .expect("recipe");
    let result = pipeline.run(input);
    let label = path.display().to_string();

    if let Expectation::CompileError(code) = &annotations.expect {
        let errors: Vec<&'static str> = reporter
            .apply_levels(result.diagnostics())
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code.id())
            .collect();
        // The expected error is the test passing: don't render it.
        if errors.contains(&code.as_str()) {
            return Ok(TestOutcome::Pass);
        }
        reporter.emit_run(result.diagnostics(), &text, &label);
        return Ok(TestOutcome::Fail(if errors.is_empty() {
            format!("expected compile error {code} but the program compiled")
        } else {
            format!("expected compile error {code}, got {}", errors.join(", "))
        }));
    }

    let had_compile_error = reporter.emit_run(result.diagnostics(), &text, &label);
    if had_compile_error {
        return Ok(TestOutcome::Fail("compile error".into()));
    }

    let Some(hir) = result.get::<HirArtifact>() else {
        return Ok(TestOutcome::Fail("HIR lowering produced no output".into()));
    };

    // Default op budget per test file when no `timeout` annotation is given.
    let budget = annotations
        .timeout
        .unwrap_or(leek_backend_native::DEFAULT_OP_BUDGET);
    // Execute via the native JIT (the interpreter backend was removed), at the
    // input's settled version and strict mode. A runtime error surfaces as
    // `Err(NativeError::Runtime(..))`.
    let mut opts = leek_backend_native::NativeOptions::jit_for_input(result.input(), budget);
    if let Some(depth) = project
        .manifest
        .backend
        .native
        .as_ref()
        .and_then(|s| s.max_call_depth)
    {
        opts.max_call_depth = depth;
    }
    let run = match leek_backend_native::compile(hir.0.as_ref(), &opts) {
        Ok(NativeArtifact::Value(v)) => Ok(v.to_string()),
        Ok(_) => return Ok(TestOutcome::Fail("the JIT produced no result value".into())),
        Err(e) => Err(e),
    };
    Ok(judge(&annotations, run))
}

/// Compare a finished run (`Ok` = the displayed result value) with what the
/// test expects. Compile-error expectations are settled before running.
fn judge(annotations: &Annotations, run: Result<String, NativeError>) -> TestOutcome {
    let fail = |reason: String| TestOutcome::Fail(reason);
    match (&annotations.expect, run) {
        (Expectation::Pass, Ok(output)) => match &annotations.output {
            Some(want) if *want != output => {
                fail(format!("expected output {want:?}, got {output:?}"))
            }
            _ => TestOutcome::Pass,
        },
        (Expectation::Pass, Err(e)) => fail(format!("runtime: {e}")),
        (Expectation::CompileError(code), _) => fail(format!(
            "expected compile error {code} but the program compiled"
        )),
        (Expectation::RuntimeError(want), Err(NativeError::Runtime(got))) if got == *want => {
            TestOutcome::Pass
        }
        (Expectation::RuntimeError(want), Err(e)) => {
            fail(format!("expected runtime error {want}, got {e}"))
        }
        (Expectation::RuntimeError(want), Ok(_)) => fail(format!(
            "expected runtime error {want} but the program ran clean"
        )),
        (Expectation::Fail, Ok(_)) => fail("expected failure but program ran clean".into()),
        (Expectation::Fail, Err(NativeError::Runtime(code))) => {
            if code != BUDGET_EXHAUSTED || annotations.timeout.is_some() {
                TestOutcome::Pass
            } else {
                fail(format!(
                    "expected failure, but the run only exhausted the default op budget \
                     ({code}); set `timeout` or use `expect-runtime-error: {code}`"
                ))
            }
        }
        (Expectation::Fail, Err(e)) => fail(format!("expected a runtime error, got {e}")),
    }
}

/// How a test is expected to end.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expectation {
    /// Compiles and runs without error (the default).
    Pass,
    /// Some runtime error; op-budget exhaustion only with an explicit timeout.
    Fail,
    /// Compilation reports this diagnostic code (canonical id) as an error.
    CompileError(String),
    /// The run fails with this runtime error code.
    RuntimeError(String),
}

#[derive(Debug)]
struct Annotations {
    expect: Expectation,
    /// `expect-output`: the displayed result value of a passing run.
    output: Option<String>,
    timeout: Option<u64>,
    /// Unknown, malformed, conflicting or misplaced directives.
    problems: Vec<String>,
}

fn parse_annotations(text: &str) -> Annotations {
    let mut out = Annotations {
        expect: Expectation::Pass,
        output: None,
        timeout: None,
        problems: Vec::new(),
    };
    let mut expect_set = false;
    let mut in_header = true;
    for (line_no, raw_line) in (1_usize..).zip(text.lines()) {
        let trimmed = raw_line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let Some(comment) = trimmed.strip_prefix("//") else {
            in_header = false;
            continue;
        };
        let Some(directive) = comment.trim().strip_prefix("miku-test:") else {
            continue;
        };
        let directive = directive.trim();
        if !in_header {
            out.problems.push(format!(
                "line {line_no}: `{directive}` must be in the leading comment block"
            ));
            continue;
        }

        let (name, arg) = split_directive(directive);
        let expect = match name {
            "expect-pass" if arg.is_empty() => Expectation::Pass,
            "expect-fail" if arg.is_empty() => Expectation::Fail,
            "expect-compile-error" => match Code::resolve(arg) {
                Some(code) => Expectation::CompileError(code.id().to_string()),
                None if arg.is_empty() => {
                    out.problems
                        .push(format!("line {line_no}: `{name}` needs a diagnostic code"));
                    continue;
                }
                None => {
                    out.problems
                        .push(format!("line {line_no}: unknown diagnostic code `{arg}`"));
                    continue;
                }
            },
            "expect-runtime-error" => {
                if arg.is_empty()
                    || !arg
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                {
                    out.problems.push(format!(
                        "line {line_no}: `{name}` needs an error code like STACKOVERFLOW, got `{arg}`"
                    ));
                    continue;
                }
                Expectation::RuntimeError(arg.to_string())
            }
            "expect-output" => {
                if arg.is_empty() {
                    out.problems
                        .push(format!("line {line_no}: `{name}` needs the expected text"));
                } else if out.output.replace(arg.to_string()).is_some() {
                    out.problems
                        .push(format!("line {line_no}: `{name}` given more than once"));
                }
                continue;
            }
            "timeout" => {
                match arg.parse::<u64>() {
                    Ok(n) if out.timeout.is_none() => out.timeout = Some(n),
                    Ok(_) => out
                        .problems
                        .push(format!("line {line_no}: `timeout` given more than once")),
                    Err(_) => out.problems.push(format!(
                        "line {line_no}: `timeout` needs an integer op budget, got `{arg}`"
                    )),
                }
                continue;
            }
            _ => {
                out.problems
                    .push(format!("line {line_no}: unknown directive `{directive}`"));
                continue;
            }
        };
        if expect_set && out.expect != expect {
            out.problems.push(format!(
                "line {line_no}: `{directive}` conflicts with an earlier expectation"
            ));
        }
        out.expect = expect;
        expect_set = true;
    }
    if out.output.is_some() && out.expect != Expectation::Pass {
        out.problems
            .push("`expect-output` only applies to a test that runs clean".into());
    }
    out
}

/// Split `name[: arg]` / `name arg` into the directive name and its
/// argument (trimmed; empty when absent).
fn split_directive(directive: &str) -> (&str, &str) {
    let end = directive
        .find(|c: char| c == ':' || c.is_whitespace())
        .unwrap_or(directive.len());
    let (name, rest) = directive.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':').unwrap_or(rest);
    (name, rest.trim())
}

fn display_relative(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root)
        .map_or_else(|_| p.to_path_buf(), std::path::Path::to_path_buf)
}

/// Decide where the JUnit XML goes (manifest `[test].junit_xml` if
/// set, else stdout) and write it.
fn write_junit_output(project: &Project, records: &[TestRecord]) -> Result<()> {
    let xml = render_junit(&project.manifest.project.name, records);
    match project.manifest.test.junit_xml.as_ref() {
        Some(rel) => {
            let path = if rel.is_absolute() {
                rel.clone()
            } else {
                project.root.join(rel)
            };
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&path, &xml).with_context(|| format!("writing {}", path.display()))?;
        }
        None => {
            print!("{xml}");
        }
    }
    Ok(())
}

/// Render a single `<testsuite>` block. We don't currently group by
/// directory or annotation — each `.leek` file is one testcase. The
/// `package` attribute is the project name.
fn render_junit(project_name: &str, records: &[TestRecord]) -> String {
    let total = records.len();
    let failures = records.iter().filter(|r| r.failure.is_some()).count();
    let time: f64 = records.iter().map(|r| r.duration_s).sum();

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<testsuites>\n");
    let _ = writeln!(
        out,
        "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"0\" time=\"{:.4}\">",
        xml_escape(project_name),
        total,
        failures,
        time,
    );
    for r in records {
        let _ = write!(
            out,
            "    <testcase classname=\"{}\" name=\"{}\" time=\"{:.4}\"",
            xml_escape(project_name),
            xml_escape(&r.name),
            r.duration_s,
        );
        match &r.failure {
            None => out.push_str("/>\n"),
            Some(reason) => {
                out.push_str(">\n");
                let _ = writeln!(out, "      <failure message=\"{}\"/>", xml_escape(reason));
                out.push_str("    </testcase>\n");
            }
        }
    }
    out.push_str("  </testsuite>\n");
    out.push_str("</testsuites>\n");
    out
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(code: &str) -> Result<String, NativeError> {
        Err(NativeError::Runtime(code.into()))
    }

    fn is_fail(outcome: &TestOutcome) -> bool {
        matches!(outcome, TestOutcome::Fail(_))
    }

    #[test]
    fn typed_directives_parse() {
        let a = parse_annotations(
            "// miku-test: expect-runtime-error: STACKOVERFLOW\n// miku-test: timeout 500\nreturn 1;\n",
        );
        assert!(a.problems.is_empty(), "{:?}", a.problems);
        assert_eq!(a.expect, Expectation::RuntimeError("STACKOVERFLOW".into()));
        assert_eq!(a.timeout, Some(500));

        let a = parse_annotations("// miku-test: expect-compile-error: L0013\n");
        assert!(a.problems.is_empty(), "{:?}", a.problems);
        assert_eq!(a.expect, Expectation::CompileError("L0013".into()));

        let a = parse_annotations("// @version:4\n// miku-test: expect-output: [1, 2]\n");
        assert!(a.problems.is_empty(), "{:?}", a.problems);
        assert_eq!(a.expect, Expectation::Pass);
        assert_eq!(a.output.as_deref(), Some("[1, 2]"));
    }

    #[test]
    fn bad_directives_are_problems_not_ignored() {
        for src in [
            "// miku-test: timeout lots\n",
            "// miku-test: expect-sucess\n",
            "// miku-test: expect-compile-error\n",
            "// miku-test: expect-compile-error: NOT_A_CODE\n",
            "// miku-test: expect-runtime-error:\n",
            "// miku-test: expect-fail\n// miku-test: expect-pass\n",
            "// miku-test: expect-fail\n// miku-test: expect-output: 1\n",
            "return 1;\n// miku-test: expect-fail\n",
        ] {
            assert!(
                !parse_annotations(src).problems.is_empty(),
                "expected a problem for {src:?}"
            );
        }
    }

    #[test]
    fn expect_fail_rejects_default_budget_exhaustion() {
        let loose = parse_annotations("// miku-test: expect-fail\n");
        assert!(is_fail(&judge(&loose, runtime(BUDGET_EXHAUSTED))));
        assert_eq!(judge(&loose, runtime("STACKOVERFLOW")), TestOutcome::Pass);
        assert!(is_fail(&judge(
            &loose,
            Err(NativeError::Compile("boom".into()))
        )));
        assert!(is_fail(&judge(&loose, Ok("null".into()))));

        let timed = parse_annotations("// miku-test: expect-fail\n// miku-test: timeout 10\n");
        assert_eq!(judge(&timed, runtime(BUDGET_EXHAUSTED)), TestOutcome::Pass);
    }

    #[test]
    fn expect_runtime_error_matches_the_code() {
        let a = parse_annotations("// miku-test: expect-runtime-error: STACKOVERFLOW\n");
        assert_eq!(judge(&a, runtime("STACKOVERFLOW")), TestOutcome::Pass);
        assert!(is_fail(&judge(&a, runtime(BUDGET_EXHAUSTED))));
        assert!(is_fail(&judge(&a, Ok("1".into()))));
    }

    #[test]
    fn expect_output_compares_the_result() {
        let a = parse_annotations("// miku-test: expect-output: 3\n");
        assert_eq!(judge(&a, Ok("3".into())), TestOutcome::Pass);
        assert!(is_fail(&judge(&a, Ok("4".into()))));
        assert!(is_fail(&judge(&a, runtime("STACKOVERFLOW"))));
    }
}
