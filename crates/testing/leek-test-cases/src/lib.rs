//! Test-case data model and TOML/JSON serialization.
//!
//! Split out of `leek-test-driver` (#150) so `leek-test-corpus`'s build
//! script can depend on the data model without the runner behind it.
//! `leek-test-driver` re-exports this crate as its `cases` module, so
//! `leek_test_driver::cases::TestCase` still resolves.
//!
//! Nothing here may grow a dependency on the compiler: serde, toml and
//! anyhow are the whole budget.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One extracted test case from an upstream JUnit file. A single
/// `code_v1_3("...")` call in Java explodes into multiple cases here
/// — one per language version in its range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Stable identifier: `<source_file>::<method>::<call_idx>@v<n>`.
    pub id: String,

    /// JUnit source file relative to the upstream tests dir
    /// (e.g. `"TestBoolean.java"`).
    pub source_file: String,

    /// Surrounding `@Test` method name.
    pub method_name: String,

    /// Line in `source_file` where the helper call begins.
    pub line: u32,

    /// 0-based index of the call within its method.
    pub call_index: u32,

    /// Upstream helper prefix (e.g. `code_v4_`, `code_strict_v2_`).
    #[serde(default)]
    pub helper: String,

    /// Full Java call chain from the helper through the expectation.
    #[serde(default)]
    pub java_line: String,

    /// Language version this case runs at (1..=4).
    pub version: u8,

    /// Whether strict mode is enabled.
    pub strict: bool,

    /// Whether the upstream marked the helper as `DISABLED_…`.
    pub enabled: bool,

    /// Raw Leekscript source from the helper's argument(s).
    pub code: String,

    /// What the chained assertion asks for.
    pub expected: Expectation,

    /// Optional pipeline snapshot (compile errors / hir built) attached
    /// by `leek_test_driver::audit::audit_case`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<CaseAudit>,
}

/// Pipeline stages observed when enriching a case (see `audit` module).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaseAudit {
    pub compile_errors: u32,
    pub compile_warnings: u32,
    pub hir_built: bool,
}

/// Expected outcome of running the case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expectation {
    Equals {
        value: String,
    },
    Error {
        code: String,
    },
    Warning {
        code: String,
    },
    NoWarning,
    AnyError,
    Almost {
        value: String,
    },
    Ops {
        count: u64,
    },
    /// `.equalsOps("value", N)` — result string and op count.
    EqualsOps {
        value: String,
        count: u64,
    },
    Unknown {
        detail: String,
    },
}

impl Expectation {
    pub fn implies_clean_parse(&self) -> bool {
        match self {
            Self::Equals { .. }
            | Self::Almost { .. }
            | Self::Ops { .. }
            | Self::EqualsOps { .. }
            | Self::NoWarning
            | Self::Warning { .. } => true,
            Self::Error { code } if code == "NONE" => true,
            _ => false,
        }
    }

    pub fn implies_error(&self) -> bool {
        match self {
            Self::Error { code } if code == "NONE" => false,
            Self::Error { .. } | Self::AnyError => true,
            _ => false,
        }
    }
}

/// Extracted upstream test manifest (`upstream_cases.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub cases: Vec<TestCase>,
    pub source_files: Vec<String>,
    pub skipped: Vec<SkippedCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedCall {
    pub source_file: String,
    pub line: u32,
    pub reason: String,
    pub snippet: String,
}

impl Manifest {
    pub const SCHEMA_VERSION: u32 = 2;

    pub fn empty() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            cases: Vec::new(),
            source_files: Vec::new(),
            skipped: Vec::new(),
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest is written by `leek-test-corpus`'s build script and
    /// read back by the runner, so this TOML round-trip is the whole
    /// contract of the crate the build script depends on.
    #[test]
    fn manifest_round_trips_through_toml() {
        let dir = std::env::temp_dir().join(format!("leek-test-cases-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join("upstream_cases.toml");

        let mut manifest = Manifest::empty();
        manifest.source_files.push("TestBoolean.java".to_string());
        manifest.cases.push(TestCase {
            id: "TestBoolean.java::t::0@v4".to_string(),
            source_file: "TestBoolean.java".to_string(),
            method_name: "t".to_string(),
            line: 12,
            call_index: 0,
            helper: "code_v4_".to_string(),
            java_line: "code_v4_(\"true\").equals(\"true\");".to_string(),
            version: 4,
            strict: false,
            enabled: true,
            code: "true".to_string(),
            expected: Expectation::Equals {
                value: "true".to_string(),
            },
            audit: None,
        });

        manifest.save(&path).expect("save manifest");
        let loaded = Manifest::load(&path).expect("load manifest");

        assert_eq!(loaded.schema_version, Manifest::SCHEMA_VERSION);
        assert_eq!(loaded.source_files, manifest.source_files);
        assert_eq!(loaded.cases.len(), 1);
        assert_eq!(loaded.cases[0].id, manifest.cases[0].id);
        assert_eq!(loaded.cases[0].expected, manifest.cases[0].expected);
        std::fs::remove_file(&path).expect("clean up");
    }
}
