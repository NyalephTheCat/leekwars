//! Path → [`SourceId`] registry and on-disk file loading.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use leek_manifest::discover;
use leek_span::pragma::LanguageSettings;
use leek_span::{LineTable, SourceId};

/// Error discovering or indexing a project.
#[derive(Debug)]
pub struct ProjectError {
    pub message: String,
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProjectError {}

/// Canonical path → stable [`SourceId`] registry for `.leek` files.
#[derive(Debug, Clone)]
pub struct ProjectIndex {
    pub root: PathBuf,
    pub src_root: PathBuf,
    pub tests_root: Option<PathBuf>,
    pub default_version_byte: u8,
    pub default_strict: bool,
    files: Vec<PathBuf>,
    path_to_source: HashMap<PathBuf, SourceId>,
    source_to_path: HashMap<SourceId, PathBuf>,
    next_source_id: u32,
}

impl ProjectIndex {
    pub fn discover(start: &Path) -> Result<Self, ProjectError> {
        let loaded = discover(start).map_err(|e| ProjectError { message: e.message })?;
        Ok(Self::from_manifest(loaded.root, &loaded.manifest))
    }

    pub fn from_manifest(root: PathBuf, manifest: &leek_manifest::Manifest) -> Self {
        let src_root = root.join(&manifest.paths.src);
        let tests_root = {
            let t = root.join(&manifest.paths.tests);
            t.is_dir().then_some(t)
        };
        let mut index = Self {
            root,
            src_root: src_root.clone(),
            tests_root,
            default_version_byte: manifest.project.language,
            default_strict: manifest.project.strict,
            files: Vec::new(),
            path_to_source: HashMap::new(),
            source_to_path: HashMap::new(),
            next_source_id: 1,
        };
        index.enumerate_dir(&src_root);
        index
    }

    pub fn from_src_root(src_root: &Path) -> Self {
        let root = src_root
            .parent()
            .map_or_else(|| src_root.to_path_buf(), Path::to_path_buf);
        let mut index = Self {
            root,
            src_root: src_root.to_path_buf(),
            tests_root: None,
            default_version_byte: 4,
            default_strict: false,
            files: Vec::new(),
            path_to_source: HashMap::new(),
            source_to_path: HashMap::new(),
            next_source_id: 1,
        };
        index.enumerate_dir(src_root);
        index
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn source_for_path(&mut self, path: &Path) -> SourceId {
        let key = Self::canonicalize(path);
        if let Some(&id) = self.path_to_source.get(&key) {
            return id;
        }
        let id = SourceId::new(self.next_source_id).expect("non-zero SourceId");
        self.next_source_id += 1;
        self.path_to_source.insert(key.clone(), id);
        if !self.files.contains(&key) {
            self.files.push(key.clone());
            self.files.sort();
        }
        self.source_to_path.insert(id, key);
        id
    }

    pub fn path_for_source(&self, source: SourceId) -> Option<&Path> {
        self.source_to_path.get(&source).map(PathBuf::as_path)
    }

    pub fn source_for_existing(&self, path: &Path) -> Option<SourceId> {
        self.path_to_source.get(&Self::canonicalize(path)).copied()
    }

    pub fn load_file(&mut self, path: &Path) -> Result<LoadedProjectFile, ProjectError> {
        let canonical = Self::canonicalize(path);
        let text = std::fs::read_to_string(&canonical).map_err(|e| ProjectError {
            message: format!("reading {}: {e}", canonical.display()),
        })?;
        let source = self.source_for_path(&canonical);
        let lang = self.language_settings(&text);
        let line_table = LineTable::new(&text);
        Ok(LoadedProjectFile {
            path: canonical,
            source,
            text,
            version_byte: lang.version,
            strict: lang.strict,
            line_table,
        })
    }

    /// Settle a file's language version and strict mode: its own
    /// `// @version:N` / `// @strict` pragmas, falling back to the manifest's
    /// `[project].language` / `strict` defaults. This is the one place project
    /// inputs get their version; every pass then reads `Input::version_byte`.
    pub fn language_settings(&self, text: &str) -> LanguageSettings {
        LanguageSettings::resolve(text, None, self.default_version_byte, self.default_strict)
    }

    pub fn canonicalize(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    pub fn walk_leek_under(&self, dir: &Path) -> Vec<PathBuf> {
        walk_leek_files(dir)
    }

    fn enumerate_dir(&mut self, dir: &Path) {
        self.files.clear();
        for path in walk_leek_files(dir) {
            let canonical = Self::canonicalize(&path);
            // `source_for_path` already inserts into `self.files` (dedup'd);
            // a second push here duplicated every discovered file.
            let _ = self.source_for_path(&canonical);
        }
        self.files.sort();
    }
}

#[derive(Debug, Clone)]
pub struct LoadedProjectFile {
    pub path: PathBuf,
    pub source: SourceId,
    pub text: String,
    pub version_byte: u8,
    pub strict: bool,
    pub line_table: LineTable,
}

/// Recursively collect `*.leek` under `dir`, honoring ignore rules.
pub fn walk_leek_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    let mut builder = ignore::WalkBuilder::new(dir);
    builder
        .standard_filters(true)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false)
        .parents(true)
        .require_git(false);
    let mut overrides = ignore::overrides::OverrideBuilder::new(dir);
    let _ = overrides.add("!build/");
    let _ = overrides.add("!target/");
    if let Ok(ov) = overrides.build() {
        builder.overrides(ov);
    }
    for entry in builder.build().flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "leek") {
            out.push(path.to_path_buf());
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "leek-project-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// An index whose manifest defaults are `language = 4`, `strict = false`.
    fn v4_index(src: &Path) -> ProjectIndex {
        let mut index = ProjectIndex::from_src_root(src);
        index.default_version_byte = 4;
        index.default_strict = false;
        index
    }

    #[test]
    fn file_pragma_overrides_manifest_language() {
        // Regression: the old pre-scan stripped `// @version` and tried to
        // parse `:N` as a number, so it never matched the real syntax and
        // every file got the manifest default.
        let dir = scratch("pragma");
        let index = v4_index(&dir);
        for (text, want) in [
            ("// @version:1\nreturn 1;\n", 1),
            ("//@version:2\nreturn 1;\n", 2),
            (" // @version:3\nreturn 1;\n", 3),
            ("return 1;\n", 4),
        ] {
            assert_eq!(index.language_settings(text).version, want, "{text:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_file_uses_pragma_version_and_strict() {
        let dir = scratch("load");
        let path = dir.join("main.leek");
        std::fs::write(&path, "// @version:1\n// @strict\nreturn 1;\n").expect("write");
        let mut index = v4_index(&dir);
        let loaded = index.load_file(&path).expect("load");
        assert_eq!(loaded.version_byte, 1);
        assert!(loaded.strict);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_strict_default_applies_without_pragma() {
        let dir = scratch("strict");
        let mut index = v4_index(&dir);
        index.default_strict = true;
        let lang = index.language_settings("return 1;\n");
        assert!(lang.strict);
        assert_eq!(lang.version, 4);
        std::fs::remove_dir_all(&dir).ok();
    }
}
