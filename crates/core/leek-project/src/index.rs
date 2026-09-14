//! Path → [`SourceId`] registry and on-disk file loading.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use leek_manifest::discover;
use leek_span::pragma::LanguageSettings;
use leek_span::{LineTable, SourceId};

/// Error discovering or indexing a project.
///
/// The manifest half keeps the [`ManifestError`](leek_manifest::ManifestError)
/// whole — with its span — so a caller that has a reporter can still render a
/// caret in `Miku.toml` instead of receiving a flattened sentence.
#[derive(Debug)]
pub enum ProjectError {
    /// The project's `Miku.toml` could not be found, read, or understood.
    Manifest(leek_manifest::ManifestError),
    /// A source file could not be read.
    Io { path: PathBuf, message: String },
    /// The current directory could not be determined, so discovery had
    /// nowhere to start looking for a manifest.
    Cwd { message: String },
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::Manifest(e) => e.fmt(f),
            ProjectError::Io { path, message } => {
                write!(f, "reading {}: {message}", path.display())
            }
            ProjectError::Cwd { message } => {
                write!(f, "determining the current directory: {message}")
            }
        }
    }
}

impl std::error::Error for ProjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProjectError::Manifest(e) => Some(e),
            ProjectError::Io { .. } | ProjectError::Cwd { .. } => None,
        }
    }
}

