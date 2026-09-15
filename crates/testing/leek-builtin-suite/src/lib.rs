//! Run focused builtin tests from `suite.toml`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use leek_diagnostics::Severity;
use leek_project::Input;
use leek_span::SourceId;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Suite {
    pub tests: Vec<Case>,
}

#[derive(Debug, Deserialize)]
pub struct Case {
    pub id: String,
    /// Builtin under test, for catalog-coverage cases. Optional — omit it
    /// for a free-form case (e.g. a parser/language test) that just runs
    /// `source` and checks the result.
    #[serde(default)]
    pub builtin: Option<String>,
    #[serde(default = "default_version")]
    pub version: u8,
    pub source: String,
    /// Operation budget for the run. Defaults to [`DEFAULT_OP_LIMIT`], except
    /// for `{ ops_at_most = N }`, which runs at `4 * N` so a row that blows
    /// its own expectation still finishes and reports the real count. A
    /// `{ runtime_error = "TOO_MUCH_OPERATIONS" }` row sets it explicitly.
    #[serde(default)]
    pub op_limit: Option<u64>,
    #[serde(deserialize_with = "deserialize_expect")]
    pub expect: Expectation,
}

/// Op budget a row runs under when it doesn't ask for one. Generous: a row
/// is meant to fail on its expectation, not on the budget.
pub const DEFAULT_OP_LIMIT: u64 = 5_000_000;

