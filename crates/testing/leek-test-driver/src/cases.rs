//! Test-case data model and TOML/JSON serialization.

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
    /// by [`crate::audit::audit_case`].
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
    Equals { value: String },
    Error { code: String },
    Warning { code: String },
    NoWarning,
    AnyError,
    Almost { value: String },
    Ops { count: u64 },
    /// `.equalsOps("value", N)` — result string and op count.
    EqualsOps { value: String, count: u64 },
    Unknown { detail: String },
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
