//! `cargo xtask check-layers`: enforce the crate layering rule.
//!
//! The workspace graph comes from `cargo metadata --no-deps`, so every member
//! is checked and no crate list has to be kept in sync by hand. A crate's
//! layer is taken from where it lives:
//!
//! | path                     | layer    | rank |
//! |--------------------------|----------|------|
//! | `crates/core/<crate>`     | core     | 0 |
//! | `crates/frontend/<crate>` | frontend | 1 |
//! | `crates/middle/<crate>`   | middle   | 2 |
//! | `crates/db/<crate>`       | db       | 3 |
//! | `crates/backends/<crate>` | backends | 4 |
//! | `crates/game/<crate>`     | game     | 5 |
//! | `crates/tools/<crate>`    | tools    | 6 |
//! | `crates/testing/<crate>`  | testing  | 6 |
//! | `bins/<crate>`            | bins     | 7 |
//! | `xtask`                   | xtask    | 7 |
//!
//! A member anywhere else is an error. The rules, per dependency edge between
//! two workspace members:
//!
//! - **normal and build** dependencies stay inside their layer or point at a
//!   strictly lower rank. Peer layers (same rank, different layer) may not
//!   depend on each other.
//! - **dev** dependencies may reach at most one rank higher than their crate,
//!   peers included, so tests can use the next layer up.
//! - a **substrate** crate (`leek-pipeline`) may only depend on its ceiling
//!   layer (`core`) or lower, apart from dev dependencies.
//!
//! Exceptions are listed, with a reason, in `xtask/layer-allowlist.txt`. An
//! entry that no longer matches a violation is itself an error, so the list
//! can only shrink.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Deserialize;

/// Allowlist location, relative to the workspace root.
const ALLOWLIST_PATH: &str = "xtask/layer-allowlist.txt";

/// Crates that may depend on nothing above the given layer (dev dependencies
/// excepted), whatever layer they live in themselves.
const SUBSTRATES: &[(&str, Layer)] = &[
    // The generic orchestration engine must not know any concrete stage.
    ("leek-pipeline", Layer::Core),
];

/// A workspace layer, derived from a crate's directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Core,
    Frontend,
    Middle,
    Db,
    Backends,
    Game,
    Tools,
    Testing,
    Bins,
    Xtask,
}

impl Layer {
    /// Layers that live under `crates/<name>/`.
    const UNDER_CRATES: [Self; 8] = [
        Self::Core,
        Self::Frontend,
        Self::Middle,
        Self::Db,
        Self::Backends,
        Self::Game,
        Self::Tools,
        Self::Testing,
    ];

    /// Directory / display name of the layer.
    pub fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Frontend => "frontend",
            Self::Middle => "middle",
            Self::Db => "db",
            Self::Backends => "backends",
            Self::Game => "game",
            Self::Tools => "tools",
            Self::Testing => "testing",
            Self::Bins => "bins",
            Self::Xtask => "xtask",
        }
    }

    /// Position in the stack. Layers with equal rank are peers.
    pub fn rank(self) -> u8 {
        match self {
            Self::Core => 0,
            Self::Frontend => 1,
            Self::Middle => 2,
            Self::Db => 3,
            Self::Backends => 4,
            Self::Game => 5,
            Self::Tools | Self::Testing => 6,
            Self::Bins | Self::Xtask => 7,
        }
    }

    /// Layer of a crate whose directory is `dir`, relative to the workspace
    /// root. `None` when the directory is not a recognized layer location.
    pub fn from_dir(dir: &Path) -> Option<Self> {
        let parts: Option<Vec<&str>> = dir
            .components()
            .map(|c| match c {
                Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();
        match parts?.as_slice() {
            ["crates", layer, _crate] => {
                Self::UNDER_CRATES.into_iter().find(|l| l.name() == *layer)
            }
            ["bins", _crate] => Some(Self::Bins),
            ["xtask"] => Some(Self::Xtask),
            _ => None,
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which manifest table a dependency comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DepKind {
    Normal,
    Build,
    Dev,
}

impl DepKind {
    fn from_metadata(kind: Option<&str>) -> Result<Self, String> {
        match kind {
            None => Ok(Self::Normal),
            Some("build") => Ok(Self::Build),
            Some("dev") => Ok(Self::Dev),
            Some(other) => Err(format!("unknown dependency kind `{other}`")),
        }
    }
}

impl fmt::Display for DepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Normal => "normal",
            Self::Build => "build",
            Self::Dev => "dev",
        })
    }
}