fn default_version() -> u8 {
    4
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expectation {
    Pass,
    /// The program is rejected: either the pipeline emits an error diagnostic,
    /// or it runs and traps. Use [`Expectation::RuntimeError`] when you know
    /// which fight error key you want.
    Error,
    /// The program compiles, runs, and traps with exactly this fight error key
    /// (`TOO_MUCH_OPERATIONS`, `ARRAY_OUT_OF_BOUND`, …).
    RuntimeError(String),
    OpsAtMost(u64),
    /// The program runs without error and its result displays as this string
    /// (version-aware, matching the upstream corpus's value comparison).
    Equals(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ExpectationWire {
    Name(String),
    Ops { ops_at_most: u64 },
    Equals { equals: String },
    RuntimeError { runtime_error: String },
}

fn deserialize_expect<'de, D>(deserializer: D) -> Result<Expectation, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match ExpectationWire::deserialize(deserializer)? {
        ExpectationWire::Name(s) => match s.as_str() {
            "pass" => Ok(Expectation::Pass),
            "error" => Ok(Expectation::Error),
            other => Err(serde::de::Error::custom(format!(
                "unknown expectation {other:?}; use pass, error, {{ ops_at_most: N }}, \
                 {{ equals: \"…\" }} or {{ runtime_error: \"CODE\" }}"
            ))),
        },
        ExpectationWire::Ops { ops_at_most } => Ok(Expectation::OpsAtMost(ops_at_most)),
        ExpectationWire::Equals { equals } => Ok(Expectation::Equals(equals)),
        ExpectationWire::RuntimeError { runtime_error } => {
            Ok(Expectation::RuntimeError(runtime_error))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    /// Failed, and why — a red row has to say what it saw, or the only way to
    /// read a CI failure is to rerun the suite locally.
    Fail(String),
}

pub struct Report {
    pub passed: usize,
    pub failed: usize,
    pub failures: Vec<String>,
}

pub fn load_suite(path: &Path) -> Result<Suite> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).context("parse suite.toml")
}

pub fn run_suite(suite: &Suite) -> Report {
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut failures = Vec::new();
    for (i, case) in suite.tests.iter().enumerate() {
        let file_id = i + 1;
        match run_case(case, file_id) {
            Ok(Outcome::Pass) => passed += 1,
            Ok(Outcome::Fail(why)) => {
                failed += 1;
                failures.push(format!("{}: {why}", case.id));
            }
            Err(e) => {
                failed += 1;
                failures.push(format!("{}: {e:#}", case.id));
            }
        }
    }
    Report {
        passed,
        failed,
        failures,
    }
}

fn run_case(case: &Case, file_id: usize) -> Result<Outcome> {
    if let Some(builtin) = &case.builtin
        && !leek_builtins::is_catalogued(builtin.as_str())
    {
        bail!(
            "builtin `{builtin}` is not in catalog.yaml — add metadata before writing a suite test"
        );
    }

    let source = SourceId::new(file_id.try_into().unwrap()).unwrap();
    let input = Input {
        source,
        text: case.source.clone().into(),
        version_byte: case.version,
        strict: false,
        flags: leek_span::FeatureFlags::from_env(),
    };

    let db = leek_db::LeekDb::default();
    let file = leek_db::input_file(&db, String::new(), &input);
    let diagnostics =
        leek_db::queries::file_diagnostics_upto(&db, file, leek_db::queries::Stage::Hir);
    if let Some(d) = diagnostics.iter().find(|d| d.severity == Severity::Error) {
        // A rejected program never reaches the backend, so `error` is
        // satisfied here; every other expectation wanted it to compile.
        return Ok(match case.expect {
            Expectation::Error => Outcome::Pass,
            _ => Outcome::Fail(format!("the program failed to compile: {}", d.message)),
        });
    }

    let lowered = leek_db::queries::lower_hir_query(&db, file).hir;
    let hir = lowered.as_ref();

    // `ops_at_most` runs at 4x its own bound so an over-budget row reports the
    // count it actually reached rather than tripping the budget first.
    let op_limit = case.op_limit.unwrap_or(match case.expect {
        Expectation::OpsAtMost(limit) => limit.saturating_mul(4),
        _ => DEFAULT_OP_LIMIT,
    });
    let ran = native_run(hir, case.version, op_limit);
    // "Outside the native subset" is a gap in the executor, not a verdict on
    // the row: report it as a harness error instead of a silent red.
    if let Err(e) = &ran
        && e.is_unsupported()
    {
        bail!("row is outside the native subset: {}", e.reason());
    }

    Ok(match (&case.expect, ran) {
        (Expectation::OpsAtMost(limit), Ok(_)) => {
            let used = leek_backend_native::ops_used();
            if used <= *limit {
                Outcome::Pass
            } else {
                Outcome::Fail(format!("used {used} operations, budget is {limit}"))
            }
        }
        (Expectation::OpsAtMost(_), Err(e)) => {
            Outcome::Fail(format!("the program errored instead of running: {e}"))
        }

        (Expectation::Pass, Ok(_)) => Outcome::Pass,
        (Expectation::Pass, Err(e)) => Outcome::Fail(format!("the program errored: {e}")),

        (Expectation::Equals(want), Ok(got)) => {
            if &got == want {
                Outcome::Pass
            } else {
                Outcome::Fail(format!("got {got:?}, want {want:?}"))
            }
        }
        (Expectation::Equals(_), Err(e)) => Outcome::Fail(format!("the program errored: {e}")),

        // The compile-diagnostic half of `error` was handled above, so what
        // is left is a program that had to trap at runtime.
        (Expectation::Error, Err(e)) if e.runtime_code().is_some() => Outcome::Pass,
        (Expectation::Error, Err(e)) => {
            Outcome::Fail(format!("expected a runtime error, the backend said: {e}"))
        }
        (Expectation::Error, Ok(got)) => {
            Outcome::Fail(format!("the program ran clean and returned {got:?}"))
        }

        (Expectation::RuntimeError(want), Err(e)) if e.runtime_code() == Some(want.as_str()) => {
            Outcome::Pass
        }
        (Expectation::RuntimeError(want), Err(e)) => {
            Outcome::Fail(format!("expected runtime error {want:?}, got: {e}"))
        }
        (Expectation::RuntimeError(want), Ok(got)) => Outcome::Fail(format!(
            "expected runtime error {want:?}, the program ran clean and returned {got:?}"
        )),
    })
}

/// Execute `hir` on the native JIT, returning the displayed result string —
/// or the backend's error, *kept*: collapsing a compile failure, an
/// unsupported construct and a runtime trap into one `None` is what made a
/// `runtime_error` row impossible to express. Replaces the removed
/// interpreter as the in-process executor (the upstream `expected` values
/// remain the oracle).
fn native_run(
    hir: &leek_hir::HirFile,
    version: u8,
    op_limit: u64,
) -> Result<String, leek_backend_native::NativeError> {
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(version));
    let mut opts = leek_backend_native::NativeOptions::release();
    opts.version = version;
    opts.op_limit = op_limit;
    opts.emit = leek_backend_native::NativeEmit::Jit;
    match leek_backend_native::compile(hir, &opts)? {
        leek_backend_native::NativeArtifact::Value(v) => Ok(v.to_string()),
        other => Err(leek_backend_native::NativeError::unsupported(format!(
            "the JIT produced {other:?} instead of a value"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_passes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("suite.toml");
        let suite = load_suite(&path).expect("load suite");
        let report = run_suite(&suite);
        assert_eq!(report.failed, 0, "failures: {:?}", report.failures);
    }
}
