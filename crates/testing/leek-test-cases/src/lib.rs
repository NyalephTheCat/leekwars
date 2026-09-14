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

    /// Every variant, built once so the tables below cannot quietly stop
    /// covering one. The exhaustive `match` is the guard: a new
    /// [`Expectation`] variant fails to compile here until it is added,
    /// which is what keeps the two predicates and the TOML round-trip from
    /// drifting apart from the enum.
    fn all_expectations() -> Vec<Expectation> {
        let all = vec![
            Expectation::Equals { value: "1".into() },
            Expectation::Error {
                code: "UNKNOWN_VARIABLE".into(),
            },
            Expectation::Error {
                code: "NONE".into(),
            },
            Expectation::Warning {
                code: "UNUSED_VARIABLE".into(),
            },
            Expectation::NoWarning,
            Expectation::AnyError,
            Expectation::Almost {
                value: "0.5".into(),
            },
            Expectation::Ops { count: 12 },
            Expectation::EqualsOps {
                value: "1".into(),
                count: 12,
            },
            Expectation::Unknown {
                detail: "code_v4_(x).thing()".into(),
            },
        ];
        for e in &all {
            match e {
                Expectation::Equals { .. }
                | Expectation::Error { .. }
                | Expectation::Warning { .. }
                | Expectation::NoWarning
                | Expectation::AnyError
                | Expectation::Almost { .. }
                | Expectation::Ops { .. }
                | Expectation::EqualsOps { .. }
                | Expectation::Unknown { .. } => {}
            }
        }
        all
    }

    /// `implies_clean_parse` gates what `leekbench --corpus-expectation clean`
    /// selects and what `leek-test-driver` treats as "must compile". Getting
    /// it wrong doesn't fail a test — it silently changes which cases the
    /// corpus sweep runs.
    #[test]
    fn clean_parse_is_pinned_for_every_variant() {
        let expected: Vec<(Expectation, bool)> = vec![
            (Expectation::Equals { value: "1".into() }, true),
            (
                Expectation::Almost {
                    value: "0.5".into(),
                },
                true,
            ),
            (Expectation::Ops { count: 1 }, true),
            (
                Expectation::EqualsOps {
                    value: "1".into(),
                    count: 1,
                },
                true,
            ),
            (Expectation::NoWarning, true),
            // A warning is still a successful compile.
            (
                Expectation::Warning {
                    code: "UNUSED_VARIABLE".into(),
                },
                true,
            ),
            // The inversion: upstream spells "this must NOT error" as an
            // `Error` expectation whose code is the literal `NONE`.
            (
                Expectation::Error {
                    code: "NONE".into(),
                },
                true,
            ),
            (
                Expectation::Error {
                    code: "UNKNOWN_VARIABLE".into(),
                },
                false,
            ),
            (Expectation::AnyError, false),
            (Expectation::Unknown { detail: "?".into() }, false),
        ];
        for (e, want) in &expected {
            assert_eq!(e.implies_clean_parse(), *want, "{e:?}");
        }
        assert_eq!(expected.len(), all_expectations().len());
    }

    /// `implies_error` decides whether the driver expects a diagnostic at
    /// all, so the `NONE` inversion has to go the *other* way here.
    #[test]
    fn implies_error_is_pinned_for_every_variant() {
        for e in all_expectations() {
            let want = match &e {
                Expectation::Error { code } => code != "NONE",
                Expectation::AnyError => true,
                _ => false,
            };
            assert_eq!(e.implies_error(), want, "{e:?}");
        }
        // Spelled out, because the two `Error` arms are the whole point.
        assert!(
            !Expectation::Error {
                code: "NONE".into()
            }
            .implies_error()
        );
        assert!(
            Expectation::Error {
                code: "NONE_OF_THE_ABOVE".into()
            }
            .implies_error(),
            "only the exact code NONE is the inversion"
        );
    }

    /// The two predicates are near-complements, but not exactly: `Unknown`
    /// is neither a clean parse nor an error, and nothing may quietly make
    /// them total.
    #[test]
    fn the_two_predicates_are_never_both_true() {
        for e in all_expectations() {
            assert!(
                !(e.implies_clean_parse() && e.implies_error()),
                "{e:?} claims to both parse cleanly and error"
            );
        }
        let unknown = Expectation::Unknown { detail: "?".into() };
        assert!(!unknown.implies_clean_parse() && !unknown.implies_error());
    }

    /// `#[serde(tag = "kind", rename_all = "snake_case")]` means a renamed or
    /// reordered variant changes the on-disk manifest the corpus build script
    /// writes and the runner reads. Round-trip every variant through TOML so a
    /// rename is caught here rather than as a corpus that mysteriously
    /// extracts zero cases.
    #[test]
    fn every_expectation_variant_round_trips_through_toml() {
        for e in all_expectations() {
            let mut m = Manifest::empty();
            m.cases.push(TestCase {
                id: "T.java::t::0@v4".into(),
                source_file: "T.java".into(),
                method_name: "t".into(),
                line: 1,
                call_index: 0,
                helper: "code_v4_".into(),
                java_line: String::new(),
                version: 4,
                strict: false,
                enabled: true,
                code: "1".into(),
                expected: e.clone(),
                audit: None,
            });
            let text = toml::to_string_pretty(&m).expect("serialize");
            let back: Manifest = toml::from_str(&text).expect("deserialize");
            assert_eq!(back.cases[0].expected, e, "round-trip failed:\n{text}");
        }
    }

    /// The tag is what upstream extraction writes; pin the exact snake_case
    /// spellings so a variant rename is a visible, deliberate schema change.
    #[test]
    fn the_serialized_tag_names_are_stable() {
        let tag = |e: &Expectation| {
            let text = toml::to_string(e).expect("serialize");
            text.lines()
                .find_map(|l| l.strip_prefix("kind = "))
                .map(|v| v.trim().trim_matches('"').to_string())
                .expect("a kind tag")
        };
        assert_eq!(
            all_expectations().iter().map(tag).collect::<Vec<_>>(),
            [
                "equals",
                "error",
                "error",
                "warning",
                "no_warning",
                "any_error",
                "almost",
                "ops",
                "equals_ops",
                "unknown",
            ]
        );
    }

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
