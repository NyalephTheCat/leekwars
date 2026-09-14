//! TOML parsing for `Miku.toml`.
//!
//! Hand-rolled over `toml_edit`'s immutable document rather than
//! serde-derived so we can emit per-key warnings instead of hard-failing on
//! unknown fields — and so every error can point at the key or value that
//! caused it. `toml_edit::ImDocument` is the span-preserving half of the
//! `toml` crate the workspace already depends on: it keeps each `Key` and
//! `Item`'s byte range, which is what turns "unknown key `fight.worker_count`"
//! into a caret under `worker_count`.

use crate::error::{
    ManifestError, ManifestErrorKind, ManifestWarning, ManifestWarningKind, span_of,
};
use crate::format::FormatOptions;
use crate::types::{
    BackendSettings, BackendTable, FightTable, JavaMode, LintTable, Manifest, PathsTable,
    ProjectTable, TestTable,
};
use leek_span::Span;
use std::path::PathBuf;
use toml_edit::{Item, TableLike};

/// Top-level table names we recognize. Anything outside this set is
/// an error.
pub(crate) const KNOWN_TOP_LEVEL: &[&str] = &[
    "project",
    "paths",
    "backend",
    "lint",
    "format",
    "test",
    "fight",
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
    let doc = toml_edit::ImDocument::parse(s).map_err(|e| {
        ManifestError::at(
            span_of(e.span()),
            ManifestErrorKind::Toml {
                message: e.to_string(),
            },
        )
    })?;
    let root: &dyn TableLike = doc.as_table();

    let mut warnings = Vec::new();

    for (key, _) in root.iter() {
        if !KNOWN_TOP_LEVEL.contains(&key) {
            return Err(ManifestError::at(
                key_span(root, key),
                ManifestErrorKind::UnknownTopLevelKey {
                    key: key.to_string(),
                },
            ));
        }
    }
    for deferred in DEFERRED_TOP_LEVEL {
        if root.contains_key(deferred) {
            warnings.push(ManifestWarning::at(
                key_span(root, deferred),
                ManifestWarningKind::DeferredTable { name: deferred },
            ));
        }
    }

    let project_item = root.get("project").ok_or_else(|| {
        ManifestError::detached(ManifestErrorKind::MissingTable { name: "project" })
    })?;
    let project_tbl = table_val(project_item, "project")?;
    let project = parse_project(project_tbl, &mut warnings)?;

    let paths = match root.get("paths") {
        None => PathsTable::default(),
        Some(v) => parse_paths(table_val(v, "paths")?, &mut warnings)?,
    };

    let backend = match root.get("backend") {
        None => BackendTable::default(),
        Some(v) => parse_backend(table_val(v, "backend")?, &mut warnings)?,
    };

    let lint = match root.get("lint") {
        None => LintTable::default(),
        Some(v) => parse_lint(table_val(v, "lint")?, &mut warnings)?,
    };

    let format = match root.get("format") {
        None => FormatOptions::default(),
        Some(v) => FormatOptions::from_toml_table(table_val(v, "format")?)?,
    };

    let test = match root.get("test") {
        None => TestTable::default(),
        Some(v) => parse_test(table_val(v, "test")?, &mut warnings)?,
    };

    let fight = match root.get("fight") {
        None => FightTable::default(),
        Some(v) => parse_fight(table_val(v, "fight")?, &mut warnings)?,
    };

    Ok((
        Manifest {
            project,
            paths,
            backend,
            lint,
            format,
            test,
            fight,
        },
        warnings,
    ))
}

