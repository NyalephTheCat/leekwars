//! `cargo xtask check-errors`: keep `anyhow` out of the library layers.
//!
//! The error convention (see "Error handling" in `docs/architecture.md`) says
//! a library crate returns a typed enum; `anyhow` belongs to the front-ends —
//! `bins/`, `xtask/` and `crates/testing/` — where an error's only remaining
//! job is to be printed. A library that returns `anyhow::Error` has thrown
//! away the distinction its caller needs.
//!
//! The check is on the dependency, not the signature: a crate that does not
//! depend on `anyhow` cannot leak it. That is coarser than reading every
//! `pub fn`, and deliberately so — it is exact, needs no parsing, and cannot
//! be argued with. Clippy's `disallowed_types` can't express this rule at all,
//! since `clippy.toml` is workspace-global and would ban `anyhow` in `bins/`
//! too.
//!
//! Existing violations are listed, with a reason, in
//! `xtask/error-allowlist.txt`. An entry that no longer matches a violation is
//! itself an error, so the list can only shrink.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Deserialize;

use crate::layers::Layer;

/// Allowlist location, relative to the workspace root.
const ALLOWLIST_PATH: &str = "xtask/error-allowlist.txt";

/// The crates a library may not reach for. One entry today; the list is here
/// so adding the next one is a one-line change rather than a rewrite.
const FRONT_END_ONLY: &[&str] = &["anyhow"];

/// Layers whose crates are front-ends, where [`FRONT_END_ONLY`] is fine.
fn is_front_end(layer: Layer) -> bool {
    matches!(layer, Layer::Bins | Layer::Xtask | Layer::Testing)
}

/// One crate depending on a crate its layer may not use.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    pub krate: String,
    pub layer: Layer,
    pub dependency: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}) depends on `{}`: library crates return a typed error \
             (see docs/architecture.md)",
            self.krate, self.layer, self.dependency
        )
    }
}

/// The subset of `cargo metadata --format-version 1 --no-deps` we read.
#[derive(Debug, Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    packages: Vec<Package>,
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<Dependency>,
}

#[derive(Debug, Deserialize)]
struct Dependency {
    name: String,
    kind: Option<String>,
}

/// Every violation in the workspace graph, sorted.
///
/// Dev dependencies are exempt: a crate's own tests are front-ends too, and
/// they cannot leak a type through the crate's public API.
pub fn violations_from_metadata_json(json: &str) -> Result<Vec<Violation>, Vec<String>> {
    let meta: Metadata = serde_json::from_str(json)
        .map_err(|e| vec![format!("cannot parse cargo metadata: {e}")])?;
    let mut errors = Vec::new();
    let mut out = Vec::new();

    for pkg in &meta.packages {
        let dir = pkg.manifest_path.parent().unwrap_or(&pkg.manifest_path);
        let rel = dir.strip_prefix(&meta.workspace_root).unwrap_or(dir);
        let Some(layer) = Layer::from_dir(rel) else {
            errors.push(format!(
                "{} ({}) is not in a known layer; `cargo xtask check-layers` \
                 has the details",
                pkg.name,
                rel.display()
            ));
            continue;
        };
        if is_front_end(layer) {
            continue;
        }
        for dep in &pkg.dependencies {
            if dep.kind.as_deref() == Some("dev") {
                continue;
            }
            if FRONT_END_ONLY.contains(&dep.name.as_str()) {
                out.push(Violation {
                    krate: pkg.name.clone(),
                    layer,
                    dependency: dep.name.clone(),
                });
            }
        }
    }

    if errors.is_empty() {
        out.sort();
        out.dedup();
        Ok(out)
    } else {
        Err(errors)
    }
}

/// One allowlisted crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowEntry {
    pub line: usize,
    pub reason: String,
}

/// Parsed `xtask/error-allowlist.txt`, keyed by `(crate, dependency)`.
#[derive(Debug, Default)]
pub struct Allowlist {
    pub entries: BTreeMap<(String, String), AllowEntry>,
}

