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
use std::path::{Path, PathBuf};

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

impl LoadError {
    /// Convert a folder load failure into a resolver diagnostic at
    /// the `include(...)` call site.
    pub fn to_diagnostic(
        self,
        span: leek_span::Span,
        include_name: &str,
    ) -> leek_diagnostics::Diagnostic {
        use leek_diagnostics::{Diagnostic, codes};
        match self {
            LoadError::NotFound => Diagnostic::error(
                codes::INCLUDE_NOT_FOUND,
                span,
                format!("included file `{include_name}` not found"),
            ),
            LoadError::Unreadable(msg) => Diagnostic::error(
                codes::INCLUDE_UNREADABLE,
                span,
                format!("included file `{include_name}` is unreadable: {msg}"),
            ),
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
        let canonical = candidate.canonicalize().unwrap_or(candidate.clone());
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
        self.files.insert(path.into(), text.into());
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
            parent.join(format!("{name}.leek")),
            parent.join(name),
            PathBuf::from(name),
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
}
