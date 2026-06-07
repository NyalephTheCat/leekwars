//! `miku test` — run every `.leek` file under `tests/` through the
//! interpreter.
//!
//! Recognized annotations in leading comments:
//! ```text
//! // miku-test: expect-pass         (default if absent)
//! // miku-test: expect-fail
//! // miku-test: timeout <ops>       (op budget — integer)
//! ```
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

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result};
use leek_hir::pipeline::HirArtifact;
use leek_span::SourceId;

use leek_diagnostics::Reporter;
use leek_pipeline::Input;
use leek_project::Project;

use crate::cli::{ColorWhen, MessageFormat, Test};
use crate::util::reporter_from_cli;

/// Default op budget per test file when no `timeout` annotation is given.
const DEFAULT_OP_BUDGET: u64 = 5_000_000;

pub fn run(
    args: Test,
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
    let mut next_source: u32 = 1;
    for path in &tests {
        let source = SourceId::new(next_source).unwrap();
        next_source += 1;
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
    let version_byte = input.version_byte;
    let annotations = parse_annotations(&text);

    let pipeline =
        leek_recipes::pipeline(leek_recipes::Target::Linted, &leek_recipes::driver_params())
            .expect("recipe");
    let result = pipeline.run(input);

    let had_compile_error =
        reporter.emit_run(result.diagnostics(), &text, &path.display().to_string());
    if had_compile_error {
        return Ok(TestOutcome::Fail("compile error".into()));
    }

    let Some(hir) = result.get::<HirArtifact>() else {
        return Ok(TestOutcome::Fail("HIR lowering produced no output".into()));
    };

    let budget = annotations.timeout.unwrap_or(DEFAULT_OP_BUDGET);
    let r = leek_backend_interp::run_with_limit_version(hir.0.as_ref(), budget, version_byte);

    match (&r.error, annotations.expect_fail) {
        (None, false) => Ok(TestOutcome::Pass),
        (None, true) => Ok(TestOutcome::Fail(
            "expected failure but program ran clean".into(),
        )),
        (Some(_), true) => Ok(TestOutcome::Pass),
        (Some(err), false) => Ok(TestOutcome::Fail(format!("runtime: {err}"))),
    }
}

struct Annotations {
    expect_fail: bool,
    timeout: Option<u64>,
}

fn parse_annotations(text: &str) -> Annotations {
    let mut out = Annotations {
        expect_fail: false,
        timeout: None,
    };
    for raw_line in text.lines() {
        let trimmed = raw_line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let body = if let Some(rest) = trimmed.strip_prefix("//") {
            rest.trim()
        } else {
            break;
        };
        let Some(directive) = body.strip_prefix("miku-test:") else {
            continue;
        };
        let directive = directive.trim();
        if directive == "expect-pass" {
            out.expect_fail = false;
        } else if directive == "expect-fail" {
            out.expect_fail = true;
        } else if let Some(arg) = directive.strip_prefix("timeout")
            && let Ok(n) = arg.trim().parse::<u64>() {
                out.timeout = Some(n);
            }
    }
    out
}

fn display_relative(root: &Path, p: &Path) -> PathBuf {
    p.strip_prefix(root).map_or_else(|_| p.to_path_buf(), std::path::Path::to_path_buf)
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
    out.push_str(&format!(
        "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"0\" time=\"{:.4}\">\n",
        xml_escape(project_name),
        total,
        failures,
        time,
    ));
    for r in records {
        out.push_str(&format!(
            "    <testcase classname=\"{}\" name=\"{}\" time=\"{:.4}\"",
            xml_escape(project_name),
            xml_escape(&r.name),
            r.duration_s,
        ));
        match &r.failure {
            None => out.push_str("/>\n"),
            Some(reason) => {
                out.push_str(">\n");
                out.push_str(&format!(
                    "      <failure message=\"{}\"/>\n",
                    xml_escape(reason),
                ));
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
