//! Manifest types — the runtime shape of `Miku.toml` data.

use std::path::PathBuf;

use crate::format::FormatOptions;

/// The whole `Miku.toml` after parsing + defaults.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub project: ProjectTable,
    pub paths: PathsTable,
    pub backend: BackendTable,
    pub lint: LintTable,
    pub format: FormatOptions,
    pub test: TestTable,
    pub fight: FightTable,
}

/// `[project]` — required.
#[derive(Debug, Clone)]
pub struct ProjectTable {
    pub name: String,
    pub version: String,
    /// Default `@version` for sources that omit the pragma. 1..=4.
    pub language: u8,
    /// Default `@strict` for sources.
    pub strict: bool,
    /// Entry point. Default `src/main.leek`.
    pub entry: PathBuf,
    pub authors: Vec<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub repository: Option<String>,
}

impl ProjectTable {
    pub(crate) fn defaults_with(name: String, version: String) -> Self {
        Self {
            name,
            version,
            language: 4,
            strict: false,
            entry: PathBuf::from("src/main.leek"),
            authors: Vec::new(),
            description: None,
            license: None,
            repository: None,
        }
    }
}

/// `[paths]` — directory layout. All paths are relative to the
/// project root.
#[derive(Debug, Clone)]
pub struct PathsTable {
    pub src: PathBuf,
    pub tests: PathBuf,
    pub benches: PathBuf,
    /// The single output root every command writes under: backend
    /// artifacts, generated docs, fight reports. `miku clean` removes
    /// it, so the parser keeps it a relative path inside the project.
    pub build: PathBuf,
}

impl Default for PathsTable {
    fn default() -> Self {
        Self {
            src: PathBuf::from("src"),
            tests: PathBuf::from("tests"),
            benches: PathBuf::from("benches"),
            build: PathBuf::from("build"),
        }
    }
}

/// `[backend.*]` — one entry per backend kind.
#[derive(Debug, Clone, Default)]
pub struct BackendTable {
    pub java: Option<BackendSettings>,
    pub jar: Option<BackendSettings>,
    pub native: Option<BackendSettings>,
    pub wasm: Option<BackendSettings>,
    pub leekscript: Option<BackendSettings>,
}

impl BackendTable {
    /// Which backend `miku run` / `miku build` (no flag) should use.
    ///
    /// The backend marked `default = true` wins. The parser rejects a
    /// manifest that marks two of them, or that marks a disabled backend
    /// default, so at most one match reaches here. With no `default`, falls
    /// back to the first *enabled* backend in the order
    /// java → jar → native → wasm → leekscript.
    pub fn default_kind(&self) -> Option<BackendKind> {
        let entries: [(BackendKind, &Option<BackendSettings>); 5] = [
            (BackendKind::Java, &self.java),
            (BackendKind::Jar, &self.jar),
            (BackendKind::Native, &self.native),
            (BackendKind::Wasm, &self.wasm),
            (BackendKind::LeekScript, &self.leekscript),
        ];
        debug_assert!(
            entries
                .iter()
                .filter(|(_, slot)| slot.as_ref().is_some_and(|s| s.is_default))
                .count()
                <= 1,
            "the parser rejects two `default = true` backends"
        );
        for (kind, slot) in &entries {
            if let Some(s) = slot
                && s.is_default
            {
                return Some(*kind);
            }
        }
        for (kind, slot) in &entries {
            if let Some(s) = slot
                && s.enable
            {
                return Some(*kind);
            }
        }
        None
    }

    pub fn get(&self, kind: BackendKind) -> Option<&BackendSettings> {
        match kind {
            BackendKind::Java => self.java.as_ref(),
            BackendKind::Jar => self.jar.as_ref(),
            BackendKind::Native => self.native.as_ref(),
            BackendKind::Wasm => self.wasm.as_ref(),
            BackendKind::LeekScript => self.leekscript.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Java,
    Jar,
    Native,
    Wasm,
    /// Emit desugared official LeekScript source.
    LeekScript,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Java => "java",
            BackendKind::Jar => "jar",
            BackendKind::Native => "native",
            BackendKind::Wasm => "wasm",
            BackendKind::LeekScript => "leekscript",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "java" => BackendKind::Java,
            "jar" => BackendKind::Jar,
            "native" => BackendKind::Native,
            "wasm" => BackendKind::Wasm,
            "leekscript" => BackendKind::LeekScript,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct BackendSettings {
    pub enable: bool,
    pub is_default: bool,
    /// Java mode — only meaningful for `BackendKind::Java`.
    pub java_mode: Option<JavaMode>,
    /// Output directory override. Where each backend writes its
    /// artifacts; interpretation is backend-specific.
    pub out_dir: Option<PathBuf>,
    /// Single-file output path. `[backend.native].out` names the standalone
    /// executable `miku build` writes; a relative path resolves against the
    /// project root.
    pub out: Option<PathBuf>,
    /// `[backend.java].emit_lines` — emit a `.lines` sidecar.
    pub emit_lines: bool,
    /// `[backend.jar].main_class`. Parsed, then reported as
    /// [`IgnoredKey`](crate::ManifestWarningKind::IgnoredKey): the jar
    /// backend is not implemented.
    pub main_class: Option<String>,
    /// `[backend.native].target`. Parsed, then reported as
    /// [`IgnoredKey`](crate::ManifestWarningKind::IgnoredKey): the native
    /// backend compiles for the host only.
    pub target: Option<String>,
    /// `[backend.native].opt_level` — the level the AOT and JIT compilers
    /// optimize at. `None` keeps the backend's own default.
    pub opt_level: Option<NativeOptLevel>,
    /// `[backend.native].max_call_depth` — nested user-function calls allowed
    /// before a run fails with `STACKOVERFLOW` (`None`: the backend default).
    pub max_call_depth: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavaMode {
    Exact,
    Clean,
}

/// `[backend.native].opt_level` — spelled the same way as `leekc`'s
/// `--opt-level` value enum so the two front-ends agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeOptLevel {
    /// No optimization (debug-friendly).
    None,
    /// Optimize for speed.
    Speed,
    /// Optimize for speed and code size.
    SpeedAndSize,
}

impl NativeOptLevel {
    /// The manifest spelling, and what the error message lists.
    pub const NAMES: &'static [&'static str] = &["none", "speed", "speed-and-size"];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => NativeOptLevel::None,
            "speed" => NativeOptLevel::Speed,
            "speed-and-size" => NativeOptLevel::SpeedAndSize,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NativeOptLevel::None => "none",
            NativeOptLevel::Speed => "speed",
            NativeOptLevel::SpeedAndSize => "speed-and-size",
        }
    }
}

