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
}

/// `[project]` — required.
#[derive(Debug, Clone)]
pub struct ProjectTable {
    pub name: String,
    pub version: String,
    pub edition: Option<String>,
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
            edition: None,
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
}

impl Default for PathsTable {
    fn default() -> Self {
        Self {
            src: PathBuf::from("src"),
            tests: PathBuf::from("tests"),
            benches: PathBuf::from("benches"),
        }
    }
}

/// `[backend.*]` — one entry per backend kind.
#[derive(Debug, Clone, Default)]
pub struct BackendTable {
    pub java: Option<BackendSettings>,
    pub jar: Option<BackendSettings>,
    pub native: Option<BackendSettings>,
    pub interp: Option<BackendSettings>,
    pub wasm: Option<BackendSettings>,
}

impl BackendTable {
    /// Which backend `miku run` / `miku build` (no flag) should use.
    ///
    /// If exactly one backend has `default = true`, that wins. Otherwise
    /// falls back to the first enabled backend in the order
    /// java → interp → jar → native → wasm.
    pub fn default_kind(&self) -> Option<BackendKind> {
        let entries: [(BackendKind, &Option<BackendSettings>); 5] = [
            (BackendKind::Java, &self.java),
            (BackendKind::Interp, &self.interp),
            (BackendKind::Jar, &self.jar),
            (BackendKind::Native, &self.native),
            (BackendKind::Wasm, &self.wasm),
        ];
        for (kind, slot) in &entries {
            if let Some(s) = slot
                && s.is_default {
                    return Some(*kind);
                }
        }
        for (kind, slot) in &entries {
            if let Some(s) = slot
                && s.enable {
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
            BackendKind::Interp => self.interp.as_ref(),
            BackendKind::Wasm => self.wasm.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Java,
    Jar,
    Native,
    Interp,
    Wasm,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Java => "java",
            BackendKind::Jar => "jar",
            BackendKind::Native => "native",
            BackendKind::Interp => "interp",
            BackendKind::Wasm => "wasm",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "java" => BackendKind::Java,
            "jar" => BackendKind::Jar,
            "native" => BackendKind::Native,
            "interp" => BackendKind::Interp,
            "wasm" => BackendKind::Wasm,
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
    /// Single-file output (e.g. `[backend.jar].out`).
    pub out: Option<PathBuf>,
    /// `[backend.java].emit_lines` — emit a `.lines` sidecar.
    pub emit_lines: bool,
    /// `[backend.java].java_version` (clean mode).
    pub java_version: Option<u32>,
    /// `[backend.jar].main_class`.
    pub main_class: Option<String>,
    /// `[backend.native].target`.
    pub target: Option<String>,
    /// `[backend.native].opt_level`.
    pub opt_level: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JavaMode {
    Exact,
    Clean,
}

/// `[lint]` — severity overrides applied on top of the catalog defaults.
#[derive(Debug, Clone, Default)]
pub struct LintTable {
    pub deny: Vec<String>,
    pub warn: Vec<String>,
    pub allow: Vec<String>,
}

/// `[test]` — runner configuration.
#[derive(Debug, Clone)]
pub struct TestTable {
    /// Per-test timeout as a free-form duration string (e.g. "5s").
    /// `None` means no explicit timeout; the runner picks a default.
    pub timeout: Option<String>,
    pub parallel: bool,
    pub junit_xml: Option<PathBuf>,
}

impl Default for TestTable {
    fn default() -> Self {
        Self {
            timeout: None,
            parallel: true,
            junit_xml: None,
        }
    }
}