impl Allowlist {
    /// Parse `<crate> -> <dependency>: <reason>` lines; `#` starts a comment.
    pub fn parse(text: &str) -> Result<Self, Vec<String>> {
        let mut list = Self::default();
        let mut errors = Vec::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = idx + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let parsed = trimmed.split_once(':').and_then(|(edge, reason)| {
                let (krate, dep) = edge.split_once("->")?;
                Some((krate.trim(), dep.trim(), reason.trim()))
            });
            let Some((krate, dep, reason)) = parsed else {
                errors.push(format!(
                    "{ALLOWLIST_PATH}:{line}: expected `<crate> -> <dependency>: <reason>`"
                ));
                continue;
            };
            if krate.is_empty() || dep.is_empty() {
                errors.push(format!("{ALLOWLIST_PATH}:{line}: missing crate name"));
            } else if reason.is_empty() {
                errors.push(format!(
                    "{ALLOWLIST_PATH}:{line}: `{krate} -> {dep}` needs a justification"
                ));
            } else if let Some(prev) = list.entries.insert(
                (krate.to_owned(), dep.to_owned()),
                AllowEntry {
                    line,
                    reason: reason.to_owned(),
                },
            ) {
                errors.push(format!(
                    "{ALLOWLIST_PATH}:{line}: `{krate} -> {dep}` already listed on line {}",
                    prev.line
                ));
            }
        }
        if errors.is_empty() {
            Ok(list)
        } else {
            Err(errors)
        }
    }
}

/// Outcome of checking the workspace against the rule and the allowlist.
#[derive(Debug, Default)]
pub struct Report {
    /// Violations not covered by the allowlist.
    pub violations: Vec<Violation>,
    /// Violations covered by the allowlist.
    pub allowed: Vec<Violation>,
    /// Allowlist entries `(crate, dependency, line)` that match no violation.
    pub stale: Vec<(String, String, usize)>,
}

impl Report {
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty() && self.stale.is_empty()
    }
}

/// Match `found` against `allow`.
pub fn check(found: Vec<Violation>, allow: &Allowlist) -> Report {
    let mut report = Report::default();
    let mut used = BTreeSet::new();
    for violation in found {
        let key = (violation.krate.clone(), violation.dependency.clone());
        if allow.entries.contains_key(&key) {
            used.insert(key);
            report.allowed.push(violation);
        } else {
            report.violations.push(violation);
        }
    }
    report.stale = allow
        .entries
        .iter()
        .filter(|(key, _)| !used.contains(*key))
        .map(|((krate, dep), entry)| (krate.clone(), dep.clone(), entry.line))
        .collect();
    report
}

/// Run `cargo metadata` for this workspace and check it against the allowlist.
fn check_workspace() -> Result<Report, Vec<String>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .output()
        .map_err(|e| vec![format!("cannot run cargo metadata: {e}")])?;
    if !output.status.success() {
        return Err(vec![format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )]);
    }
    let json = String::from_utf8(output.stdout)
        .map_err(|e| vec![format!("cargo metadata printed invalid UTF-8: {e}")])?;
    let found = violations_from_metadata_json(&json)?;
    let allow_text = std::fs::read_to_string(root.join(ALLOWLIST_PATH))
        .map_err(|e| vec![format!("cannot read {ALLOWLIST_PATH}: {e}")])?;
    let allow = Allowlist::parse(&allow_text)?;
    Ok(check(found, &allow))
}