/// `[lint]` — severity overrides applied on top of the catalog defaults,
/// plus opt-in lint groups.
#[derive(Debug, Clone, Default)]
pub struct LintTable {
    pub deny: Vec<String>,
    pub warn: Vec<String>,
    pub allow: Vec<String>,
    /// Byte spans of the `deny` array's elements, positionally aligned with
    /// [`deny`](Self::deny). The reporter points at the element that named an
    /// unknown code rather than at the whole `[lint]` table. Empty when the
    /// manifest came from somewhere with no document to span (a test that
    /// builds a `LintTable` by hand); an element may be `None` when the
    /// document had no span for it.
    pub deny_spans: Vec<Option<leek_span::Span>>,
    /// Spans for [`warn`](Self::warn); see [`deny_spans`](Self::deny_spans).
    pub warn_spans: Vec<Option<leek_span::Span>>,
    /// Spans for [`allow`](Self::allow); see [`deny_spans`](Self::deny_spans).
    pub allow_spans: Vec<Option<leek_span::Span>>,
    /// `lint.pedantic = true` — run the strictness lints.
    pub pedantic: bool,
    /// `lint.nursery = true` — run the teaching lints.
    pub nursery: bool,
}

impl LintTable {
    /// The span recorded for `raw` in whichever of the three lists holds it.
    /// `None` when the entry isn't there, or the document had no span for it.
    pub fn span_for(&self, raw: &str) -> Option<leek_span::Span> {
        for (entries, spans) in [
            (&self.deny, &self.deny_spans),
            (&self.warn, &self.warn_spans),
            (&self.allow, &self.allow_spans),
        ] {
            if let Some(i) = entries.iter().position(|e| e == raw) {
                return spans.get(i).copied().flatten();
            }
        }
        None
    }
}

/// `[test]` — runner configuration.
#[derive(Debug, Clone, Default)]
pub struct TestTable {
    /// Per-test budget in *operations* — the unit the runner enforces (the
    /// JIT counts ops; there is no wall clock). A per-file
    /// `// miku-test: timeout <ops>` annotation overrides it, and `None`
    /// falls back to the backend's default budget.
    pub timeout: Option<u64>,
    /// Parsed, then reported as
    /// [`IgnoredKey`](crate::ManifestWarningKind::IgnoredKey): the runner is
    /// sequential (leekwars#133 — `Pipeline` / `Run` are `!Send`). The
    /// default is `false` so the struct stops asserting something untrue.
    pub parallel: bool,
    pub junit_xml: Option<PathBuf>,
}

/// `[fight]` — defaults for `miku fight`.
#[derive(Debug, Clone)]
pub struct FightTable {
    /// Scenario played when `miku fight` is given no path. Resolved like
    /// any other scenario argument (see [`FightTable::scenarios_dir`]).
    pub default_scenario: Option<PathBuf>,
    /// Directory searched for scenarios named without a directory part.
    /// `None` means "look next to the manifest".
    pub scenarios_dir: Option<PathBuf>,
    /// Where `--report` writes when it is given no path.
    /// Default `build/fight-reports`.
    pub reports_dir: PathBuf,
    /// Worker count for the parallel sweep drivers. `None` means "let the
    /// driver decide". Parsed and exposed today; the matrix/tournament/
    /// random drivers still run sequentially.
    pub jobs: Option<u32>,
}

impl Default for FightTable {
    fn default() -> Self {
        Self {
            default_scenario: None,
            scenarios_dir: None,
            reports_dir: PathBuf::from("build/fight-reports"),
            jobs: None,
        }
    }
}
