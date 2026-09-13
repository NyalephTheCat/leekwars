//! Per-server workspace: holds the salsa DB, open-document registry,
//! and optional project-wide file index.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_pipeline::ProjectIndex;
use leek_pipeline::salsa::{LeekDb, ProjectFile, SourceFile};
use leek_span::LineTable;
use leek_span::pragma::{LATEST_VERSION, LanguageSettings};
use salsa::Setter;
use tower_lsp::lsp_types::{SemanticToken, Url};

use crate::documents::DocHandle;

/// A project file indexed on disk but not necessarily open in the
/// editor.
pub struct IndexedFile {
    pub uri: Url,
    pub path: PathBuf,
    /// Salsa input for open-buffer-style pipeline queries.
    pub source_file: SourceFile,
    /// Salsa input keyed by canonical path for project-file queries.
    pub project_file: ProjectFile,
    pub line_table: LineTable,
    pub text: Arc<str>,
}

impl IndexedFile {
    /// UTF-16-aware position map (line table + source text).
    pub fn pos_map(&self) -> crate::util::position::PosMap<'_> {
        crate::util::position::PosMap::new(&self.line_table, &self.text)
    }
}

/// One file the LSP can analyze — either an open buffer or an indexed
/// project file.
pub struct AnalysisTarget<'a> {
    pub uri: &'a Url,
    pub line_table: &'a LineTable,
    /// Source text, paired with `line_table` for UTF-16 position conversion.
    pub text: &'a str,
    pub source_file: SourceFile,
    pub project_file: Option<ProjectFile>,
}

impl AnalysisTarget<'_> {
    /// UTF-16-aware position map (line table + source text).
    pub fn pos_map(&self) -> crate::util::position::PosMap<'_> {
        crate::util::position::PosMap::new(self.line_table, self.text)
    }
}

pub struct Workspace {
    pub db: LeekDb,
    pub docs: HashMap<Url, DocHandle>,
    /// Project-wide index when a `Miku.toml` was discovered.
    pub project: Option<ProjectIndex>,
    /// On-disk `.leek` files from the project index (not open).
    pub indexed: HashMap<PathBuf, IndexedFile>,
    /// Monotonic counter for [`leek_span::SourceId`] allocation.
    /// Starts at 1 because `SourceId::new(0)` is rejected.
    next_source_id: u32,
    /// Project roots received during `initialize`, indexed in
    /// `initialized`.
    pending_project_roots: Vec<PathBuf>,
    /// Library-load log lines collected during `initialize` and flushed
    /// to the client via `window/logMessage` in `initialized` (sending
    /// before the client finishes initializing can drop the message).
    /// `(is_error, message)`.
    pub pending_library_log: Vec<(bool, String)>,
    /// Last semantic-token set returned per document, keyed by the
    /// `result_id` we stamped on it. A `…/full/delta` request names a
    /// `previous_result_id`; we look it up here to diff against. Holds
    /// only the most recent result per URI (one editor only ever deltas
    /// from its latest response).
    semantic_tokens_cache: HashMap<Url, (String, Vec<SemanticToken>)>,
    /// Monotonic source of semantic-token `result_id`s.
    next_result_id: u64,
    /// Editor-pushed configuration (formatter options, inlay-hint
    /// toggle). Mirrors the client's `leek` settings section; updated by
    /// `workspace/didChangeConfiguration` and the initial pull.
    pub settings: crate::settings::Settings,
    /// Per-file `class IDENT` declarations, keyed by URI. The sorted
    /// union feeds every salsa input's `extra_classes` so any file can
    /// use any project class as a type head (`testClass tc = …`),
    /// mirroring upstream's program-wide `getDefinedClass` lookup.
    class_names: HashMap<Url, Vec<String>>,
    /// Last union pushed into the salsa inputs — skip the (re-parse
    /// triggering) input writes when an edit didn't change it.
    class_union: Vec<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        // The LSP wants builtin + leek-wars calls to infer their declared
        // return types (so hover, binary expressions, and member access
        // resolve real types instead of `any`). Seed the typed `.leek`
        // signature headers process-wide — idempotent, set before any
        // tracked query runs, and off for the corpus/driver baseline.
        leek_types::set_seed_library(true);
        Self {
            db: LeekDb::default(),
            docs: HashMap::new(),
            project: None,
            indexed: HashMap::new(),
            next_source_id: 1,
            pending_project_roots: Vec::new(),
            pending_library_log: Vec::new(),
            semantic_tokens_cache: HashMap::new(),
            next_result_id: 1,
            settings: crate::settings::Settings::default(),
            class_names: HashMap::new(),
            class_union: Vec::new(),
        }
    }
}