/// A dependency between two workspace members.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: DepKind,
}

/// The workspace members, their layers, and the edges between them.
#[derive(Debug, Default)]
pub struct Workspace {
    pub crates: BTreeMap<String, Layer>,
    pub edges: BTreeSet<Edge>,
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
    path: Option<PathBuf>,
}

impl Workspace {
    /// Build the graph from `cargo metadata` JSON. Fails, listing every
    /// offender, when a member lives outside a known layer location.
    pub fn from_metadata_json(json: &str) -> Result<Self, Vec<String>> {
        let meta: Metadata = serde_json::from_str(json)
            .map_err(|e| vec![format!("cannot parse cargo metadata: {e}")])?;
        let mut errors = Vec::new();
        let mut ws = Self::default();

        for pkg in &meta.packages {
            let dir = pkg.manifest_path.parent().unwrap_or(&pkg.manifest_path);
            let rel = dir.strip_prefix(&meta.workspace_root).unwrap_or(dir);
            match Layer::from_dir(rel) {
                Some(layer) => {
                    ws.crates.insert(pkg.name.clone(), layer);
                }
                None => errors.push(format!(
                    "{} ({}) is not in a known layer; move it under crates/<layer>/, \
                     bins/ or xtask/",
                    pkg.name,
                    rel.display()
                )),
            }
        }

        for pkg in &meta.packages {
            for dep in &pkg.dependencies {
                // Registry/git crates are outside the rule; so are path
                // dependencies that are not workspace members.
                if dep.path.is_none() || !ws.crates.contains_key(&dep.name) {
                    continue;
                }
                match DepKind::from_metadata(dep.kind.as_deref()) {
                    Ok(kind) => {
                        ws.edges.insert(Edge {
                            from: pkg.name.clone(),
                            to: dep.name.clone(),
                            kind,
                        });
                    }
                    Err(e) => errors.push(format!("{} -> {}: {e}", pkg.name, dep.name)),
                }
            }
        }

        if errors.is_empty() {
            Ok(ws)
        } else {
            Err(errors)
        }
    }
}

/// Why an edge breaks the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breach {
    /// Normal/build dependency on a higher layer.
    Upward,
    /// Normal/build dependency on a different layer of the same rank.
    Peer,
    /// Dev dependency more than one rank up.
    DevTooHigh,
    /// Substrate crate depending above its ceiling layer.
    Substrate(Layer),
}

impl fmt::Display for Breach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Upward => f.write_str("depends on a higher layer"),
            Self::Peer => f.write_str("depends on a peer layer"),
            Self::DevTooHigh => f.write_str("dev-dependency reaches more than one layer up"),
            Self::Substrate(ceiling) => write!(f, "substrate crate may only depend on {ceiling}"),
        }
    }
}

/// Decide whether `from_crate` (in `from`) may depend on a crate in `to`.
pub fn breach(from_crate: &str, from: Layer, to: Layer, kind: DepKind) -> Option<Breach> {
    if kind != DepKind::Dev
        && let Some(&(_, ceiling)) = SUBSTRATES.iter().find(|(name, _)| *name == from_crate)
        && to != ceiling
        && to.rank() >= ceiling.rank()
    {
        return Some(Breach::Substrate(ceiling));
    }
    if from == to {
        return None;
    }
    match kind {
        DepKind::Normal | DepKind::Build => match to.rank().cmp(&from.rank()) {
            std::cmp::Ordering::Less => None,
            std::cmp::Ordering::Equal => Some(Breach::Peer),
            std::cmp::Ordering::Greater => Some(Breach::Upward),
        },
        DepKind::Dev => (to.rank() > from.rank() + 1).then_some(Breach::DevTooHigh),
    }
}

/// One allowlisted edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowEntry {
    pub line: usize,
    pub reason: String,
}

