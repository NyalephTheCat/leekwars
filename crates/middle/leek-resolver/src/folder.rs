//! File-namespace abstraction for `include("name")` resolution.
//!
//! Leekscript's `include` statement takes a string-literal path and
//! resolves it relative to a "folder" — see
//! [`doc/pipeline.md`](../../../doc/pipeline.md) §5.1.2. The folder
//! mediates the lookup so the compiler can be embedded in different
//! environments: local disk (the `leekc` / `miku` CLI), an in-memory
//! workspace (the LSP server), the LeekWars asset bundle, or
//! test-only fakes.
//!
//! `Folder` is the trait. Implementors:
//! - [`DiskFolder`] — resolves relative to a base directory on disk.
//! - In-memory test folders ([`mem::MemFolder`]) live alongside.
//!
//! ## Resolution rules
//!
//! `Folder::resolve(base, name)` interprets `name` relative to the
//! file whose path is `base`. Plain names (`include("util")`) resolve
//! sibling-to the includer. Subfolder names (`include("lib/util")`)
//! traverse down. The trait returns the resolved absolute path (or
//! virtual path for in-memory folders) plus the file's contents —
//! callers don't open the file again separately.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Outcome of `Folder::load`. Carries both the canonical path and
/// the bytes so callers don't double-stat or double-read.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    /// Canonical / normalized path used as the key for the include
    /// graph. Distinct includer-relative spellings of the same file
    /// (`./util`, `util`, `../src/util`) must produce the same
    /// canonical form, otherwise the graph deduplicates wrongly.
    pub path: PathBuf,
    /// The file's contents.
    pub text: String,
}

/// Errors that may surface during `Folder::load`. Resolver lifts
/// these into proper diagnostics with spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// `name` couldn't be resolved to any file. Maps to
    /// `AI_NOT_EXISTING` in the upstream's diagnostic table.
    NotFound,
    /// The file resolved but couldn't be read (permission denied,
    /// not utf-8, …). The string carries the underlying message.
    Unreadable(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotFound => write!(f, "include name not found"),
            LoadError::Unreadable(msg) => write!(f, "include file unreadable: {msg}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// A failed `include(...)` — a [`LoadError`] together with the call
/// site needed to render it. Self-contained so it can convert via
/// [`IntoDiagnostic`](leek_diagnostics::IntoDiagnostic).
pub struct IncludeError<'a> {
    pub cause: LoadError,
    pub span: leek_span::Span,
    pub name: &'a str,
}

impl leek_diagnostics::IntoDiagnostic for IncludeError<'_> {
    fn into_diagnostic(self) -> leek_diagnostics::Diagnostic {
        use leek_diagnostics::{codes, diag};
        let IncludeError { cause, span, name } = self;
        match cause {
            LoadError::NotFound => {
                diag!(
                    codes::INCLUDE_NOT_FOUND,
                    span,
                    "included file `{name}` not found"
                )
            }
            LoadError::Unreadable(msg) => {
                diag!(
                    codes::INCLUDE_UNREADABLE,
                    span,
                    "included file `{name}` is unreadable: {msg}"
                )
            }
        }
    }
}

/// The Folder abstraction.
///
/// Implementors map a `(includer_path, include_name)` pair to a
/// concrete file (path + bytes). The trait is intentionally tiny —
/// everything else (cycle detection, version-aware tokenization,
/// symbol-table merge) is handled by the resolver/lowerer using
/// `Folder` as the underlying I/O.
pub trait Folder: Send + Sync {
    /// Locate and read the file `name` referenced from `includer`.
    /// `includer` is the canonical path of the file containing the
    /// `include("name")` site. For top-level entry points, callers
    /// pass the entry file's own canonical path so relative names
    /// resolve next to it.
    fn load(&self, includer: &Path, name: &str) -> Result<LoadedFile, LoadError>;
}

/// Normalize `.` and `..` without touching the filesystem.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalize a real path, or lexically normalize a virtual/nonexistent one.
pub fn canonical_or_normalized(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| normalize_path(path))
}

/// Resolves include names against the filesystem, treating each
/// name as a `.leek` file relative to the includer's directory.
/// `name` may contain `/` segments for sub-folders.
pub struct DiskFolder;