impl Workspace {
    // Takes `uri` by value: it's stored (cloned) into `self.docs` and used
    // across both branches, and all call sites hand over an owned `Url`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn open(&mut self, uri: Url, text: String) {
        if let Some(path) = uri_to_path(&uri)
            && let Some(indexed) = self.indexed.remove(&path)
        {
            let lang = self.settle(&text);
            self.apply_language(indexed.source_file, Some(indexed.project_file), lang);
            let source = indexed.source_file.source(&self.db);
            let classes = Self::scan_classes(&text, source, lang.version);
            let line_table = LineTable::new(&text);
            let arc_text: Arc<str> = Arc::from(text.as_str());
            indexed.source_file.set_text(&mut self.db).to(text.clone());
            indexed.project_file.set_text(&mut self.db).to(text);
            self.docs.insert(
                uri.clone(),
                DocHandle {
                    source_file: indexed.source_file,
                    line_table,
                    text: arc_text,
                    version: 0,
                },
            );
            self.refresh_classes(&uri, Some(classes));
            return;
        }

        let source_id = self.alloc_source_id();
        let line_table = LineTable::new(&text);
        let arc_text: Arc<str> = Arc::from(text.as_str());
        let lang = self.settle(&text);
        let classes = Self::scan_classes(
            &text,
            leek_span::SourceId::new(source_id).expect("non-zero SourceId"),
            lang.version,
        );
        let source_file = SourceFile::new(
            &self.db,
            source_id,
            text,
            lang.version,
            lang.strict,
            leek_pipeline::FeatureFlags::from_env().to_bits(),
            self.class_union.clone(),
        );
        self.docs.insert(
            uri.clone(),
            DocHandle {
                source_file,
                line_table,
                text: arc_text,
                version: 0,
            },
        );
        self.refresh_classes(&uri, Some(classes));
    }

    /// Record the client's version number for an open document. Echoed
    /// back on `publishDiagnostics` so stale results can be discarded.
    pub fn set_doc_version(&mut self, uri: &Url, version: i32) {
        if let Some(doc) = self.docs.get_mut(uri) {
            doc.version = version;
        }
    }

    /// Replace the document text. Re-builds the `LineTable` and
    /// mutates the salsa input so any tracked queries are
    /// invalidated.
    pub fn update(&mut self, uri: &Url, new_text: String) {
        let Some(doc) = self.docs.get_mut(uri) else {
            return;
        };
        let source_file = doc.source_file;
        doc.line_table = LineTable::new(&new_text);
        doc.text = Arc::from(new_text.as_str());
        // The edit may have added, removed or changed `@version`/`@strict`:
        // re-settle so every tracked pass sees the buffer's current settings.
        let lang = self.settle(&new_text);
        let project_file = uri_to_path(uri)
            .and_then(|path| self.indexed.get(&path))
            .map(|indexed| indexed.project_file);
        self.apply_language(source_file, project_file, lang);
        let source = source_file.source(&self.db);
        let classes = Self::scan_classes(&new_text, source, lang.version);
        source_file.set_text(&mut self.db).to(new_text.clone());
        if let Some(path) = uri_to_path(uri)
            && let Some(indexed) = self.indexed.get_mut(&path)
        {
            indexed.line_table = LineTable::new(&new_text);
            indexed.text = Arc::from(new_text.as_str());
            indexed.project_file.set_text(&mut self.db).to(new_text);
        }
        self.refresh_classes(uri, Some(classes));
    }

    pub fn close(&mut self, uri: &Url) {
        self.docs.remove(uri);
    }

    pub fn doc(&self, uri: &Url) -> Option<&DocHandle> {
        self.docs.get(uri)
    }

    /// Queue a workspace root for indexing during `initialized`.
    pub fn queue_project_root(&mut self, root: PathBuf) {
        self.pending_project_roots.push(root);
    }

    /// Index every root queued during `initialize`.
    pub fn index_pending_projects(&mut self) {
        let roots = std::mem::take(&mut self.pending_project_roots);
        for root in roots {
            self.index_project_at(&root);
        }
    }

    /// Discover `Miku.toml` from `start` and register every `.leek`
    /// file under the project's source tree as salsa-tracked inputs. When
    /// no manifest exists in `start` or its ancestors, treat `start` itself
    /// as the source root (`src = "."`).
    pub fn index_project_at(&mut self, start: &Path) {
        let mut index = if manifest_exists_at_or_above(start) {
            match ProjectIndex::discover(start) {
                Ok(index) => index,
                Err(error) => {
                    eprintln!(
                        "leek-lsp: failed to index project at {}: {error}",
                        start.display()
                    );
                    return;
                }
            }
        } else {
            ProjectIndex::from_src_root(start)
        };
        for path in index.files().to_vec() {
            let Ok(loaded) = index.load_file(&path) else {
                continue;
            };
            if self.indexed.contains_key(&loaded.path) {
                continue;
            }
            let uri = path_to_uri(&loaded.path);
            if self.docs.contains_key(&uri) {
                continue;
            }
            let arc_text: Arc<str> = Arc::from(loaded.text.as_str());
            let flags_bits = leek_pipeline::FeatureFlags::from_env().to_bits();
            let classes = Self::scan_classes(&loaded.text, loaded.source, loaded.version_byte);
            self.class_names.insert(uri.clone(), classes);
            let source_file = SourceFile::new(
                &self.db,
                loaded.source.get(),
                loaded.text.clone(),
                loaded.version_byte,
                loaded.strict,
                flags_bits,
                self.class_union.clone(),
            );
            let project_file = ProjectFile::new(
                &self.db,
                loaded.path.display().to_string(),
                loaded.source.get(),
                loaded.text,
                loaded.version_byte,
                loaded.strict,
                flags_bits,
                self.class_union.clone(),
            );
            self.indexed.insert(
                loaded.path.clone(),
                IndexedFile {
                    uri,
                    path: loaded.path,
                    source_file,
                    project_file,
                    line_table: loaded.line_table,
                    text: arc_text,
                },
            );
        }
        self.project = Some(index);
        // One union rebuild after the whole tree is registered — this
        // pushes every file's classes into every input.
        self.rebuild_class_union();
    }

    /// Every file available for project-wide analysis: open docs plus
    /// indexed project files not currently open.
    pub fn analysis_targets(&self) -> Vec<AnalysisTarget<'_>> {
        let mut out: Vec<AnalysisTarget<'_>> = self
            .docs
            .iter()
            .map(|(uri, doc)| AnalysisTarget {
                uri,
                line_table: &doc.line_table,
                text: &doc.text,
                source_file: doc.source_file,
                project_file: None,
            })
            .collect();
        for indexed in self.indexed.values() {
            if self.docs.contains_key(&indexed.uri) {
                continue;
            }
            out.push(AnalysisTarget {
                uri: &indexed.uri,
                line_table: &indexed.line_table,
                text: &indexed.text,
                source_file: indexed.source_file,
                project_file: Some(indexed.project_file),
            });
        }
        out
    }

    /// Settle one buffer's language settings from its text: the file's
    /// `@version` pragma, else the project's `[project].language` default
    /// (latest outside a project); strict on `@strict` or the project
    /// default.
    fn settle(&self, text: &str) -> LanguageSettings {
        self.project.as_ref().map_or_else(
            || LanguageSettings::resolve(text, None, LATEST_VERSION, false),
            |index| index.language_settings(text),
        )
    }

    /// Push settled language settings into a file's salsa inputs, writing
    /// only the fields that changed so an ordinary edit doesn't needlessly
    /// invalidate on them.
    fn apply_language(
        &mut self,
        source_file: SourceFile,
        project_file: Option<ProjectFile>,
        lang: LanguageSettings,
    ) {
        if source_file.version_byte(&self.db) != lang.version {
            source_file.set_version_byte(&mut self.db).to(lang.version);
        }
        if source_file.strict(&self.db) != lang.strict {
            source_file.set_strict(&mut self.db).to(lang.strict);
        }
        if let Some(project_file) = project_file {
            if project_file.version_byte(&self.db) != lang.version {
                project_file.set_version_byte(&mut self.db).to(lang.version);
            }
            if project_file.strict(&self.db) != lang.strict {
                project_file.set_strict(&mut self.db).to(lang.strict);
            }
        }
    }

    fn alloc_source_id(&mut self) -> u32 {
        let id = self.next_source_id;
        self.next_source_id += 1;
        id
    }

    /// Scan one file's `class IDENT` declarations (version-aware:
    /// `class` only lexes as a keyword from v2 on).
    fn scan_classes(text: &str, source: leek_span::SourceId, version_byte: u8) -> Vec<String> {
        let version = leek_syntax::pipeline::version_from_byte(version_byte);
        let lexed = leek_lexer::lex(text, source, version);
        leek_parser::scan_class_names(text, &lexed.tokens)
    }

    /// Record `uri`'s class declarations and, if the project-wide
    /// union changed, push it into every salsa input's
    /// `extra_classes` (invalidating their parses). Pass `None` to
    /// drop a removed file's contribution.
    fn refresh_classes(&mut self, uri: &Url, names: Option<Vec<String>>) {
        match names {
            Some(n) => {
                self.class_names.insert(uri.clone(), n);
            }
            None => {
                self.class_names.remove(uri);
            }
        }
        self.rebuild_class_union();
    }

    /// Recompute the union of all files' class names and push it into
    /// the salsa inputs when it changed.
    fn rebuild_class_union(&mut self) {
        let mut union: Vec<String> = self
            .class_names
            .values()
            .flat_map(|v| v.iter().cloned())
            .collect();
        union.sort();
        union.dedup();
        if union == self.class_union {
            return;
        }
        self.class_union = union;
        for doc in self.docs.values() {
            doc.source_file
                .set_extra_classes(&mut self.db)
                .to(self.class_union.clone());
        }
        for indexed in self.indexed.values() {
            indexed
                .source_file
                .set_extra_classes(&mut self.db)
                .to(self.class_union.clone());
            indexed
                .project_file
                .set_extra_classes(&mut self.db)
                .to(self.class_union.clone());
        }
    }

    /// Stash the semantic tokens just computed for `uri` under a fresh
    /// `result_id` and return that id. The next `…/full/delta` request
    /// that cites this id can diff against the stored tokens. Only the
    /// latest result per URI is retained.
    pub fn cache_semantic_tokens(&mut self, uri: &Url, tokens: Vec<SemanticToken>) -> String {
        let id = self.next_result_id.to_string();
        self.next_result_id += 1;
        self.semantic_tokens_cache
            .insert(uri.clone(), (id.clone(), tokens));
        id
    }

    /// The cached token set for `uri` if its stored `result_id` matches
    /// `previous_result_id` — the baseline a delta diffs against.
    /// `None` when we never cached it or the id is stale (the client
    /// must then accept a full token set).
    pub fn semantic_tokens_baseline(
        &self,
        uri: &Url,
        previous_result_id: &str,
    ) -> Option<Vec<SemanticToken>> {
        self.semantic_tokens_cache
            .get(uri)
            .filter(|(id, _)| id == previous_result_id)
            .map(|(_, tokens)| tokens.clone())
    }

    /// React to a `workspace/didRenameFiles` move: carry the open
    /// buffer and/or indexed entry from `old` to `new` so subsequent
    /// requests under the new URI resolve. Best-effort — the client
    /// typically also re-opens the moved editor, which reconciles
    /// `docs` regardless.
    pub fn rename_file(&mut self, old: &Url, new: &Url) {
        if let Some(handle) = self.docs.remove(old) {
            self.docs.insert(new.clone(), handle);
        }
        if let Some(classes) = self.class_names.remove(old) {
            self.class_names.insert(new.clone(), classes);
        }
        self.semantic_tokens_cache.remove(old);
        if let (Some(old_path), Some(new_path)) = (uri_to_path(old), uri_to_path(new))
            && let Some(mut indexed) = self.indexed.remove(&old_path)
        {
            indexed.uri = new.clone();
            indexed.path.clone_from(&new_path);
            self.indexed.insert(new_path, indexed);
        }
    }

    /// Reload a file's text from disk into its salsa inputs. Used for a
    /// `workspace/didChangeWatchedFiles` change to a file the editor
    /// does not have open (open buffers are authoritative via
    /// `didChange`, so those are left alone). No-op if the file is open
    /// or not indexed.
    pub fn reload_from_disk(&mut self, uri: &Url) {
        if self.docs.contains_key(uri) {
            return; // open buffer wins
        }
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        let Some(indexed) = self.indexed.get_mut(&path) else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        indexed.line_table = LineTable::new(&text);
        indexed.text = Arc::from(text.as_str());
        let source_file = indexed.source_file;
        let project_file = indexed.project_file;
        let lang = self.settle(&text);
        self.apply_language(source_file, Some(project_file), lang);
        let source = source_file.source(&self.db);
        let classes = Self::scan_classes(&text, source, lang.version);
        source_file.set_text(&mut self.db).to(text.clone());
        project_file.set_text(&mut self.db).to(text);
        self.refresh_classes(uri, Some(classes));
    }

    /// Drop all state for a deleted file.
    pub fn remove_file(&mut self, uri: &Url) {
        self.docs.remove(uri);
        self.semantic_tokens_cache.remove(uri);
        if let Some(path) = uri_to_path(uri) {
            self.indexed.remove(&path);
        }
        self.refresh_classes(uri, None);
    }
}