impl From<leek_manifest::ManifestError> for ProjectError {
    fn from(err: leek_manifest::ManifestError) -> Self {
        ProjectError::Manifest(err)
    }
}

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
        let loaded = discover(start)?;
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
        // Read the caller's own spelling, not the canonical form: the
        // canonical form of a path that does *not* resolve is the
        // lexically normalized one, and `a/link/..` is not `a` when
        // `link` is a symlink (#181). Canonicalization is for the map
        // key and the reported path, never for the `open`.
        let text = std::fs::read_to_string(path).map_err(|e| ProjectError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let canonical = Self::canonicalize(path);
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

    /// The index's file-identity key. Delegates to the workspace-wide
    /// [`leek_span::paths::canonical_or_normalized`] so the index, the
    /// include graph and the LSP agree on when two spellings name the
    /// same file — they used to disagree for any path that does not
    /// exist yet, because only the resolver normalized `..` (#181).
    pub fn canonicalize(path: &Path) -> PathBuf {
        leek_span::paths::canonical_or_normalized(path)
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

    #[test]
    fn source_ids_are_stable_and_path_shape_independent() {
        let dir = scratch("ids");
        let a = dir.join("a.leek");
        let b = dir.join("b.leek");
        std::fs::write(&a, "return 1;\n").expect("write a");
        std::fs::write(&b, "return 2;\n").expect("write b");

        let mut index = v4_index(&dir);
        let id_a = index.source_for_path(&a);
        assert_eq!(
            index.source_for_path(&a),
            id_a,
            "the same path re-uses its id"
        );

        // A path that canonicalizes to the same file must not mint a second
        // id — included files reach the index through both shapes.
        let indirect = dir.join(".").join("a.leek");
        assert_eq!(index.source_for_path(&indirect), id_a);

        let id_b = index.source_for_path(&b);
        assert_ne!(id_a, id_b);
        assert_eq!(
            id_b.get(),
            id_a.get() + 1,
            "ids are handed out in call order"
        );

        assert_eq!(
            index.path_for_source(id_a),
            Some(ProjectIndex::canonicalize(&a).as_path())
        );
        assert_eq!(index.source_for_existing(&b), Some(id_b));
        assert_eq!(index.source_for_existing(&dir.join("never.leek")), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn enumerating_a_tree_lists_each_file_once_in_sorted_order() {
        // Regression: `enumerate_dir` pushed every discovered file itself on
        // top of the push inside `source_for_path`, so `files()` held each
        // path twice.
        let dir = scratch("enumerate");
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("nested/deeper")).expect("dirs");
        for rel in ["main.leek", "nested/one.leek", "nested/deeper/two.leek"] {
            std::fs::write(src.join(rel), "return 1;\n").expect("write");
        }
        // Not Leekscript: must not be indexed.
        std::fs::write(src.join("notes.txt"), "hello").expect("write");

        let index = ProjectIndex::from_src_root(&src);
        let files = index.files();
        assert_eq!(files.len(), 3, "{files:?}");
        let mut sorted = files.to_vec();
        sorted.sort();
        assert_eq!(files, sorted.as_slice(), "files() stays sorted");
        assert!(
            files
                .iter()
                .all(|p| p.extension().is_some_and(|e| e == "leek"))
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn walking_skips_build_and_target_trees() {
        let dir = scratch("walk-outputs");
        std::fs::create_dir_all(dir.join("build/leekscript")).expect("dirs");
        std::fs::create_dir_all(dir.join("target/debug")).expect("dirs");
        std::fs::write(dir.join("kept.leek"), "return 1;\n").expect("write");
        // Emitted output that would otherwise be re-compiled as source.
        std::fs::write(dir.join("build/leekscript/main.leek"), "return 1;\n").expect("write");
        std::fs::write(dir.join("target/debug/main.leek"), "return 1;\n").expect("write");

        let found = walk_leek_files(&dir);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].ends_with("kept.leek"), "{found:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn walking_honors_a_gitignore_and_skips_hidden_files() {
        let dir = scratch("walk-ignore");
        std::fs::write(dir.join(".gitignore"), "ignored.leek\nvendor/\n").expect("write");
        std::fs::write(dir.join("kept.leek"), "return 1;\n").expect("write");
        std::fs::write(dir.join("ignored.leek"), "return 1;\n").expect("write");
        std::fs::write(dir.join(".hidden.leek"), "return 1;\n").expect("write");
        std::fs::create_dir_all(dir.join("vendor")).expect("dirs");
        std::fs::write(dir.join("vendor/dep.leek"), "return 1;\n").expect("write");

        let found = walk_leek_files(&dir);
        let names: Vec<String> = found
            .iter()
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["kept.leek"], "{found:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn walking_a_missing_directory_yields_nothing() {
        let dir = scratch("walk-missing");
        assert!(walk_leek_files(&dir.join("no-such-dir")).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_paths_resolve_against_the_project_root() {
        let dir = scratch("manifest-paths");
        std::fs::create_dir_all(dir.join("sources")).expect("dirs");
        std::fs::write(dir.join("sources/main.leek"), "return 1;\n").expect("write");

        let (manifest, _) = leek_manifest::load_str(
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nlanguage = 3\nstrict = true\n\
             [paths]\nsrc = \"sources\"\ntests = \"spec\"\n",
        )
        .expect("parse manifest");
        let index = ProjectIndex::from_manifest(dir.clone(), &manifest);

        assert_eq!(index.src_root, dir.join("sources"));
        // `spec/` does not exist, so there is no tests root to walk.
        assert_eq!(index.tests_root, None);
        assert_eq!(index.default_version_byte, 3);
        assert!(index.default_strict);
        assert_eq!(index.files().len(), 1, "{:?}", index.files());

        // Create the tests dir and it is picked up on the next load.
        std::fs::create_dir_all(dir.join("spec")).expect("dirs");
        let index = ProjectIndex::from_manifest(dir.clone(), &manifest);
        assert_eq!(index.tests_root, Some(dir.join("spec")));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// #181: an include target that does not exist yet — one being
    /// typed, or an unsaved buffer — used to get two ids here, because
    /// `ProjectIndex::canonicalize` left `..` alone while the
    /// resolver's own helper collapsed it. Both now go through
    /// `leek_span::paths`, so the two spellings are one file.
    #[test]
    fn nonexistent_paths_with_parent_components_share_one_source_id() {
        let dir = scratch("nonexistent-parent");
        let direct = dir.join("Code.leek");
        let indirect = dir.join("sub").join("..").join("Code.leek");
        assert!(!direct.exists(), "the file must not exist for this case");

        let mut index = v4_index(&dir);
        let id = index.source_for_path(&direct);
        assert_eq!(index.source_for_path(&indirect), id);
        assert_eq!(index.files().len(), 1, "{:?}", index.files());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `load_file` must open the caller's spelling and only *key* by
    /// the canonical one: reading the canonical path would follow a
    /// lexically-collapsed `..` somewhere else entirely.
    #[cfg(unix)]
    #[test]
    fn load_file_reads_through_a_linked_directory() {
        let dir = scratch("load-linked");
        let real = dir.join("real");
        std::fs::create_dir_all(&real).expect("create real dir");
        std::fs::write(real.join("main.leek"), "return 42;\n").expect("write");
        let alias = dir.join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("link the directory");

        let mut index = v4_index(&dir);
        let loaded = index.load_file(&alias.join("main.leek")).expect("load");
        assert_eq!(loaded.text, "return 42;\n");
        // Keyed by the real file, so the same file reached both ways
        // is one entry.
        assert_eq!(
            loaded.path,
            ProjectIndex::canonicalize(&real.join("main.leek"))
        );
        let through_real = index.load_file(&real.join("main.leek")).expect("load");
        assert_eq!(through_real.source, loaded.source);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_file_names_the_path_it_could_not_read() {
        let dir = scratch("load-missing");
        let mut index = v4_index(&dir);
        let missing = dir.join("nope.leek");
        let err = index.load_file(&missing).expect_err("missing file");
        // The path is a field, not a substring to fish back out of prose.
        let ProjectError::Io { path, .. } = &err else {
            panic!("expected an IO error, got {err:?}");
        };
        assert!(path.ends_with("nope.leek"), "{}", path.display());
        assert!(
            err.to_string().contains("nope.leek"),
            "the message must name the file: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_file_registers_the_file_and_builds_a_line_table() {
        let dir = scratch("load-registers");
        let path = dir.join("main.leek");
        std::fs::write(&path, "var a = 1;\nreturn a;\n").expect("write");
        let mut index = v4_index(&dir);

        let loaded = index.load_file(&path).expect("load");
        assert_eq!(loaded.path, ProjectIndex::canonicalize(&path));
        assert_eq!(index.source_for_existing(&path), Some(loaded.source));
        assert_eq!(index.files(), [ProjectIndex::canonicalize(&path)]);
        // Offset 11 is the `r` of `return` — the first byte of line 2.
        assert_eq!(loaded.line_table.line_col(11).line, 2);

        std::fs::remove_dir_all(&dir).ok();
    }
}