fn parse_project(
    tbl: &dyn TableLike,
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
            return Err(ManifestError::at(
                item_span(v),
                ManifestErrorKind::BadValue {
                    key: "project.language".to_string(),
                    expected: "1..=4".to_string(),
                    got: Some(n.to_string()),
                },
            ));
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
    tbl: &dyn TableLike,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<PathsTable, ManifestError> {
    const KNOWN: &[&str] = &["src", "tests", "benches", "build"];
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
    if let Some(v) = tbl.get("build") {
        out.build = build_dir_val(&string_val(v, "paths.build")?, item_span(v))?;
    }
    Ok(out)
}

/// `paths.build` names the directory `miku clean` deletes wholesale, so
/// it must stay a relative path *inside* the project: no absolute paths,
/// no `..`, no bare `.`.
fn build_dir_val(raw: &str, span: Option<Span>) -> Result<PathBuf, ManifestError> {
    let path = PathBuf::from(raw);
    let mut components = path.components();
    let ok = components.next().is_some_and(|c| is_plain(&c)) && components.all(|c| is_plain(&c));
    if !ok {
        return Err(ManifestError::at(
            span,
            ManifestErrorKind::BadValue {
                key: "paths.build".to_string(),
                expected: "a relative path inside the project".to_string(),
                got: Some(format!("`{raw}`")),
            },
        ));
    }
    Ok(path)
}

fn is_plain(component: &std::path::Component<'_>) -> bool {
    matches!(component, std::path::Component::Normal(_))
}

fn parse_backend(
    tbl: &dyn TableLike,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<BackendTable, ManifestError> {
    const KNOWN: &[&str] = &["java", "jar", "native", "wasm", "leekscript"];
    warn_unknown(tbl, "backend", KNOWN, warnings);
    let mut out = BackendTable::default();
    for (kind, val) in tbl.iter() {
        if !KNOWN.contains(&kind) {
            continue;
        }
        let sub = table_val(val, &format!("backend.{kind}"))?;
        let settings = parse_backend_settings(sub, kind, warnings)?;
        match kind {
            "java" => out.java = Some(settings),
            "jar" => out.jar = Some(settings),
            "native" => out.native = Some(settings),
            "wasm" => out.wasm = Some(settings),
            "leekscript" => out.leekscript = Some(settings),
            _ => unreachable!(),
        }
    }
    Ok(out)
}

fn parse_backend_settings(
    tbl: &dyn TableLike,
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
        "max_call_depth",
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
                return Err(ManifestError::at(
                    item_span(v),
                    ManifestErrorKind::BadValue {
                        key: format!("{scope}.mode"),
                        expected: "\"exact\" or \"clean\"".to_string(),
                        got: Some(format!("{other:?}")),
                    },
                ));
            }
        });
    }
    if let Some(v) = tbl.get("java_version") {
        let n = int_val(v, &format!("{scope}.java_version"))?;
        out.java_version = Some(u32::try_from(n).map_err(|_| {
            bad_value(
                v,
                format!("{scope}.java_version"),
                "non-negative".to_string(),
            )
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
        out.opt_level = Some(
            u8::try_from(n)
                .map_err(|_| bad_value(v, format!("{scope}.opt_level"), "0..=255".to_string()))?,
        );
    }
    if let Some(v) = tbl.get("max_call_depth") {
        let n = int_val(v, &format!("{scope}.max_call_depth"))?;
        out.max_call_depth = Some(u32::try_from(n).map_err(|_| {
            bad_value(
                v,
                format!("{scope}.max_call_depth"),
                format!("0..={}", u32::MAX),
            )
        })?);
    }
    Ok(out)
}

fn parse_lint(
    tbl: &dyn TableLike,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<LintTable, ManifestError> {
    const KNOWN: &[&str] = &["deny", "warn", "allow", "pedantic", "nursery"];
    warn_unknown(tbl, "lint", KNOWN, warnings);
    let mut out = LintTable::default();
    if let Some(v) = tbl.get("deny") {
        out.deny = string_array(v, "lint.deny")?;
        out.deny_spans = string_array_spans(v);
    }
    if let Some(v) = tbl.get("warn") {
        out.warn = string_array(v, "lint.warn")?;
        out.warn_spans = string_array_spans(v);
    }
    if let Some(v) = tbl.get("allow") {
        out.allow = string_array(v, "lint.allow")?;
        out.allow_spans = string_array_spans(v);
    }
    if let Some(v) = tbl.get("pedantic") {
        out.pedantic = bool_val(v, "lint.pedantic")?;
    }
    if let Some(v) = tbl.get("nursery") {
        out.nursery = bool_val(v, "lint.nursery")?;
    }
    Ok(out)
}

fn parse_test(
    tbl: &dyn TableLike,
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

fn parse_fight(
    tbl: &dyn TableLike,
    warnings: &mut Vec<ManifestWarning>,
) -> Result<FightTable, ManifestError> {
    const KNOWN: &[&str] = &["default_scenario", "scenarios_dir", "reports_dir", "jobs"];
    warn_unknown(tbl, "fight", KNOWN, warnings);
    let mut out = FightTable::default();
    if let Some(v) = tbl.get("default_scenario") {
        out.default_scenario = Some(PathBuf::from(string_val(v, "fight.default_scenario")?));
    }
    if let Some(v) = tbl.get("scenarios_dir") {
        out.scenarios_dir = Some(PathBuf::from(string_val(v, "fight.scenarios_dir")?));
    }
    if let Some(v) = tbl.get("reports_dir") {
        out.reports_dir = PathBuf::from(string_val(v, "fight.reports_dir")?);
    }
    if let Some(v) = tbl.get("jobs") {
        let n = int_val(v, "fight.jobs")?;
        if n < 1 {
            return Err(ManifestError::at(
                item_span(v),
                ManifestErrorKind::BadValue {
                    key: "fight.jobs".to_string(),
                    expected: ">= 1".to_string(),
                    got: Some(n.to_string()),
                },
            ));
        }
        out.jobs =
            Some(u32::try_from(n).map_err(|_| {
                bad_value(v, "fight.jobs".to_string(), format!("1..={}", u32::MAX))
            })?);
    }
    Ok(out)
}

// ---- helpers ----

/// The span of `key`'s *name* in `tbl` — what a "this key is wrong" error
/// underlines. `None` for a table the document synthesized (dotted keys).
fn key_span(tbl: &dyn TableLike, key: &str) -> Option<Span> {
    tbl.get_key_value(key).and_then(|(k, _)| span_of(k.span()))
}

/// The span of a *value* — what a "this value is wrong" error underlines.
fn item_span(item: &Item) -> Option<Span> {
    span_of(item.span())
}

/// A [`ManifestErrorKind::BadValue`] at `item`, for the range checks whose
/// message shows no value.
fn bad_value(item: &Item, key: String, expected: String) -> ManifestError {
    ManifestError::at(
        item_span(item),
        ManifestErrorKind::BadValue {
            key,
            expected,
            got: None,
        },
    )
}

fn warn_unknown(
    tbl: &dyn TableLike,
    scope: &str,
    known: &[&str],
    warnings: &mut Vec<ManifestWarning>,
) {
    for (key, _) in tbl.iter() {
        if !known.contains(&key) {
            warnings.push(ManifestWarning::at(
                key_span(tbl, key),
                ManifestWarningKind::UnknownField {
                    table: scope.to_string(),
                    key: key.to_string(),
                },
            ));
        }
    }
}

fn expect_string(tbl: &dyn TableLike, scope: &str, key: &str) -> Result<String, ManifestError> {
    let v = tbl.get(key).ok_or_else(|| {
        // The table is present but the key is not, so the table header is the
        // closest thing to point at.
        ManifestError::detached(ManifestErrorKind::MissingKey {
            key: format!("{scope}.{key}"),
        })
    })?;
    string_val(v, &format!("{scope}.{key}"))
}

/// A table or inline table. `as_table_like` covers both spellings —
/// `[backend.java]` and `backend = { java = { … } }` — the way
/// `toml::Value::as_table` used to.
pub(crate) fn table_val<'a>(item: &'a Item, key: &str) -> Result<&'a dyn TableLike, ManifestError> {
    item.as_table_like().ok_or_else(|| {
        ManifestError::at(
            item_span(item),
            ManifestErrorKind::NotATable {
                key: key.to_string(),
            },
        )
    })
}

pub(crate) fn string_val(item: &Item, key: &str) -> Result<String, ManifestError> {
    item.as_str()
        .map(std::string::ToString::to_string)
        .ok_or_else(|| wrong_type(item, key, "a string"))
}

pub(crate) fn int_val(item: &Item, key: &str) -> Result<i64, ManifestError> {
    item.as_integer()
        .ok_or_else(|| wrong_type(item, key, "an integer"))
}

pub(crate) fn bool_val(item: &Item, key: &str) -> Result<bool, ManifestError> {
    item.as_bool()
        .ok_or_else(|| wrong_type(item, key, "a boolean"))
}

pub(crate) fn wrong_type(item: &Item, key: &str, expected: &'static str) -> ManifestError {
    ManifestError::at(
        item_span(item),
        ManifestErrorKind::WrongType {
            key: key.to_string(),
            expected,
        },
    )
}

fn string_array(item: &Item, key: &str) -> Result<Vec<String>, ManifestError> {
    let arr = item
        .as_array()
        .ok_or_else(|| wrong_type(item, key, "an array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, value) in arr.iter().enumerate() {
        out.push(
            value
                .as_str()
                .map(std::string::ToString::to_string)
                .ok_or_else(|| {
                    ManifestError::at(
                        span_of(value.span()),
                        ManifestErrorKind::WrongType {
                            key: format!("{key}[{i}]"),
                            expected: "a string",
                        },
                    )
                })?,
        );
    }
    Ok(out)
}

/// Per-element spans for a string array, positionally aligned with
/// [`string_array`]'s output. `[lint]` keeps these so an unknown lint code can
/// be reported at the array element that named it rather than at the table.
fn string_array_spans(item: &Item) -> Vec<Option<Span>> {
    item.as_array()
        .map(|arr| arr.iter().map(|v| span_of(v.span())).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ManifestErrorKind;

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
    fn paths_build_defaults_to_build() {
        let (m, _) = parse_ok(
            r#"
            [project]
            name = "demo"
            version = "0.1.0"
            "#,
        );
        assert_eq!(m.paths.build, PathBuf::from("build"));
    }

    #[test]
    fn paths_build_is_configurable() {
        let (m, w) = parse_ok(
            r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [paths]
            build = "out/artifacts"
            "#,
        );
        assert_eq!(m.paths.build, PathBuf::from("out/artifacts"));
        assert!(w.is_empty(), "warnings: {w:?}");
    }

    #[test]
    fn paths_build_must_stay_inside_the_project() {
        for bad in ["..", "../elsewhere", "/tmp/elsewhere", ".", ""] {
            let src = format!(
                r#"
                [project]
                name = "demo"
                version = "0.1.0"
                [paths]
                build = "{bad}"
                "#
            );
            let err = parse(&src).unwrap_err();
            assert!(
                matches!(&err.kind, ManifestErrorKind::BadValue { key, .. } if key == "paths.build"),
                "{bad}: {err:?}"
            );
            assert!(err.to_string().contains("paths.build"), "{bad}: {err}");
        }
    }

    #[test]
    fn missing_project_table_errors() {
        let err = parse("").unwrap_err();
        assert!(matches!(
            err.kind,
            ManifestErrorKind::MissingTable { name: "project" }
        ));
        assert!(err.to_string().contains("project"));
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
        assert!(
            matches!(&err.kind, ManifestErrorKind::UnknownTopLevelKey { key } if key == "moonbeam"),
            "{err:?}"
        );
        assert!(err.to_string().contains("moonbeam"));
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
        assert!(matches!(
            &warnings[0].kind,
            ManifestWarningKind::UnknownField { table, key }
                if table == "project" && key == "future_knob"
        ));
        assert!(warnings[0].to_string().contains("project.future_knob"));
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
        assert!(warnings.iter().any(|w| matches!(
            w.kind,
            ManifestWarningKind::DeferredTable { name: "workspace" }
        )));
        assert!(
            warnings
                .iter()
                .any(|w| w.to_string().contains("[workspace]"))
        );
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
    fn inline_tables_parse_like_headers() {
        // `as_table_like` has to cover the inline spelling too — the old
        // `toml::Value::as_table` did, and a manifest is allowed to use it.
        let src = r#"
            project = { name = "demo", version = "0.1.0" }
            backend = { java = { enable = true, java_version = 21 } }
        "#;
        let (m, warnings) = parse_ok(src);
        assert!(warnings.is_empty(), "warnings: {warnings:?}");
        assert_eq!(m.project.name, "demo");
        let j = m.backend.java.as_ref().unwrap();
        assert!(j.enable);
        assert_eq!(j.java_version, Some(21));
    }

    #[test]
    fn dotted_keys_parse_like_headers() {
        // A dotted key synthesizes its parent table, which has no span. The
        // value still does, so the error is not span-less.
        let src = r#"
            project.name = "demo"
            project.version = "0.1.0"
            fight.jobs = 0
        "#;
        let err = parse(src).unwrap_err();
        assert!(
            matches!(&err.kind, ManifestErrorKind::BadValue { key, .. } if key == "fight.jobs"),
            "{err:?}"
        );
        let span = err.span.expect("the value itself has a span");
        assert_eq!(&src[span.start as usize..span.end as usize], "0");
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
    fn native_max_call_depth_parsed() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [backend.native]
            enable = true
            max_call_depth = 250
        "#;
        let (m, warnings) = parse_ok(src);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(m.backend.native.as_ref().unwrap().max_call_depth, Some(250));
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
    fn lint_entries_keep_their_spans() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [lint]
            deny = ["L0006", "L0007"]
        "#;
        let (m, _) = parse_ok(src);
        assert_eq!(m.lint.deny_spans.len(), 2);
        let span = m.lint.deny_spans[1].expect("array elements have spans");
        assert_eq!(&src[span.start as usize..span.end as usize], "\"L0007\"");
    }

    #[test]
    fn fight_table_defaults() {
        let (m, w) = parse_ok(
            r#"
            [project]
            name = "demo"
            version = "0.1.0"
            "#,
        );
        assert_eq!(m.fight.default_scenario, None);
        assert_eq!(m.fight.scenarios_dir, None);
        assert_eq!(m.fight.reports_dir, PathBuf::from("build/fight-reports"));
        assert_eq!(m.fight.jobs, None);
        assert!(w.is_empty());
    }

    #[test]
    fn fight_table_parsed() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [fight]
            default_scenario = "duel.toml"
            scenarios_dir = "scenarios"
            reports_dir = "out/fights"
            jobs = 8
        "#;
        let (m, warnings) = parse_ok(src);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(m.fight.default_scenario, Some(PathBuf::from("duel.toml")));
        assert_eq!(m.fight.scenarios_dir, Some(PathBuf::from("scenarios")));
        assert_eq!(m.fight.reports_dir, PathBuf::from("out/fights"));
        assert_eq!(m.fight.jobs, Some(8));
    }

    #[test]
    fn fight_unknown_key_warns() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [fight]
            reports_dir = "out/fights"
            worker_count = 4
        "#;
        let (m, warnings) = parse_ok(src);
        assert_eq!(m.fight.reports_dir, PathBuf::from("out/fights"));
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            &warnings[0].kind,
            ManifestWarningKind::UnknownField { table, key }
                if table == "fight" && key == "worker_count"
        ));
        assert!(warnings[0].to_string().contains("fight.worker_count"));
    }

    #[test]
    fn fight_jobs_must_be_positive() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [fight]
            jobs = 0
        "#;
        let err = parse(src).unwrap_err();
        assert!(
            matches!(&err.kind, ManifestErrorKind::BadValue { key, .. } if key == "fight.jobs"),
            "{err:?}"
        );
        assert!(err.to_string().contains("fight.jobs"), "{err}");
    }

    #[test]
    fn fight_default_scenario_must_be_a_string() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [fight]
            default_scenario = 3
        "#;
        let err = parse(src).unwrap_err();
        assert!(
            matches!(
                &err.kind,
                ManifestErrorKind::WrongType { key, expected: "a string" }
                    if key == "fight.default_scenario"
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("fight.default_scenario"), "{err}");
    }

    #[test]
    fn fight_reports_dir_must_be_a_string() {
        let src = r#"
            [project]
            name = "demo"
            version = "0.1.0"
            [fight]
            reports_dir = 3
        "#;
        let err = parse(src).unwrap_err();
        assert!(
            matches!(
                &err.kind,
                ManifestErrorKind::WrongType { key, .. } if key == "fight.reports_dir"
            ),
            "{err:?}"
        );
        assert!(err.to_string().contains("fight.reports_dir"), "{err}");
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
