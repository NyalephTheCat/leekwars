//! File-namespace abstraction for `include("name")` resolution.
//!
//! Leekscript's `include` statement takes a string-literal path and
//! resolves it relative to a "folder" — see `docs/semantics.md` §2 for
//! the full include semantics. The folder
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
use std::sync::Arc;

use leek_span::paths::{canonical_or_normalized, normalize_lexical};

/// Outcome of `Folder::load`. Carries both the canonical path and
/// the bytes so callers don't double-stat or double-read.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    /// Canonical / normalized path used as the key for the include
    /// graph. Distinct includer-relative spellings of the same file
    /// (`./util`, `util`, `../src/util`) must produce the same
    /// canonical form, otherwise the graph deduplicates wrongly.
    pub path: PathBuf,
    /// The file's contents. Shared rather than owned: the include
    /// walker, the closure it builds and every downstream consumer keep
    /// the same bytes, so passing them on costs a refcount bump instead
    /// of a copy per hop.
    pub text: Arc<str>,
}

/// Errors that may surface during `Folder::load`. Resolver lifts
/// these into proper diagnostics with spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// `name` couldn't be resolved to any file. Reported as
    /// [`INCLUDE_NOT_FOUND`](leek_diagnostics::codes::INCLUDE_NOT_FOUND)
    /// — *not* `AI_NOT_EXISTING`, which is reserved for `import`.
    NotFound,
    /// The file resolved but couldn't be read (permission denied,
    /// not utf-8, …). The string carries the underlying message.
    /// Reported as
    /// [`INCLUDE_UNREADABLE`](leek_diagnostics::codes::INCLUDE_UNREADABLE).
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

/// The paths `include("name")`, written in `includer`, may name — in
/// the order every include resolver in the workspace consults them.
///
/// Upstream's `Folder.resolve` accepts a name with or without its
/// `.leek` extension, so the sibling `<dir>/<name>.leek` comes first
/// and the bare `<dir>/<name>` second. This is the one spelling of
/// that order: [`DiskFolder`] takes the first candidate that is a
/// file, [`MemFolder`] the first that is a known key, and
/// `leek_db::queries::resolve_include` the first the workspace holds a
/// `SourceFile` for. A fourth copy used to live in the LSP's
/// program-scope handler; it calls the query now.
///
/// This list is exhaustive, not a shared prefix: no resolver may try a
/// candidate of its own after these. `MemFolder` did until #521, and
/// the LSP shadows disk with it, so the editor accepted includes that
/// `miku` and `leekc` rejected.
///
/// The results are paths to *open*, not yet map keys — run one through
/// [`canonical_or_normalized`] before keying anything by it.
#[must_use]
pub fn include_candidates(includer: &Path, name: &str) -> [PathBuf; 2] {
    let base = includer
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    [base.join(format!("{name}.leek")), base.join(name)]
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
        // `include("util")` looks for `util.leek` first, then `util`.
        let [with_ext, bare] = include_candidates(includer, name);
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
            text: text.into(),
        })
    }
}

/// In-memory `Folder` for tests and the LSP's open-document layer.
///
/// Keys are virtual paths (e.g. `"file:///proj/main.leek"`), normalized
/// on insert. Names resolve through [`include_candidates`] and only
/// through it — `<dirname-of-includer>/<name>.leek` first, then
/// `<dirname-of-includer>/<name>` — so a name this folder answers is a
/// name [`DiskFolder`] would answer given the same tree, and a name it
/// refuses is one the CLI refuses too.
pub struct MemFolder {
    files: BTreeMap<PathBuf, Arc<str>>,
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
    pub fn insert(&mut self, path: impl Into<PathBuf>, text: impl Into<Arc<str>>) {
        self.files
            .insert(normalize_lexical(&path.into()), text.into());
    }

    /// Build with a single entry shortcut.
    pub fn with_file(path: impl Into<PathBuf>, text: impl Into<Arc<str>>) -> Self {
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
        // [`include_candidates`] and nothing else, in its order. The
        // only thing this folder adds is the normalization the map's
        // keys already went through in [`MemFolder::insert`]; the
        // candidates themselves are the disk folder's.
        //
        // There used to be a third, folder-local candidate here — the
        // raw include name keyed as written, which let
        // `include("shared/util")` reach a fixture inserted as
        // `shared/util.leek` from an includer in another directory.
        // It is gone (#521): this folder backs the LSP's shadowing
        // layer, so a name only it could resolve was a program that
        // compiled in the editor and failed under `miku` and `leekc` —
        // the worst way for the two to disagree.
        for candidate in include_candidates(includer, name) {
            let key = normalize_lexical(&candidate);
            if let Some(text) = self.files.get(&key) {
                return Ok(LoadedFile {
                    path: key,
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
    fn mem_folder_prefers_the_dot_leek_candidate() {
        let mut f = MemFolder::new();
        f.insert("/proj/main.leek", "");
        f.insert("/proj/util.leek", "var from_dot_leek = 1;");
        f.insert("/proj/util", "var from_bare = 1;");
        let got = f.load(Path::new("/proj/main.leek"), "util").unwrap();
        assert_eq!(got.path, PathBuf::from("/proj/util.leek"));
    }

    #[test]
    fn mem_folder_falls_back_to_the_bare_candidate() {
        let mut f = MemFolder::new();
        f.insert("/proj/main.leek", "");
        f.insert("/proj/util", "var from_bare = 1;");
        let got = f.load(Path::new("/proj/main.leek"), "util").unwrap();
        assert_eq!(got.path, PathBuf::from("/proj/util"));
    }

    /// #521. This folder used to try the raw include name, normalized,
    /// as a third candidate, so a file keyed relatively answered an
    /// includer sitting somewhere else entirely. [`DiskFolder`] never
    /// had that lookup and neither does `resolve_include`, and the LSP
    /// shadows disk with this folder — so the editor resolved includes
    /// `miku` and `leekc` reported as not found.
    #[test]
    fn mem_folder_does_not_resolve_the_raw_include_name() {
        // Keyed exactly as the name is written — the spelling the third
        // candidate matched.
        let mut f = MemFolder::new();
        f.insert("/proj/main.leek", "");
        f.insert("shared/util.leek", "function helper() {}");
        assert_eq!(
            f.load(Path::new("/proj/main.leek"), "shared/util.leek")
                .unwrap_err(),
            LoadError::NotFound,
        );

        // And the extensionless spelling of the same thing.
        let mut f = MemFolder::new();
        f.insert("/proj/main.leek", "");
        f.insert("shared/util", "function helper() {}");
        assert_eq!(
            f.load(Path::new("/proj/main.leek"), "shared/util")
                .unwrap_err(),
            LoadError::NotFound,
        );
    }

    /// The other half of the pin: the same file, keyed where the
    /// includer's directory actually puts it, still resolves. Dropping
    /// the third candidate narrowed this folder to the shared two, it
    /// did not break sibling lookup.
    #[test]
    fn mem_folder_still_resolves_that_file_from_the_includers_directory() {
        let mut f = MemFolder::new();
        f.insert("/proj/main.leek", "");
        f.insert("/proj/shared/util.leek", "function helper() {}");
        for name in ["shared/util", "shared/util.leek"] {
            let got = f.load(Path::new("/proj/main.leek"), name).unwrap();
            assert_eq!(got.path, PathBuf::from("/proj/shared/util.leek"));
        }
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