/// Entry point for `cargo xtask check-errors`.
pub fn run() -> ExitCode {
    let report = match check_workspace() {
        Ok(report) => report,
        Err(errors) => {
            for e in errors {
                eprintln!("error check: {e}");
            }
            return ExitCode::FAILURE;
        }
    };
    for v in &report.violations {
        eprintln!("error check: {v}");
    }
    for (krate, dep, line) in &report.stale {
        eprintln!(
            "error check: {ALLOWLIST_PATH}:{line}: `{krate}` no longer depends on `{dep}`; \
             remove the entry"
        );
    }
    if report.is_ok() {
        println!("error check: ok ({} allowlisted)", report.allowed.len());
        ExitCode::SUCCESS
    } else {
        if !report.violations.is_empty() {
            eprintln!(
                "error check: return a typed error instead (see \"Error handling\" in \
                 docs/architecture.md), or as a last resort add \
                 `<crate> -> <dependency>: <reason>` to {ALLOWLIST_PATH}"
            );
        }
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `cargo metadata` JSON: `(name, dir, [(dep, kind)])`.
    fn metadata(packages: &[(&str, &str, &[(&str, Option<&str>)])]) -> String {
        let packages: Vec<serde_json::Value> = packages
            .iter()
            .map(|(name, dir, deps)| {
                let deps: Vec<serde_json::Value> = deps
                    .iter()
                    .map(|(dep, kind)| serde_json::json!({ "name": dep, "kind": kind }))
                    .collect();
                serde_json::json!({
                    "name": name,
                    "manifest_path": format!("/ws/{dir}/Cargo.toml"),
                    "dependencies": deps,
                })
            })
            .collect();
        serde_json::json!({ "workspace_root": "/ws", "packages": packages }).to_string()
    }

    #[test]
    fn a_library_crate_may_not_depend_on_anyhow() {
        let json = metadata(&[("leek-thing", "crates/core/leek-thing", &[("anyhow", None)])]);
        let found = violations_from_metadata_json(&json).expect("metadata parses");
        assert_eq!(
            found,
            [Violation {
                krate: "leek-thing".to_string(),
                layer: Layer::Core,
                dependency: "anyhow".to_string(),
            }]
        );
    }

    #[test]
    fn front_ends_and_test_harnesses_may() {
        let json = metadata(&[
            ("miku", "bins/miku", &[("anyhow", None)]),
            ("xtask", "xtask", &[("anyhow", None)]),
            (
                "leek-bench",
                "crates/testing/leek-bench",
                &[("anyhow", None)],
            ),
        ]);
        assert!(
            violations_from_metadata_json(&json)
                .expect("metadata parses")
                .is_empty()
        );
    }

    #[test]
    fn a_dev_dependency_is_not_a_violation() {
        // Tests are front-ends too, and can't leak a type through the API.
        let json = metadata(&[(
            "leek-thing",
            "crates/core/leek-thing",
            &[("anyhow", Some("dev"))],
        )]);
        assert!(
            violations_from_metadata_json(&json)
                .expect("metadata parses")
                .is_empty()
        );
    }

    #[test]
    fn a_build_dependency_is_a_violation() {
        let json = metadata(&[(
            "leek-thing",
            "crates/core/leek-thing",
            &[("anyhow", Some("build"))],
        )]);
        assert_eq!(
            violations_from_metadata_json(&json)
                .expect("metadata parses")
                .len(),
            1
        );
    }

    #[test]
    fn an_allowlisted_violation_passes_and_an_unlisted_one_does_not() {
        let found = vec![
            Violation {
                krate: "leek-listed".to_string(),
                layer: Layer::Db,
                dependency: "anyhow".to_string(),
            },
            Violation {
                krate: "leek-unlisted".to_string(),
                layer: Layer::Db,
                dependency: "anyhow".to_string(),
            },
        ];
        let allow = Allowlist::parse("leek-listed -> anyhow: being converted\n").expect("parses");
        let report = check(found, &allow);
        assert_eq!(report.allowed.len(), 1);
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].krate, "leek-unlisted");
        assert!(!report.is_ok());
    }

    #[test]
    fn the_allowlist_may_only_shrink() {
        let allow = Allowlist::parse("leek-fixed -> anyhow: was converted\n").expect("parses");
        let report = check(Vec::new(), &allow);
        assert_eq!(
            report.stale,
            [("leek-fixed".to_string(), "anyhow".to_string(), 1)]
        );
        assert!(!report.is_ok(), "a stale entry must fail the check");
    }

    #[test]
    fn a_malformed_allowlist_line_is_rejected() {
        let errors = Allowlist::parse("leek-thing -> anyhow\n").expect_err("no reason");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("expected"), "{errors:?}");

        let errors = Allowlist::parse("leek-thing -> anyhow:  \n").expect_err("empty reason");
        assert!(errors[0].contains("justification"), "{errors:?}");
    }

    /// The real workspace must agree with the checked-in allowlist. This is
    /// the same check `cargo xtask check-errors` runs, wired into `cargo test`
    /// so a new `anyhow` dependency fails the unit suite too.
    #[test]
    fn the_real_workspace_matches_its_allowlist() {
        let report = check_workspace().expect("workspace metadata");
        assert!(
            report.is_ok(),
            "violations: {:?}\nstale: {:?}",
            report.violations,
            report.stale
        );
    }
}