/// Parsed `xtask/layer-allowlist.txt`, keyed by `(from, to)`.
#[derive(Debug, Default)]
pub struct Allowlist {
    pub entries: BTreeMap<(String, String), AllowEntry>,
}

impl Allowlist {
    /// Parse `<from> -> <to>: <reason>` lines; `#` starts a comment line.
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
                let (from, to) = edge.split_once("->")?;
                Some((from.trim(), to.trim(), reason.trim()))
            });
            let Some((from, to, reason)) = parsed else {
                errors.push(format!(
                    "{ALLOWLIST_PATH}:{line}: expected `<from> -> <to>: <reason>`"
                ));
                continue;
            };
            if from.is_empty() || to.is_empty() {
                errors.push(format!("{ALLOWLIST_PATH}:{line}: missing crate name"));
            } else if reason.is_empty() {
                errors.push(format!(
                    "{ALLOWLIST_PATH}:{line}: `{from} -> {to}` needs a justification"
                ));
            } else if let Some(prev) = list.entries.insert(
                (from.to_owned(), to.to_owned()),
                AllowEntry {
                    line,
                    reason: reason.to_owned(),
                },
            ) {
                errors.push(format!(
                    "{ALLOWLIST_PATH}:{line}: `{from} -> {to}` already listed on line {}",
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

/// An edge that breaks the rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub edge: Edge,
    pub from_layer: Layer,
    pub to_layer: Layer,
    pub breach: Breach,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}) -> {} ({}) [{}]: {}",
            self.edge.from,
            self.from_layer,
            self.edge.to,
            self.to_layer,
            self.edge.kind,
            self.breach
        )
    }
}

/// Outcome of checking a workspace against the rule and the allowlist.
#[derive(Debug, Default)]
pub struct Report {
    /// Violations not covered by the allowlist.
    pub violations: Vec<Violation>,
    /// Violations covered by the allowlist.
    pub allowed: Vec<Violation>,
    /// Allowlist entries `(from, to, line)` that match no violation.
    pub stale: Vec<(String, String, usize)>,
}

impl Report {
    pub fn is_ok(&self) -> bool {
        self.violations.is_empty() && self.stale.is_empty()
    }
}

