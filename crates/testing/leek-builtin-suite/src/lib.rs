//! Run focused builtin tests from `suite.toml`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use leek_diagnostics::Severity;
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target};
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
    #[serde(deserialize_with = "deserialize_expect")]
    pub expect: Expectation,
}

fn default_version() -> u8 {
    4
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expectation {
    Pass,
    Error,
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
                 or {{ equals: \"…\" }}"
            ))),
        },
        ExpectationWire::Ops { ops_at_most } => Ok(Expectation::OpsAtMost(ops_at_most)),
        ExpectationWire::Equals { equals } => Ok(Expectation::Equals(equals)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
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
            Ok(Outcome::Fail) => {
                failed += 1;
                failures.push(case.id.clone());
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
        flags: leek_pipeline::FeatureFlags::from_env(),
    };

    let pipeline =
        leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive()).expect("recipe");
    let run = pipeline.run(input);
    if run
        .diagnostics()
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return Ok(if case.expect == Expectation::Error {
            Outcome::Pass
        } else {
            Outcome::Fail
        });
    }

    let Some(hir_art) = run.get::<HirArtifact>() else {
        return Ok(Outcome::Fail);
    };
    let hir = hir_art.0.as_ref();

    match case.expect {
        Expectation::OpsAtMost(limit) => {
            let (_result, used) =
                leek_backend_interp::run_with_ops_used(hir, limit.saturating_mul(4), case.version);
            Ok(if used <= limit {
                Outcome::Pass
            } else {
                Outcome::Fail
            })
        }
        Expectation::Pass => {
            let r = leek_backend_interp::run_with_limit_version(hir, 5_000_000, case.version);
            Ok(if r.error.is_none() {
                Outcome::Pass
            } else {
                Outcome::Fail
            })
        }
        Expectation::Equals(ref want) => {
            let r = leek_backend_interp::run_with_limit_version(hir, 5_000_000, case.version);
            if r.error.is_some() {
                return Ok(Outcome::Fail);
            }
            // Format the result version-aware, matching the upstream corpus.
            leek_backend_interp::value::DISPLAY_VERSION.with(|c| c.set(case.version));
            let got = r.value.to_string();
            Ok(if &got == want {
                Outcome::Pass
            } else {
                Outcome::Fail
            })
        }
        Expectation::Error => Ok(Outcome::Fail),
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