pub fn path_to_uri(path: &Path) -> Url {
    Url::from_file_path(path)
        .unwrap_or_else(|()| Url::parse(&format!("file://{}", path.display())).expect("file uri"))
}

pub fn uri_to_path(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path().ok()
}

/// Whether a `Miku.toml` exists at `start` or in one of its ancestors.
/// Keep this separate from [`ProjectIndex::discover`] so a malformed
/// manifest is still reported as an error instead of being mistaken for a
/// manifestless workspace.
fn manifest_exists_at_or_above(start: &Path) -> bool {
    let mut cursor = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| start.to_path_buf(), |current| current.join(start))
    };
    loop {
        if cursor.join("Miku.toml").is_file() {
            return true;
        }
        if !cursor.pop() {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "leek-lsp-workspace-fallback-{}-{suffix}",
            std::process::id()
        ))
    }

    #[test]
    fn index_project_defaults_to_workspace_root_without_manifest() {
        let root = temp_root();
        let library = root.join("library");
        fs::create_dir_all(&library).expect("create fallback project");
        fs::write(root.join("main.leek"), "var main = 1\n").expect("write entry");
        fs::write(
            library.join("helper.leek"),
            "function helper() { return 1 }\n",
        )
        .expect("write nested source");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);

        let mut indexed: Vec<PathBuf> = ws.indexed.values().map(|file| file.path.clone()).collect();
        indexed.sort();
        let mut expected = vec![
            root.join("main.leek")
                .canonicalize()
                .expect("entry path should canonicalize"),
            library
                .join("helper.leek")
                .canonicalize()
                .expect("nested path should canonicalize"),
        ];
        expected.sort();

        fs::remove_dir_all(&root).expect("remove fallback project");

        assert_eq!(indexed, expected);
        assert_eq!(ws.analysis_targets().len(), 2);
    }

    fn lang_of(ws: &Workspace, uri: &Url) -> (u8, bool) {
        let file = ws.doc(uri).expect("open doc").source_file;
        (file.version_byte(&ws.db), file.strict(&ws.db))
    }

    #[test]
    fn open_buffer_outside_project_honours_pragmas() {
        let mut ws = Workspace::default();
        let uri = Url::parse("untitled:scratch.leek").expect("uri");
        ws.open(uri.clone(), "// @version:1\n// @strict\nreturn 1\n".into());
        assert_eq!(lang_of(&ws, &uri), (1, true));
    }

    #[test]
    fn editing_pragmas_resettles_buffer_language() {
        let mut ws = Workspace::default();
        let uri = Url::parse("untitled:scratch.leek").expect("uri");
        ws.open(uri.clone(), "// @version:4\nreturn 1\n".into());
        assert_eq!(lang_of(&ws, &uri), (4, false));

        ws.update(&uri, "// @version:1\n// @strict\nreturn 1\n".into());
        assert_eq!(lang_of(&ws, &uri), (1, true));

        ws.update(&uri, "return 1\n".into());
        assert_eq!(lang_of(&ws, &uri), (LATEST_VERSION, false));
    }

    #[test]
    fn editing_pragma_in_indexed_file_updates_project_input() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project");
        let main = root.join("main.leek");
        fs::write(&main, "return 1\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        let path = main.canonicalize().expect("canonical entry");
        let uri = path_to_uri(&path);

        // Not open yet: an on-disk change is picked up by reload.
        fs::write(&main, "// @version:2\nreturn 1\n").expect("rewrite entry");
        ws.reload_from_disk(&uri);
        let project_file = ws.indexed[&path].project_file;
        assert_eq!(project_file.version_byte(&ws.db), 2);

        // Opening with a different pragma re-settles both inputs, and later
        // edits re-settle the open buffer.
        ws.open(uri.clone(), "// @version:1\nreturn 1\n".into());
        assert_eq!(project_file.version_byte(&ws.db), 1);
        ws.update(&uri, "// @version:3\n// @strict\nreturn 1\n".into());
        fs::remove_dir_all(&root).expect("remove project");

        assert_eq!(lang_of(&ws, &uri), (3, true));
    }
}
