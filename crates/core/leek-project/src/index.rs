//! Path → [`SourceId`] registry and on-disk file loading.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use leek_manifest::discover;
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
        let version_byte = peek_version_byte(&text, self.default_version_byte);
        let strict = peek_strict_flag(&text) || self.default_strict;
        let line_table = LineTable::new(&text);
        Ok(LoadedProjectFile {
            path: canonical,
            source,
            text,
            version_byte,
            strict,
            line_table,
        })
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

pub(crate) fn peek_version_byte(text: &str, default: u8) -> u8 {
    for line in text.lines().take(32) {
        let trimmed = line.trim();
        let rest = trimmed
            .strip_prefix("// @version")
            .or_else(|| trimmed.strip_prefix("@version"))
            .or_else(|| trimmed.strip_prefix("//@version"));
        let Some(rest) = rest else {
            continue;
        };
        if let Ok(n) = rest.trim().parse::<u8>()
            && (1..=4).contains(&n)
        {
            return n;
        }
    }
    default
}

pub(crate) fn peek_strict_flag(text: &str) -> bool {
    text.lines().take(32).any(|line| {
        let t = line.trim();
        t == "// @strict" || t == "@strict" || t == "//@strict"
    })
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
