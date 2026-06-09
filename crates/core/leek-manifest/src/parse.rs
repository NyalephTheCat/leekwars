//! TOML parsing for `Miku.toml`.
//!
//! Hand-rolled over `toml::Value` rather than serde-derived so we can
//! emit per-key warnings instead of hard-failing on unknown fields.

use crate::format::FormatOptions;
use crate::types::{
    BackendSettings, BackendTable, JavaMode, LintTable, Manifest, PathsTable, ProjectTable,
    TestTable,
};
use std::path::PathBuf;

/// Hard parse failure — invalid TOML, missing required field, unknown
/// top-level key, or a typed field with the wrong shape.
#[derive(Debug, Clone)]
pub struct ManifestError {
    pub message: String,
}

impl ManifestError {
    fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ManifestError {}

impl From<ManifestError> for leek_diagnostics::Diagnostic {
    fn from(err: ManifestError) -> Self {
        leek_diagnostics::convert::manifest_error(err.message)
    }
}

/// Soft warning — unknown key inside a known table, or a deferred
/// table that was used. Surfaced so editors can show squiggles but
/// non-fatal so older toolchains can still load newer manifests.
#[derive(Debug, Clone)]
pub struct ManifestWarning {
    pub message: String,
}

impl ManifestWarning {
    fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for ManifestWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Top-level table names we recognize. Anything outside this set is
/// an error.
const KNOWN_TOP_LEVEL: &[&str] = &[
    "project",
    "paths",
    "backend",
    "lint",
    "format",
    "test",
    "lsp",
    "bench",
    "experimental",
    "profiles",
    "profile",
    "workspace",
    "toolchain",
];

/// Tables that we accept syntactically but do not act on in v0.1.
const DEFERRED_TOP_LEVEL: &[&str] = &[
    "lsp",
    "bench",
    "experimental",
    "profiles",
    "profile",
    "workspace",
    "toolchain",
];

pub(crate) fn parse(s: &str) -> Result<(Manifest, Vec<ManifestWarning>), ManifestError> {
    let doc: toml::Value =
        toml::from_str(s).map_err(|e| ManifestError::new(format!("Miku.toml: {e}")))?;
    let root = doc
        .as_table()
        .ok_or_else(|| ManifestError::new("Miku.toml: top level must be a table"))?;

    let mut warnings = Vec::new();

    for (key, _) in root {
        if !KNOWN_TOP_LEVEL.contains(&key.as_str()) {
            return Err(ManifestError::new(format!(
                "Miku.toml: unknown top-level key `{key}` (expected one of: {})",
                KNOWN_TOP_LEVEL.join(", ")
            )));
        }
    }
    for deferred in DEFERRED_TOP_LEVEL {
        if root.contains_key(*deferred) {
            warnings.push(ManifestWarning::new(format!(
                "Miku.toml: `[{deferred}]` is parsed but not yet interpreted in this toolchain"
            )));
        }
    }

    let project_tbl = root
        .get("project")
        .ok_or_else(|| ManifestError::new("Miku.toml: missing required table `[project]`"))?
        .as_table()
        .ok_or_else(|| ManifestError::new("Miku.toml: `project` must be a table"))?;
    let project = parse_project(project_tbl, &mut warnings)?;

    let paths = match root.get("paths") {
        None => PathsTable::default(),
        Some(v) => {
            let tbl = v
                .as_table()
                .ok_or_else(|| ManifestError::new("Miku.toml: `paths` must be a table"))?;
            parse_paths(tbl, &mut warnings)?
        }
    };

    let backend = match root.get("backend") {
        None => BackendTable::default(),
        Some(v) => {
            let tbl = v
                .as_table()
                .ok_or_else(|| ManifestError::new("Miku.toml: `backend` must be a table"))?;
            parse_backend(tbl, &mut warnings)?
        }
    };

    let lint = match root.get("lint") {
        None => LintTable::default(),
        Some(v) => {
            let tbl = v
                .as_table()
                .ok_or_else(|| ManifestError::new("Miku.toml: `lint` must be a table"))?;
            parse_lint(tbl, &mut warnings)?
        }
    };

    let format = match root.get("format") {
        None => FormatOptions::default(),
        Some(v) => {
            let tbl = v
                .as_table()
                .ok_or_else(|| ManifestError::new("Miku.toml: `format` must be a table"))?;
            FormatOptions::from_toml_table(tbl).map_err(ManifestError::new)?
        }
    };

    let test = match root.get("test") {
        None => TestTable::default(),
        Some(v) => {
            let tbl = v
                .as_table()
                .ok_or_else(|| ManifestError::new("Miku.toml: `test` must be a table"))?;
            parse_test(tbl, &mut warnings)?
        }
    };

    Ok((
        Manifest {
            project,
            paths,
            backend,
            lint,
            format,
            test,
        },
        warnings,
    ))
}

fn parse_project(
    tbl: &toml::value::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<ProjectTable, ManifestError> {
    const KNOWN: &[&str] = &[
        "name",
        "version",
        "edition",
        "language",
        "strict",
        "entry",
        "authors",
        "description",
        "license",
        "repository",
    ];
    warn_unknown(tbl, "project", KNOWN, warnings);

    let name = expect_string(tbl, "project", "name")?;
    let version = expect_string(tbl, "project", "version")?;
    let mut out = ProjectTable::defaults_with(name, version);

    if let Some(v) = tbl.get("edition") {
        out.edition = Some(string_val(v, "project.edition")?);
    }
    if let Some(v) = tbl.get("language") {
        let n = int_val(v, "project.language")?;
        if !(1..=4).contains(&n) {
            return Err(ManifestError::new(format!(
                "Miku.toml: project.language must be 1..=4, got {n}"
            )));
        }
        out.language = u8::try_from(n).expect("validated to 1..=4 above");
    }
    if let Some(v) = tbl.get("strict") {
        out.strict = bool_val(v, "project.strict")?;
    }
    if let Some(v) = tbl.get("entry") {
        out.entry = PathBuf::from(string_val(v, "project.entry")?);
    }
    if let Some(v) = tbl.get("authors") {
        out.authors = string_array(v, "project.authors")?;
    }
    if let Some(v) = tbl.get("description") {
        out.description = Some(string_val(v, "project.description")?);
    }
    if let Some(v) = tbl.get("license") {
        out.license = Some(string_val(v, "project.license")?);
    }
    if let Some(v) = tbl.get("repository") {
        out.repository = Some(string_val(v, "project.repository")?);
    }
    Ok(out)
}

fn parse_paths(
    tbl: &toml::value::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<PathsTable, ManifestError> {
    const KNOWN: &[&str] = &["src", "tests", "benches"];
    warn_unknown(tbl, "paths", KNOWN, warnings);
    let mut out = PathsTable::default();
    if let Some(v) = tbl.get("src") {
        out.src = PathBuf::from(string_val(v, "paths.src")?);
    }
    if let Some(v) = tbl.get("tests") {
        out.tests = PathBuf::from(string_val(v, "paths.tests")?);
    }
    if let Some(v) = tbl.get("benches") {
        out.benches = PathBuf::from(string_val(v, "paths.benches")?);
    }
    Ok(out)
}

fn parse_backend(
    tbl: &toml::value::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<BackendTable, ManifestError> {
    const KNOWN: &[&str] = &["java", "jar", "native", "wasm"];
    warn_unknown(tbl, "backend", KNOWN, warnings);
    let mut out = BackendTable::default();
    for (key, val) in tbl {
        let kind = key.as_str();
        if !KNOWN.contains(&kind) {
            continue;
        }
        let sub = val.as_table().ok_or_else(|| {
            ManifestError::new(format!("Miku.toml: `backend.{kind}` must be a table"))
        })?;
        let settings = parse_backend_settings(sub, kind, warnings)?;
        match kind {
            "java" => out.java = Some(settings),
            "jar" => out.jar = Some(settings),
            "native" => out.native = Some(settings),
            "wasm" => out.wasm = Some(settings),
            _ => unreachable!(),
        }
    }
    Ok(out)
}

fn parse_backend_settings(
    tbl: &toml::value::Table,
    kind: &str,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<BackendSettings, ManifestError> {
    const KNOWN: &[&str] = &[
        "enable",
        "default",
        "mode",
        "java_version",
        "emit_lines",
        "out_dir",
        "out",
        "main_class",
        "target",
        "opt_level",
    ];
    let scope = format!("backend.{kind}");
    warn_unknown(tbl, &scope, KNOWN, warnings);
    let mut out = BackendSettings::default();
    if let Some(v) = tbl.get("enable") {
        out.enable = bool_val(v, &format!("{scope}.enable"))?;
    }
    if let Some(v) = tbl.get("default") {
        out.is_default = bool_val(v, &format!("{scope}.default"))?;
    }
    if let Some(v) = tbl.get("mode") {
        let s = string_val(v, &format!("{scope}.mode"))?;
        out.java_mode = Some(match s.as_str() {
            "exact" => JavaMode::Exact,
            "clean" => JavaMode::Clean,
            other => {
                return Err(ManifestError::new(format!(
                    "Miku.toml: {scope}.mode must be \"exact\" or \"clean\", got {other:?}"
                )));
            }
        });
    }
    if let Some(v) = tbl.get("java_version") {
        let n = int_val(v, &format!("{scope}.java_version"))?;
        out.java_version = Some(u32::try_from(n).map_err(|_| {
            ManifestError::new(format!("Miku.toml: {scope}.java_version must be non-negative"))
        })?);
    }
    if let Some(v) = tbl.get("emit_lines") {
        out.emit_lines = bool_val(v, &format!("{scope}.emit_lines"))?;
    }
    if let Some(v) = tbl.get("out_dir") {
        out.out_dir = Some(PathBuf::from(string_val(v, &format!("{scope}.out_dir"))?));
    }
    if let Some(v) = tbl.get("out") {
        out.out = Some(PathBuf::from(string_val(v, &format!("{scope}.out"))?));
    }
    if let Some(v) = tbl.get("main_class") {
        out.main_class = Some(string_val(v, &format!("{scope}.main_class"))?);
    }
    if let Some(v) = tbl.get("target") {
        out.target = Some(string_val(v, &format!("{scope}.target"))?);
    }
    if let Some(v) = tbl.get("opt_level") {
        let n = int_val(v, &format!("{scope}.opt_level"))?;
        out.opt_level = Some(u8::try_from(n).map_err(|_| {
            ManifestError::new(format!("Miku.toml: {scope}.opt_level must be 0..=255"))
        })?);
    }
    Ok(out)
}

fn parse_lint(
    tbl: &toml::value::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<LintTable, ManifestError> {
    const KNOWN: &[&str] = &["deny", "warn", "allow"];
    warn_unknown(tbl, "lint", KNOWN, warnings);
    let mut out = LintTable::default();
    if let Some(v) = tbl.get("deny") {
        out.deny = string_array(v, "lint.deny")?;
    }
    if let Some(v) = tbl.get("warn") {
        out.warn = string_array(v, "lint.warn")?;
    }
    if let Some(v) = tbl.get("allow") {
        out.allow = string_array(v, "lint.allow")?;
    }
    Ok(out)
}

fn parse_test(
    tbl: &toml::value::Table,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<TestTable, ManifestError> {
    const KNOWN: &[&str] = &["timeout", "parallel", "junit_xml"];
    warn_unknown(tbl, "test", KNOWN, warnings);
    let mut out = TestTable::default();
    if let Some(v) = tbl.get("timeout") {
        out.timeout = Some(string_val(v, "test.timeout")?);
    }
    if let Some(v) = tbl.get("parallel") {
        out.parallel = bool_val(v, "test.parallel")?;
    }
    if let Some(v) = tbl.get("junit_xml") {
        out.junit_xml = Some(PathBuf::from(string_val(v, "test.junit_xml")?));
    }
    Ok(out)
}

// ---- helpers ----

fn warn_unknown(
    tbl: &toml::value::Table,
    scope: &str,
    known: &[&str],
    warnings: &mut Vec<ManifestWarning>,
) {
    for (key, _) in tbl {
        if !known.contains(&key.as_str()) {
            warnings.push(ManifestWarning::new(format!(
                "Miku.toml: unknown key `{scope}.{key}` (ignored)"
            )));
        }
    }
}

fn expect_string(
    tbl: &toml::value::Table,
    scope: &str,
    key: &str,
) -> Result<String, ManifestError> {
    let v = tbl.get(key).ok_or_else(|| {
        ManifestError::new(format!("Miku.toml: missing required key `{scope}.{key}`"))
    })?;
    string_val(v, &format!("{scope}.{key}"))
}

fn string_val(v: &toml::Value, scope: &str) -> Result<String, ManifestError> {
    v.as_str()
        .map(std::string::ToString::to_string)
        .ok_or_else(|| ManifestError::new(format!("Miku.toml: {scope} must be a string")))
}

fn int_val(v: &toml::Value, scope: &str) -> Result<i64, ManifestError> {
    v.as_integer()
        .ok_or_else(|| ManifestError::new(format!("Miku.toml: {scope} must be an integer")))
}

fn bool_val(v: &toml::Value, scope: &str) -> Result<bool, ManifestError> {
    v.as_bool()
        .ok_or_else(|| ManifestError::new(format!("Miku.toml: {scope} must be a boolean")))
}

fn string_array(v: &toml::Value, scope: &str) -> Result<Vec<String>, ManifestError> {
    let arr = v
        .as_array()
        .ok_or_else(|| ManifestError::new(format!("Miku.toml: {scope} must be an array")))?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        out.push(string_val(item, &format!("{scope}[{i}]"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(s: &str) -> (Manifest, Vec<ManifestWarning>) {
        parse(s).expect("manifest should parse")
    }

    #[test]
    fn minimal_manifest() {
        let (m, w) = parse_ok(
            r#"
            [project]
            name = "demo"
            version = "0.1.0"
            "#,
        );
        assert_eq!(m.project.name, "demo");
        assert_eq!(m.project.version, "0.1.0");
        assert_eq!(m.project.language, 4);
        assert_eq!(m.project.entry, PathBuf::from("src/main.leek"));
        assert!(w.is_empty());
    }

    #[test]
    fn missing_project_table_errors() {
        let err = parse("").unwrap_err();
        assert!(err.message.contains("project"));
    }

    #[test]
    fn unknown_top_level_errors() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [moonbeam]
            x = 1
        "#;
        let err = parse(src).unwrap_err();
        assert!(err.message.contains("moonbeam"));
    }

    #[test]
    fn unknown_nested_key_warns() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            future_knob = true
        "#;
        let (_, warnings) = parse_ok(src);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("project.future_knob"));
    }

    #[test]
    fn deferred_table_warns_but_parses() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [workspace]
            members = ["a"]
        "#;
        let (_, warnings) = parse_ok(src);
        assert!(warnings.iter().any(|w| w.message.contains("[workspace]")));
    }

    #[test]
    fn full_backend_table() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"

            [backend.java]
            enable = true
            default = true
            mode = "clean"
            java_version = 17
            emit_lines = true
            out_dir = "build/java"

            [backend.native]
            enable = true
        "#;
        let (m, _) = parse_ok(src);
        let j = m.backend.java.as_ref().unwrap();
        assert!(j.enable);
        assert!(j.is_default);
        assert_eq!(j.java_mode, Some(JavaMode::Clean));
        assert_eq!(j.java_version, Some(17));
        assert_eq!(j.out_dir, Some(PathBuf::from("build/java")));
        assert!(m.backend.native.as_ref().unwrap().enable);
        assert_eq!(
            m.backend.default_kind(),
            Some(crate::types::BackendKind::Java)
        );
    }

    #[test]
    fn default_backend_falls_back_to_first_enabled() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [backend.native]
            enable = true
        "#;
        let (m, _) = parse_ok(src);
        assert_eq!(
            m.backend.default_kind(),
            Some(crate::types::BackendKind::Native)
        );
    }

    #[test]
    fn lint_table_parsed() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [lint]
            deny = ["L0006"]
            warn = ["L0001"]
            allow = ["L0004"]
        "#;
        let (m, _) = parse_ok(src);
        assert_eq!(m.lint.deny, ["L0006"]);
        assert_eq!(m.lint.warn, ["L0001"]);
        assert_eq!(m.lint.allow, ["L0004"]);
    }

    #[test]
    fn format_table_round_trips() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [format]
            indent = 2
            max_line_length = 80
        "#;
        let (m, _) = parse_ok(src);
        assert_eq!(m.format.indent, 2);
        assert_eq!(m.format.max_line_length, 80);
    }
}