impl Folder for DiskFolder {
    fn load(&self, includer: &Path, name: &str) -> Result<LoadedFile, LoadError> {
        let base = includer
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        // `include("util")` looks for `util.leek` first, then
        // `util` (matching upstream's `Folder.resolve` behaviour
        // — names may or may not carry the extension).
        let with_ext = base.join(format!("{name}.leek"));
        let bare = base.join(name);
        let candidate = if with_ext.is_file() {
            with_ext
        } else if bare.is_file() {
            bare
        } else {
            return Err(LoadError::NotFound);
        };
        let canonical = canonical_or_normalized(&candidate);
        let text = std::fs::read_to_string(&canonical)
            .map_err(|e| LoadError::Unreadable(e.to_string()))?;
        Ok(LoadedFile {
            path: canonical,
            text,
        })
    }
}

/// In-memory `Folder` for tests and the LSP's open-document layer.
/// Keys are virtual paths (e.g. `"file:///proj/main.leek"`) and
/// names are looked up by direct lookup or via a sibling-resolved
/// path (`<dirname-of-includer>/<name>` and `<…>/<name>.leek`).
pub struct MemFolder {
    files: BTreeMap<PathBuf, String>,
}

impl MemFolder {
    pub fn new() -> Self {
        Self {
            files: BTreeMap::new(),
        }
    }

    /// Insert a file. `path` is treated as the canonical name —
    /// callers should pass the same form they'd pass as the
    /// `includer` to `load`.
    pub fn insert(&mut self, path: impl Into<PathBuf>, text: impl Into<String>) {
        self.files.insert(normalize_path(&path.into()), text.into());
    }

    /// Build with a single entry shortcut.
    pub fn with_file(path: impl Into<PathBuf>, text: impl Into<String>) -> Self {
        let mut f = Self::new();
        f.insert(path, text);
        f
    }
}

impl Default for MemFolder {
    fn default() -> Self {
        Self::new()
    }
}

impl Folder for MemFolder {
    fn load(&self, includer: &Path, name: &str) -> Result<LoadedFile, LoadError> {
        let parent = includer.parent().map(Path::to_path_buf).unwrap_or_default();
        // Candidate paths in priority order — matches DiskFolder's
        // policy (sibling `.leek`, sibling bare, raw name).
        let candidates = [
            normalize_path(&parent.join(format!("{name}.leek"))),
            normalize_path(&parent.join(name)),
            normalize_path(Path::new(name)),
        ];
        for c in &candidates {
            if let Some(text) = self.files.get(c) {
                return Ok(LoadedFile {
                    path: c.clone(),
                    text: text.clone(),
                });
            }
        }
        Err(LoadError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_folder_resolves_sibling() {
        let mut f = MemFolder::new();
        f.insert("/proj/main.leek", "include(\"util\")");
        f.insert("/proj/util.leek", "function helper() {}");
        let got = f.load(Path::new("/proj/main.leek"), "util").unwrap();
        assert_eq!(got.path, PathBuf::from("/proj/util.leek"));
        assert!(got.text.contains("helper"));
    }

    #[test]
    fn mem_folder_missing_returns_not_found() {
        let f = MemFolder::with_file("/proj/main.leek", "");
        let err = f.load(Path::new("/proj/main.leek"), "ghost").unwrap_err();
        assert_eq!(err, LoadError::NotFound);
    }

    #[test]
    fn mem_folder_resolves_subfolder() {
        let mut f = MemFolder::new();
        f.insert("/proj/main.leek", "");
        f.insert("/proj/lib/util.leek", "function k() {}");
        let got = f.load(Path::new("/proj/main.leek"), "lib/util").unwrap();
        assert_eq!(got.path, PathBuf::from("/proj/lib/util.leek"));
    }

    #[test]
    fn mem_folder_normalizes_parent_directory() {
        let mut f = MemFolder::new();
        f.insert("/proj/shared/constants.leek", "var INCLUDED_VALUE = 1");
        let got = f
            .load(
                Path::new("/proj/src/entry.leek"),
                "../shared/constants.leek",
            )
            .unwrap();
        assert_eq!(got.path, PathBuf::from("/proj/shared/constants.leek"));
    }
}