/// Check every edge of `ws`, then match the violations against `allow`.
pub fn check(ws: &Workspace, allow: &Allowlist) -> Report {
    let mut report = Report::default();
    let mut used = BTreeSet::new();
    for edge in &ws.edges {
        let (Some(&from_layer), Some(&to_layer)) =
            (ws.crates.get(&edge.from), ws.crates.get(&edge.to))
        else {
            continue;
        };
        let Some(breach) = breach(&edge.from, from_layer, to_layer, edge.kind) else {
            continue;
        };
        let violation = Violation {
            edge: edge.clone(),
            from_layer,
            to_layer,
            breach,
        };
        let key = (edge.from.clone(), edge.to.clone());
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
        .map(|((from, to), entry)| (from.clone(), to.clone(), entry.line))
        .collect();
    report
}

/// Run `cargo metadata` for this workspace and check it against the allowlist.
fn check_workspace() -> Result<(Workspace, Report), Vec<String>> {
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
    let ws = Workspace::from_metadata_json(&json)?;
    let allow_text = std::fs::read_to_string(root.join(ALLOWLIST_PATH))
        .map_err(|e| vec![format!("cannot read {ALLOWLIST_PATH}: {e}")])?;
    let allow = Allowlist::parse(&allow_text)?;
    let report = check(&ws, &allow);
    Ok((ws, report))
}

/// Entry point for `cargo xtask check-layers`.
pub fn run() -> ExitCode {
    let (ws, report) = match check_workspace() {
        Ok(result) => result,
        Err(errors) => {
            for e in errors {
                eprintln!("layer check: {e}");
            }
            return ExitCode::FAILURE;
        }
    };
    for v in &report.violations {
        eprintln!("layer check: {v}");
    }
    for (from, to, line) in &report.stale {
        eprintln!(
            "layer check: {ALLOWLIST_PATH}:{line}: `{from} -> {to}` no longer breaks the \
             rule; remove the entry"
        );
    }
    if report.is_ok() {
        println!(
            "layer check: ok ({} crates, {} edges, {} allowlisted)",
            ws.crates.len(),
            ws.edges.len(),
            report.allowed.len()
        );
        ExitCode::SUCCESS
    } else {
        if !report.violations.is_empty() {
            eprintln!(
                "layer check: fix the dependency (see docs/architecture.md), or as a last \
                 resort add `<from> -> <to>: <reason>` to {ALLOWLIST_PATH}"
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
                    .map(|(dep, kind)| {
                        serde_json::json!({
                            "name": dep,
                            "kind": kind,
                            "path": format!("/ws/{dep}"),
                        })
                    })
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

    fn report(packages: &[(&str, &str, &[(&str, Option<&str>)])], allow: &str) -> Report {
        let ws = Workspace::from_metadata_json(&metadata(packages)).expect("valid workspace");
        check(&ws, &Allowlist::parse(allow).expect("valid allowlist"))
    }

    fn violations(r: &Report) -> Vec<String> {
        r.violations.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn layer_comes_from_the_directory() {
        assert_eq!(
            Layer::from_dir(Path::new("crates/core/leek-runtime")),
            Some(Layer::Core)
        );
        assert_eq!(
            Layer::from_dir(Path::new("crates/game/leek-generator")),
            Some(Layer::Game)
        );
        assert_eq!(Layer::from_dir(Path::new("bins/miku")), Some(Layer::Bins));
        assert_eq!(Layer::from_dir(Path::new("xtask")), Some(Layer::Xtask));
        assert_eq!(Layer::from_dir(Path::new("crates/misc/leek-x")), None);
        assert_eq!(Layer::from_dir(Path::new("crates/core")), None);
        assert_eq!(Layer::from_dir(Path::new("tools/leek-x")), None);
    }

    // The old script filed leek-runtime under `middle` and so let this edge
    // through; with the layer taken from the path it is caught.
    #[test]
    fn core_crate_depending_on_middle_is_reported() {
        let r = report(
            &[
                (
                    "leek-runtime",
                    "crates/core/leek-runtime",
                    &[("leek-hir", None)],
                ),
                ("leek-hir", "crates/middle/leek-hir", &[]),
            ],
            "",
        );
        assert_eq!(
            violations(&r),
            ["leek-runtime (core) -> leek-hir (middle) [normal]: depends on a higher layer"]
        );
    }

    // The old script skipped every crate missing from its hand-written map.
    #[test]
    fn crate_outside_a_layer_directory_fails() {
        let errors =
            Workspace::from_metadata_json(&metadata(&[("leek-new", "crates/misc/leek-new", &[])]))
                .expect_err("unknown layer must fail");
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].starts_with("leek-new (crates/misc/leek-new)"),
            "{errors:?}"
        );
    }

    // The old script read only `[dependencies]`.
    #[test]
    fn build_and_dev_dependencies_are_checked() {
        let r = report(
            &[
                (
                    "leek-complexity",
                    "crates/middle/leek-complexity",
                    &[
                        ("leek-backend-native", Some("dev")),
                        ("leek-gen", Some("build")),
                    ],
                ),
                (
                    "leek-backend-native",
                    "crates/backends/leek-backend-native",
                    &[],
                ),
                ("leek-gen", "crates/db/leek-gen", &[]),
            ],
            "",
        );
        assert_eq!(
            violations(&r),
            [
                "leek-complexity (middle) -> leek-backend-native (backends) [dev]: \
                 dev-dependency reaches more than one layer up",
                "leek-complexity (middle) -> leek-gen (db) [build]: depends on a higher layer",
            ]
        );
    }

    #[test]
    fn dev_dependency_one_layer_up_or_on_a_peer_is_allowed() {
        let r = report(
            &[
                (
                    "leek-resolver",
                    "crates/middle/leek-resolver",
                    &[("leek-recipes", Some("dev"))],
                ),
                ("leek-recipes", "crates/db/leek-recipes", &[]),
                (
                    "leek-migrate",
                    "crates/tools/leek-migrate",
                    &[("leek-test-corpus", Some("dev"))],
                ),
                ("leek-test-corpus", "crates/testing/leek-test-corpus", &[]),
            ],
            "",
        );
        assert!(r.is_ok(), "{:?}", violations(&r));
    }

    #[test]
    fn peer_layers_may_not_depend_on_each_other() {
        let r = report(
            &[
                (
                    "leek-migrate",
                    "crates/tools/leek-migrate",
                    &[("leek-test-corpus", None)],
                ),
                ("leek-test-corpus", "crates/testing/leek-test-corpus", &[]),
            ],
            "",
        );
        assert_eq!(
            violations(&r),
            [
                "leek-migrate (tools) -> leek-test-corpus (testing) [normal]: depends on a peer layer"
            ]
        );
    }

    #[test]
    fn game_sits_between_backends_and_tools() {
        let r = report(
            &[
                (
                    "leek-dap",
                    "crates/tools/leek-dap",
                    &[("leek-generator", None)],
                ),
                (
                    "leek-generator",
                    "crates/game/leek-generator",
                    &[("leek-backend-native", None)],
                ),
                (
                    "leek-backend-native",
                    "crates/backends/leek-backend-native",
                    &[],
                ),
                ("miku", "bins/miku", &[("leek-dap", None)]),
            ],
            "",
        );
        assert!(r.is_ok(), "{:?}", violations(&r));
    }

    // The old script skipped every edge into leek-pipeline.
    #[test]
    fn edges_into_the_substrate_are_not_exempt() {
        let r = report(
            &[
                (
                    "leek-lexer",
                    "crates/frontend/leek-lexer",
                    &[("leek-pipeline", None)],
                ),
                ("leek-pipeline", "crates/db/leek-pipeline", &[]),
            ],
            "",
        );
        assert_eq!(
            violations(&r),
            ["leek-lexer (frontend) -> leek-pipeline (db) [normal]: depends on a higher layer"]
        );
    }

    #[test]
    fn substrate_depends_only_on_core() {
        let r = report(
            &[
                (
                    "leek-pipeline",
                    "crates/db/leek-pipeline",
                    &[
                        ("leek-span", None),
                        ("leek-parser", None),
                        ("leek-hir", Some("dev")),
                    ],
                ),
                ("leek-span", "crates/core/leek-span", &[]),
                ("leek-parser", "crates/frontend/leek-parser", &[]),
                ("leek-hir", "crates/middle/leek-hir", &[]),
            ],
            "",
        );
        assert_eq!(
            violations(&r),
            ["leek-pipeline (db) -> leek-parser (frontend) [normal]: \
                 substrate crate may only depend on core"]
        );
    }

    #[test]
    fn allowlist_covers_listed_edges_and_flags_stale_ones() {
        let r = report(
            &[
                (
                    "leek-runtime",
                    "crates/core/leek-runtime",
                    &[("leek-hir", None)],
                ),
                ("leek-hir", "crates/middle/leek-hir", &[("leek-span", None)]),
                ("leek-span", "crates/core/leek-span", &[]),
            ],
            "# comment\n\
             leek-runtime -> leek-hir: stores DefId\n\
             leek-hir -> leek-span: fixed long ago\n",
        );
        assert!(r.violations.is_empty());
        assert_eq!(r.allowed.len(), 1);
        assert_eq!(
            r.stale,
            [("leek-hir".to_owned(), "leek-span".to_owned(), 3)]
        );
        assert!(!r.is_ok());
    }

    #[test]
    fn allowlist_rejects_malformed_entries() {
        let errors = Allowlist::parse("a -> b\na -> b:\na -> b: ok\na -> b: again\n -> b: x\n")
            .expect_err("malformed allowlist");
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(errors[0].ends_with(":1: expected `<from> -> <to>: <reason>`"));
        assert!(errors[1].ends_with(":2: `a -> b` needs a justification"));
        assert!(errors[2].ends_with(":4: `a -> b` already listed on line 3"));
        assert!(errors[3].ends_with(":5: missing crate name"));
    }

    /// The real workspace passes, and the allowlist has no stale entries.
    #[test]
    fn workspace_passes_the_layer_check() {
        let (ws, report) = check_workspace().unwrap_or_else(|e| panic!("{e:#?}"));
        assert!(
            ws.crates.len() > 40,
            "cargo metadata returned too few crates"
        );
        assert!(
            report.is_ok(),
            "violations: {:#?}\nstale: {:#?}",
            report.violations,
            report.stale
        );
    }
}
