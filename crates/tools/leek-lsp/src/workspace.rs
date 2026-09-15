//! Per-server workspace: holds the salsa DB, open-document registry,
//! and one project index per workspace root.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_pipeline::ProjectIndex;
use leek_pipeline::salsa::{LeekDb, SourceFile, WorkspaceFiles};
use leek_resolver::folder::Folder;
use leek_resolver::interner::{PathInterner, SourceInterner};
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
    /// The file's salsa input — the *same* input an open buffer over
    /// this file uses. An indexed file used to carry a second, path-keyed
    /// input beside this one; carrying the path on the input itself means
    /// there is one text to keep current instead of two.
    pub source_file: SourceFile,
    /// Shared so an [`AnalysisTarget`] can hold it for the price of a
    /// refcount bump.
    pub line_table: Arc<LineTable>,
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
///
/// Every field is either `Copy` or an `Arc`, so the whole list is built
/// once per workspace mutation (see [`Workspace::resync`]) and handed
/// out by reference afterwards. It used to be rebuilt per call, which a
/// single `publishDiagnostics` did three times.
pub struct AnalysisTarget {
    pub uri: Url,
    pub line_table: Arc<LineTable>,
    /// Source text, paired with `line_table` for UTF-16 position conversion.
    pub text: Arc<str>,
    pub source_file: SourceFile,
    /// The file's canonical path as its salsa input carries it — empty
    /// for a buffer that has no path (`untitled:`).
    pub canonical_path: String,
}

impl AnalysisTarget {
    /// UTF-16-aware position map (line table + source text).
    pub fn pos_map(&self) -> crate::util::position::PosMap<'_> {
        crate::util::position::PosMap::new(&self.line_table, &self.text)
    }
}

pub struct Workspace {
    pub db: LeekDb,
    pub docs: HashMap<Url, DocHandle>,
    /// One index per workspace root we were asked to analyze. A file's
    /// language defaults come from the root that owns it (see
    /// [`Workspace::owning_root`]), so several roots — or a nested
    /// project inside an outer one — each keep their own manifest.
    roots: Vec<ProjectIndex>,
    /// On-disk `.leek` files from the project index (not open).
    pub indexed: HashMap<PathBuf, IndexedFile>,
    /// The salsa input naming every file this workspace holds, keyed by
    /// the canonical path each [`SourceFile`] carries. Rewritten by
    /// [`Workspace::resync`] from `docs` + `indexed`, so a tracked query
    /// that wants to resolve a path to a file reads one input instead of
    /// reaching into the server's private maps.
    files: WorkspaceFiles,
    /// Cached [`Workspace::analysis_targets`] output, rebuilt by
    /// [`Workspace::resync`].
    targets: Vec<AnalysisTarget>,
    /// Cached include folder over those targets, rebuilt alongside them:
    /// open buffers shadow indexed contents, disk is the fallback. One
    /// per workspace revision rather than one per pipeline run, which for
    /// a project of N files made a full diagnostics pass build N copies
    /// of the same map.
    folder: Arc<dyn Folder>,
    /// The server's one source of [`leek_span::SourceId`]s: the ids
    /// this workspace mints for its salsa inputs, and the ids the
    /// include walker gives files that are neither open nor indexed,
    /// come out of the same counter, so the two can never name two
    /// different files the same (#191). Shared with
    /// [`crate::pipeline`], whose include walker interns into it.
    pub interner: Arc<PathInterner>,
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
        // signature headers — off for the corpus/driver baseline. Set
        // here, before any salsa input exists, because each input copies
        // the current value into its own `seed_library` field: the
        // tracked queries read the input, never this global.
        leek_types::set_seed_library(true);
        let db = LeekDb::default();
        let files = WorkspaceFiles::empty(&db);
        Self {
            db,
            docs: HashMap::new(),
            roots: Vec::new(),
            indexed: HashMap::new(),
            files,
            targets: Vec::new(),
            folder: crate::pipeline::include_folder(&[]),
            interner: Arc::new(PathInterner::new()),
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
        // The buffer's text becomes the shared handle first, and every
        // read below goes through it: the salsa input, the doc handle and
        // the analysis target all hold this one allocation.
        let arc_text: Arc<str> = Arc::from(text);
        let path = uri_to_path(&uri);
        // An indexed file keeps its entry (and salsa input) while open:
        // `resync` already skips indexed files with an open buffer, and
        // keeping the entry lets `close` hand the file back to the project
        // index instead of losing it.
        if let Some(path) = path.as_deref()
            && let Some(source_file) = self.indexed.get(path).map(|indexed| indexed.source_file)
        {
            let lang = self.settle(Some(path), &arc_text);
            self.apply_language(source_file, lang);
            let source = source_file.source(&self.db);
            let classes = Self::scan_classes(&arc_text, source, lang.version);
            let line_table = Arc::new(LineTable::new(&arc_text));
            source_file.set_text(&mut self.db).to(Arc::clone(&arc_text));
            if let Some(indexed) = self.indexed.get_mut(path) {
                indexed.line_table = Arc::clone(&line_table);
                indexed.text = Arc::clone(&arc_text);
            }
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
            self.resync();
            return;
        }

        let source_id = self.source_id_for(path.as_deref());
        let line_table = Arc::new(LineTable::new(&arc_text));
        let lang = self.settle(path.as_deref(), &arc_text);
        let classes = Self::scan_classes(
            &arc_text,
            leek_span::SourceId::new(source_id).expect("non-zero SourceId"),
            lang.version,
        );
        let source_file = SourceFile::new(
            &self.db,
            canonical_path_of(path.as_deref()),
            source_id,
            Arc::clone(&arc_text),
            lang.version,
            lang.strict,
            leek_types::seed_library_enabled(),
            leek_span::FeatureFlags::from_env().to_bits(),
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
        self.resync();
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
        let arc_text: Arc<str> = Arc::from(new_text);
        let line_table = Arc::new(LineTable::new(&arc_text));
        doc.line_table = Arc::clone(&line_table);
        doc.text = Arc::clone(&arc_text);
        // The edit may have added, removed or changed `@version`/`@strict`:
        // re-settle so every tracked pass sees the buffer's current settings.
        let path = uri_to_path(uri);
        let lang = self.settle(path.as_deref(), &arc_text);
        self.apply_language(source_file, lang);
        let source = source_file.source(&self.db);
        let classes = Self::scan_classes(&arc_text, source, lang.version);
        source_file.set_text(&mut self.db).to(Arc::clone(&arc_text));
        if let Some(path) = path
            && let Some(indexed) = self.indexed.get_mut(&path)
        {
            indexed.line_table = line_table;
            indexed.text = arc_text;
        }
        self.refresh_classes(uri, Some(classes));
        self.resync();
    }

    /// Drop every piece of per-URI state the closed buffer owns: the open
    /// handle and its semantic-token delta baseline (a full token vector
    /// that would otherwise live for the rest of the session). A file the
    /// project still indexes goes back to its on-disk text, so its classes
    /// stay in the union and the project keeps analyzing it; a buffer with
    /// no indexed file behind it contributes nothing once closed, so its
    /// class names go away instead of lingering.
    pub fn close(&mut self, uri: &Url) {
        self.docs.remove(uri);
        self.semantic_tokens_cache.remove(uri);
        if !self.reload_indexed_from_disk(uri) {
            self.refresh_classes(uri, None);
        }
        self.resync();
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
                    // The whole project stays unindexed, so cross-file
                    // resolution and diagnostics quietly go missing. The user
                    // needs to see this one.
                    tracing::warn!(
                        root = %start.display(),
                        %error,
                        "failed to index project"
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
            self.register_indexed(uri, loaded);
        }
        // Prefix matching in `owning_root` compares against a file's
        // canonical path, so the bases have to be canonical too — a
        // workspace folder arrives as whatever the client sent.
        index.root = ProjectIndex::canonicalize(&index.root);
        index.src_root = ProjectIndex::canonicalize(&index.src_root);
        match self.roots.iter().position(|root| root.root == index.root) {
            Some(at) => self.roots[at] = index,
            None => self.roots.push(index),
        }
        // One union rebuild after the whole tree is registered — this
        // pushes every file's classes into every input.
        self.rebuild_class_union();
        self.resync();
    }

    /// Fold a file that just appeared on disk into the root that owns
    /// it. Only that one file is loaded: re-indexing its parent
    /// directory would build a second, subdirectory-rooted
    /// [`ProjectIndex`] whose `language`/`strict` defaults are the
    /// manifestless fallbacks, and every later edit anywhere under it
    /// would settle against those instead of the real manifest.
    ///
    /// No-op for a file outside every indexed root, one we already
    /// index, or one the editor has open (its buffer is authoritative
    /// and already carries a salsa input).
    pub fn register_new_file(&mut self, uri: &Url) {
        let Some(path) = uri_to_path(uri) else {
            return;
        };
        let path = ProjectIndex::canonicalize(&path);
        if self.indexed.contains_key(&path) || self.docs.contains_key(uri) {
            return;
        }
        let Some(at) = self.owning_root(&path) else {
            return;
        };
        let Ok(loaded) = self.roots[at].load_file(&path) else {
            return;
        };
        if self.indexed.contains_key(&loaded.path) {
            return;
        }
        let uri = path_to_uri(&loaded.path);
        self.register_indexed(uri, loaded);
        self.rebuild_class_union();
        self.resync();
    }

    /// Register one loaded project file as a salsa-tracked input.
    ///
    /// The [`leek_span::SourceId`] comes from the workspace interner, not
    /// from the owning [`ProjectIndex`] — each index numbers its own
    /// files from 1, so two roots (or a root and an open buffer) would
    /// otherwise hand the same id to different files, and
    /// [`crate::pipeline::run_on_file_with_includes`] maps a source back
    /// to a URI by exactly that id. The index's own numbering is never
    /// read from here, so letting the two diverge costs nothing.
    ///
    /// Leaves the class union and the derived caches alone: callers
    /// registering a whole tree rebuild both once at the end.
    fn register_indexed(&mut self, uri: Url, loaded: leek_pipeline::LoadedProjectFile) {
        let arc_text: Arc<str> = Arc::from(loaded.text);
        let flags_bits = leek_span::FeatureFlags::from_env().to_bits();
        let source_id = self.source_id_for(Some(&loaded.path));
        let source = leek_span::SourceId::new(source_id).expect("non-zero SourceId");
        let classes = Self::scan_classes(&arc_text, source, loaded.version_byte);
        self.class_names.insert(uri.clone(), classes);
        let source_file = SourceFile::new(
            &self.db,
            loaded.path.display().to_string(),
            source_id,
            arc_text.clone(),
            loaded.version_byte,
            loaded.strict,
            leek_types::seed_library_enabled(),
            flags_bits,
            self.class_union.clone(),
        );
        self.indexed.insert(
            loaded.path.clone(),
            IndexedFile {
                uri,
                path: loaded.path,
                source_file,
                line_table: Arc::new(loaded.line_table),
                text: arc_text,
            },
        );
    }

    /// Every file available for project-wide analysis: open docs plus
    /// indexed project files not currently open.
    ///
    /// Already built — [`Workspace::resync`] rebuilds it at each mutation
    /// point — so a handler may call this as often as it likes.
    pub fn analysis_targets(&self) -> &[AnalysisTarget] {
        &self.targets
    }

    /// The include folder for this workspace revision: open buffers
    /// shadow indexed contents, and disk is the fallback for a file that
    /// is neither. Built alongside [`Workspace::analysis_targets`].
    pub fn include_folder(&self) -> Arc<dyn Folder> {
        Arc::clone(&self.folder)
    }

    /// The salsa input mapping canonical path → [`SourceFile`] for every
    /// file this workspace holds.
    pub fn files(&self) -> WorkspaceFiles {
        self.files
    }

    /// The file registered at `path`, if the workspace holds one.
    pub fn source_file_at(&self, path: &Path) -> Option<SourceFile> {
        self.files.get(&self.db, &path.display().to_string())
    }

    /// Rebuild everything derived from the current set of files: the
    /// analysis-target list, the [`WorkspaceFiles`] input, and the
    /// include folder over them.
    ///
    /// Every mutation point ends here. Deriving these once per mutation
    /// rather than once per use is what keeps a `publishDiagnostics` —
    /// which reaches the target list from three places and used to
    /// rebuild it each time — from paying for the workspace three times
    /// over.
    fn resync(&mut self) {
        let mut targets: Vec<AnalysisTarget> =
            Vec::with_capacity(self.docs.len() + self.indexed.len());
        for (uri, doc) in &self.docs {
            targets.push(AnalysisTarget {
                uri: uri.clone(),
                line_table: Arc::clone(&doc.line_table),
                text: Arc::clone(&doc.text),
                source_file: doc.source_file,
                canonical_path: doc.source_file.canonical_path(&self.db).clone(),
            });
        }
        // An indexed file that also has an open buffer is already in
        // `targets`. Match on the salsa `SourceFile` rather than the URI:
        // a client that reached the file through another spelling of
        // its path — a workspace root behind a symlink (#181) — sends
        // a URI that is not `indexed.uri`, but `open` has already
        // handed that buffer the indexed file's `SourceFile`, so this
        // is the identity that actually settles "same file".
        let open: Vec<_> = self.docs.values().map(|doc| doc.source_file).collect();
        for indexed in self.indexed.values() {
            if self.docs.contains_key(&indexed.uri) || open.contains(&indexed.source_file) {
                continue;
            }
            targets.push(AnalysisTarget {
                uri: indexed.uri.clone(),
                line_table: Arc::clone(&indexed.line_table),
                text: Arc::clone(&indexed.text),
                source_file: indexed.source_file,
                canonical_path: indexed.source_file.canonical_path(&self.db).clone(),
            });
        }

        let files: BTreeMap<String, SourceFile> = targets
            .iter()
            .filter(|target| !target.canonical_path.is_empty())
            .map(|target| (target.canonical_path.clone(), target.source_file))
            .collect();
        self.files.set_all(&mut self.db, files);
        self.folder = crate::pipeline::include_folder(&targets);
        self.targets = targets;
    }

    /// Settle one buffer's language settings from its text: the file's
    /// `@version` pragma, else the `[project].language` default of the
    /// root that owns `path` (latest outside every root); strict on
    /// `@strict` or that root's default.
    fn settle(&self, path: Option<&Path>, text: &str) -> LanguageSettings {
        path.and_then(|path| self.owning_root(path)).map_or_else(
            || LanguageSettings::resolve(text, None, LATEST_VERSION, false),
            |root| self.roots[root].language_settings(text),
        )
    }

    /// Index of the project root that owns `path`: the one whose source
    /// tree — or, failing that, whose project directory — is the longest
    /// path prefix of it. `None` for a file outside every indexed root,
    /// including an untitled buffer.
    ///
    /// Longest prefix rather than first match, so a nested project
    /// inside an outer workspace folder wins for its own files.
    fn owning_root(&self, path: &Path) -> Option<usize> {
        self.roots
            .iter()
            .enumerate()
            .filter_map(|(at, index)| {
                let depth = [&index.src_root, &index.root]
                    .into_iter()
                    .filter(|base| path.starts_with(base))
                    .map(|base| base.components().count())
                    .max()?;
                Some((depth, at))
            })
            .max_by_key(|&(depth, _)| depth)
            .map(|(_, at)| at)
    }

    /// Push settled language settings into a file's salsa input, writing
    /// only the fields that changed so an ordinary edit doesn't needlessly
    /// invalidate on them.
    fn apply_language(&mut self, source_file: SourceFile, lang: LanguageSettings) {
        if source_file.version_byte(&self.db) != lang.version {
            source_file.set_version_byte(&mut self.db).to(lang.version);
        }
        if source_file.strict(&self.db) != lang.strict {
            source_file.set_strict(&mut self.db).to(lang.strict);
        }
    }

    /// A [`leek_span::SourceId`] for a new salsa input.
    ///
    /// Interned by canonical path (#464), so an id belongs to the *file*
    /// rather than to the moment its input happened to be created: close
    /// and reopen a buffer, or delete and re-index a file on disk, and
    /// every span already cached against it still names the same source.
    /// Minting a fresh id each time invalidated every downstream memo
    /// that had recorded one.
    ///
    /// A buffer with no path at all (`untitled:`) has nothing to key on
    /// and gets a reserved id instead, from the same counter, so it
    /// cannot collide with an interned one.
    fn source_id_for(&self, path: Option<&Path>) -> u32 {
        match path {
            Some(path) => self.interner.intern(path).get(),
            None => self.interner.reserve().get(),
        }
    }

    /// Scan one file's `class IDENT` declarations (version-aware:
    /// `class` only lexes as a keyword from v2 on).
    fn scan_classes(text: &str, source: leek_span::SourceId, version_byte: u8) -> Vec<String> {
        let version = leek_syntax::version::version_from_byte(version_byte);
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
        if let (Some(old_path), Some(new_path)) = (uri_to_path(old), uri_to_path(new)) {
            if let Some(mut indexed) = self.indexed.remove(&old_path) {
                indexed.uri = new.clone();
                indexed.path.clone_from(&new_path);
                self.indexed.insert(new_path.clone(), indexed);
            }
            // At most one input can sit at a path, so this moves the
            // file's input whether it was open, indexed or both. Leaving
            // the old path on it would point every path-keyed lookup at a
            // file that is no longer there.
            if let Some(file) = self.files.get(&self.db, &old_path.display().to_string()) {
                file.set_canonical_path(&mut self.db)
                    .to(new_path.display().to_string());
                // The file keeps its `SourceId` across the move, so the
                // interner has to learn the new spelling — and forget the
                // old one, or a file later created there would be handed
                // the id that left with this one.
                let source = file.source(&self.db);
                self.interner.forget(&old_path);
                self.interner.assign(&new_path, source);
            }
        }
        self.resync();
    }

    /// Reload a file's text from disk into its salsa input. Used for a
    /// `workspace/didChangeWatchedFiles` change to a file the editor
    /// does not have open (open buffers are authoritative via
    /// `didChange`, so those are left alone). No-op if the file is open
    /// or not indexed.
    pub fn reload_from_disk(&mut self, uri: &Url) {
        if self.docs.contains_key(uri) {
            return; // open buffer wins
        }
        if self.reload_indexed_from_disk(uri) {
            self.resync();
        }
    }

    /// Re-read `uri`'s indexed entry from disk and refresh its classes.
    /// Returns `false` — leaving the workspace untouched — when `uri` is
    /// not an indexed file or its text can't be read, so the caller can
    /// fall back to dropping the file's state.
    ///
    /// Leaves the derived caches to the caller: `close` resyncs once for
    /// the whole close, reload or not.
    fn reload_indexed_from_disk(&mut self, uri: &Url) -> bool {
        let Some(path) = uri_to_path(uri) else {
            return false;
        };
        let Some(indexed) = self.indexed.get_mut(&path) else {
            return false;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return false;
        };
        let arc_text: Arc<str> = Arc::from(text);
        indexed.line_table = Arc::new(LineTable::new(&arc_text));
        indexed.text = Arc::clone(&arc_text);
        let source_file = indexed.source_file;
        let lang = self.settle(Some(&path), &arc_text);
        self.apply_language(source_file, lang);
        let source = source_file.source(&self.db);
        let classes = Self::scan_classes(&arc_text, source, lang.version);
        source_file.set_text(&mut self.db).to(arc_text);
        self.refresh_classes(uri, Some(classes));
        true
    }

    /// React to a file disappearing from disk: drop the project's copy
    /// of it, but keep an open buffer alive.
    ///
    /// The editor keeps the tab (and its unsaved text) open when a
    /// checkout or an external tool deletes the file underneath it, and
    /// every later `didChange` bails out at `doc(&uri)` once the handle
    /// is gone — the tab would stay dead until reopened. This is the
    /// mirror of the invariant `close` keeps from the other side:
    /// neither event may evict the state the other one owns.
    pub fn remove_from_disk(&mut self, uri: &Url) {
        if let Some(path) = uri_to_path(uri) {
            self.indexed.remove(&path);
        }
        let open = self
            .docs
            .get(uri)
            .map(|doc| (Arc::clone(&doc.text), doc.source_file));
        // Still open: the buffer is now the file's only text, so its
        // classes stay in the union — rescanned from the buffer, since
        // the indexed entry that held the disk copy is gone. Not open:
        // the file contributes nothing any more.
        if let Some((text, source_file)) = open {
            let classes = Self::scan_classes(
                &text,
                source_file.source(&self.db),
                source_file.version_byte(&self.db),
            );
            self.refresh_classes(uri, Some(classes));
        } else {
            self.semantic_tokens_cache.remove(uri);
            self.refresh_classes(uri, None);
        }
        self.resync();
    }
}

/// The canonical-path string a [`SourceFile`] carries: the path itself,
/// or empty for a source that has none.
fn canonical_path_of(path: Option<&Path>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_default()
}

pub fn path_to_uri(path: &Path) -> Url {
    Url::from_file_path(path)
        .unwrap_or_else(|()| Url::parse(&format!("file://{}", path.display())).expect("file uri"))
}

/// The path a `file:` URI names, in the same shape every path-keyed
/// map in the workspace is built with.
///
/// Canonicalizing here — rather than at each of the six `self.indexed`
/// lookups — is what makes those lookups hit: `indexed` is keyed by
/// `ProjectIndex::canonicalize` output, so a client that reaches the
/// workspace through a symlink (`file:///var/folders/…` on macOS,
/// where `/var` links to `/private/var`) would otherwise miss every
/// entry, mint a second `SourceFile` on `open`, silently no-op on
/// `reload_from_disk`, and fail to evict on `remove_file` (#181).
///
/// Paths handed *back* to the client must still come from the stored
/// `IndexedFile::uri`, never from re-deriving a URI out of this: the
/// canonical spelling is ours, not the client's.
pub fn uri_to_path(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path()
        .ok()
        .map(|path| leek_span::paths::canonical_or_normalized(&path))
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        // The timestamp alone is not unique. Tests run on parallel threads and
        // `SystemTime::now()` is coarser on macOS than on Linux, so two calls
        // in the same tick produced the SAME directory — whichever test
        // finished first then `remove_dir_all`'d the other's project out from
        // under it. The counter makes the name unique regardless of clock
        // resolution; the timestamp stays so leftovers are still sortable.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let seq = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "leek-lsp-workspace-fallback-{}-{suffix}-{seq}",
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
        let indexed_file = ws.indexed[&path].source_file;
        assert_eq!(indexed_file.version_byte(&ws.db), 2);

        // Opening re-settles the one input the buffer shares with the
        // indexed file, and later edits re-settle it again.
        ws.open(uri.clone(), "// @version:1\nreturn 1\n".into());
        assert_eq!(indexed_file.version_byte(&ws.db), 1);
        ws.update(&uri, "// @version:3\n// @strict\nreturn 1\n".into());
        fs::remove_dir_all(&root).expect("remove project");

        assert_eq!(lang_of(&ws, &uri), (3, true));
    }

    #[test]
    fn closing_a_scratch_buffer_drops_all_its_state() {
        let mut ws = Workspace::default();
        let uri = Url::parse("untitled:scratch.leek").expect("uri");
        ws.open(uri.clone(), "class Scratch {}\n".into());
        let result_id = ws.cache_semantic_tokens(&uri, Vec::new());
        assert!(ws.semantic_tokens_baseline(&uri, &result_id).is_some());
        assert_eq!(ws.class_union, vec!["Scratch".to_string()]);

        ws.close(&uri);

        assert!(ws.docs.is_empty());
        assert!(ws.semantic_tokens_cache.is_empty());
        assert!(ws.semantic_tokens_baseline(&uri, &result_id).is_none());
        assert!(ws.class_names.is_empty());
        assert!(ws.class_union.is_empty());
    }

    #[test]
    fn closing_an_indexed_file_restores_its_disk_classes() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project");
        let main = root.join("main.leek");
        fs::write(&main, "class OnDisk {}\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        let path = main.canonicalize().expect("canonical entry");
        let uri = path_to_uri(&path);

        // An unsaved buffer replaces the file's classes while it is open.
        ws.open(uri.clone(), "class InBuffer {}\n".into());
        let result_id = ws.cache_semantic_tokens(&uri, Vec::new());
        assert_eq!(ws.class_union, vec!["InBuffer".to_string()]);

        ws.close(&uri);
        let union = ws.class_union.clone();
        let targets = ws.analysis_targets().len();
        let baseline = ws.semantic_tokens_baseline(&uri, &result_id);
        fs::remove_dir_all(&root).expect("remove project");

        // The buffer's state is gone, but the file is still a project file.
        assert!(baseline.is_none());
        assert_eq!(union, vec!["OnDisk".to_string()]);
        assert_eq!(targets, 1);
    }

    /// Mirror of `closing_an_indexed_file_restores_its_disk_classes`
    /// from the other side: the disk copy goes, the live buffer stays.
    #[test]
    fn disk_delete_keeps_an_open_buffer_editable() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project");
        let main = root.join("main.leek");
        fs::write(&main, "class OnDisk {}\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        let path = main.canonicalize().expect("canonical entry");
        let uri = path_to_uri(&path);
        ws.open(uri.clone(), "class InBuffer {}\n".into());

        fs::remove_file(&main).expect("delete entry");
        ws.remove_from_disk(&uri);
        // The tab is still live, so a later edit must still land.
        ws.update(&uri, "// @version:2\nclass Edited {}\n".into());

        let union = ws.class_union.clone();
        let still_open = ws.doc(&uri).is_some();
        let indexed = ws.indexed.len();
        let version = ws.doc(&uri).map(|doc| doc.source_file.version_byte(&ws.db));
        fs::remove_dir_all(&root).expect("remove project");

        assert!(still_open, "the open buffer must survive a disk delete");
        assert_eq!(indexed, 0, "the project's copy is gone");
        assert_eq!(union, vec!["Edited".to_string()]);
        assert_eq!(version, Some(2), "the edit re-settled the buffer");
    }

    #[test]
    fn disk_delete_of_a_closed_file_drops_it() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project");
        let main = root.join("main.leek");
        fs::write(&main, "class OnDisk {}\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        let path = main.canonicalize().expect("canonical entry");
        let uri = path_to_uri(&path);
        assert_eq!(ws.class_union, vec!["OnDisk".to_string()]);

        fs::remove_file(&main).expect("delete entry");
        ws.remove_from_disk(&uri);

        let union = ws.class_union.clone();
        let indexed = ws.indexed.len();
        let targets = ws.analysis_targets().len();
        fs::remove_dir_all(&root).expect("remove project");

        assert_eq!(indexed, 0);
        assert_eq!(targets, 0);
        assert!(
            union.is_empty(),
            "a file that is gone contributes no classes"
        );
    }

    fn write_manifest(root: &Path, name: &str, language: u8) {
        let text = format!(
            "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\nlanguage = {language}\n\n[paths]\nsrc = \".\"\n"
        );
        fs::write(root.join("Miku.toml"), text).expect("write manifest");
    }

    /// Every salsa `SourceId` currently reachable for analysis.
    fn source_ids(ws: &Workspace) -> Vec<u32> {
        ws.analysis_targets()
            .iter()
            .map(|target| target.source_file.source(&ws.db).get())
            .collect()
    }

    fn assert_unique(ids: &[u32]) {
        let mut sorted = ids.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            before,
            "duplicate SourceIds break the source → URI lookup in `pipeline`: {ids:?}"
        );
    }

    #[test]
    fn a_created_file_joins_the_owning_root() {
        let root = temp_root();
        let library = root.join("library");
        fs::create_dir_all(&library).expect("create project");
        write_manifest(&root, "owning-root", 2);
        fs::write(root.join("main.leek"), "return 1\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);

        let created = library.join("helper.leek");
        fs::write(&created, "function helper() { return 1 }\n").expect("write new file");
        let path = created.canonicalize().expect("canonical new file");
        ws.register_new_file(&path_to_uri(&path));

        let version = ws
            .indexed
            .get(&path)
            .map(|file| file.source_file.version_byte(&ws.db));
        let ids = source_ids(&ws);
        fs::remove_dir_all(&root).expect("remove project");

        // The owning manifest's `language = 2` applies — not the
        // manifestless fallback a subdirectory-rooted index would give.
        assert_eq!(version, Some(2));
        assert_unique(&ids);
    }

    #[test]
    fn a_created_file_in_a_subdirectory_gets_a_fresh_source_id() {
        let root = temp_root();
        let library = root.join("library");
        fs::create_dir_all(&library).expect("create project");
        fs::write(root.join("main.leek"), "return 1\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);

        let created = library.join("helper.leek");
        fs::write(&created, "function helper() { return 1 }\n").expect("write new file");
        let path = created.canonicalize().expect("canonical new file");
        ws.register_new_file(&path_to_uri(&path));

        let indexed = ws.indexed.len();
        let ids = source_ids(&ws);
        fs::remove_dir_all(&root).expect("remove project");

        assert_eq!(indexed, 2);
        assert_unique(&ids);
    }

    #[test]
    fn source_ids_are_unique_across_indexed_and_open_files() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project");
        fs::write(root.join("main.leek"), "return 1\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        ws.open(
            Url::parse("untitled:scratch.leek").expect("uri"),
            "return 2\n".into(),
        );

        let ids = source_ids(&ws);
        fs::remove_dir_all(&root).expect("remove project");

        assert_eq!(ids.len(), 2);
        assert_unique(&ids);
    }

    #[test]
    fn two_roots_each_keep_their_own_language_defaults() {
        let first = temp_root();
        let second = first.with_extension("second");
        fs::create_dir_all(&first).expect("create first project");
        fs::create_dir_all(&second).expect("create second project");
        write_manifest(&first, "first", 2);
        write_manifest(&second, "second", 3);
        fs::write(first.join("main.leek"), "return 1\n").expect("write first entry");
        fs::write(second.join("main.leek"), "return 2\n").expect("write second entry");

        let mut ws = Workspace::default();
        ws.queue_project_root(first.clone());
        ws.queue_project_root(second.clone());
        ws.index_pending_projects();

        let first_uri = path_to_uri(&first.join("main.leek").canonicalize().expect("canonical"));
        let second_uri = path_to_uri(&second.join("main.leek").canonicalize().expect("canonical"));
        // No pragmas, so each buffer settles purely on its owning
        // manifest's default.
        ws.open(first_uri.clone(), "return 1\n".into());
        ws.open(second_uri.clone(), "return 2\n".into());
        let first_lang = lang_of(&ws, &first_uri);
        let second_lang = lang_of(&ws, &second_uri);
        let ids = source_ids(&ws);
        fs::remove_dir_all(&first).expect("remove first project");
        fs::remove_dir_all(&second).expect("remove second project");

        assert_eq!(first_lang, (2, false));
        assert_eq!(second_lang, (3, false));
        assert_unique(&ids);
    }

    #[test]
    fn rename_updates_the_canonical_path() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project");
        let old = root.join("main.leek");
        fs::write(&old, "return 1\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        let old_path = old.canonicalize().expect("canonical entry");
        let new_path = root.join("renamed.leek");
        fs::rename(&old_path, &new_path).expect("rename entry");
        ws.rename_file(&path_to_uri(&old_path), &path_to_uri(&new_path));

        // `indexed` is keyed by the canonical path (#181), so look the entry
        // up the same way. On macOS the temp root is reached through a symlink
        // (`/var` -> `/private/var`), so the raw join and the canonical
        // spelling differ and only the canonical one is a key.
        let new_path = new_path.canonicalize().expect("canonical renamed entry");
        let stored = ws
            .indexed
            .get(&new_path)
            .map(|file| file.source_file.canonical_path(&ws.db).clone());
        let at_new = ws.source_file_at(&new_path);
        let at_old = ws.source_file_at(&old_path);
        fs::remove_dir_all(&root).expect("remove project");

        assert_eq!(stored, Some(new_path.display().to_string()));
        assert!(
            at_new.is_some(),
            "the renamed file must be reachable at its new path"
        );
        assert!(
            at_old.is_none(),
            "nothing may still answer at the path the file left"
        );
    }

    /// The path → input map is the workspace's own file set, no more and
    /// no less: a pathless buffer has no key, and a file that is both
    /// open and indexed is one entry because it is one input.
    #[test]
    fn the_workspace_files_input_tracks_the_file_set() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create project");
        let main = root.join("main.leek");
        fs::write(&main, "return 1\n").expect("write entry");

        let mut ws = Workspace::default();
        ws.index_project_at(&root);
        let path = main.canonicalize().expect("canonical entry");
        let uri = path_to_uri(&path);

        let indexed_only = ws.files().len(&ws.db);
        let indexed_file = ws.source_file_at(&path);

        // A scratch buffer has no path, so it adds no entry.
        ws.open(
            Url::parse("untitled:scratch.leek").expect("uri"),
            "return 2\n".into(),
        );
        let with_scratch = ws.files().len(&ws.db);

        // Opening the indexed file reuses its input, so still one entry.
        ws.open(uri.clone(), "return 3\n".into());
        let with_buffer = ws.files().len(&ws.db);
        let opened_file = ws.source_file_at(&path);

        ws.remove_from_disk(&uri);
        ws.close(&uri);
        let after_removal = ws.files().len(&ws.db);
        fs::remove_dir_all(&root).expect("remove project");

        assert_eq!(indexed_only, 1);
        assert!(
            indexed_file.is_some(),
            "the indexed file is a workspace file"
        );
        assert_eq!(with_scratch, 1, "a pathless buffer has no canonical path");
        assert_eq!(with_buffer, 1, "one file, one input, one entry");
        assert!(
            opened_file == indexed_file,
            "the buffer must reuse the indexed file's input"
        );
        assert_eq!(
            after_removal, 0,
            "a file that is gone is not a workspace file"
        );
    }

    /// #464: a `SourceId` belongs to a path, not to the moment an input
    /// was created. Reopening a closed buffer used to mint a fresh id,
    /// which invalidated every span and memo downstream had cached
    /// against the old one.
    #[test]
    fn reopening_a_file_keeps_its_source_id() {
        let root = temp_root();
        fs::create_dir_all(&root).expect("create scratch dir");
        let file = root.join("loose.leek");
        fs::write(&file, "var x = 1\n").expect("write file");
        let path = file.canonicalize().expect("canonical file");
        let uri = path_to_uri(&path);

        let mut ws = Workspace::default();
        ws.open(uri.clone(), "var x = 1\n".into());
        let first = ws.doc(&uri).expect("open doc").source_file.source(&ws.db);
        ws.close(&uri);
        ws.open(uri.clone(), "var x = 2\n".into());
        let second = ws.doc(&uri).expect("open doc").source_file.source(&ws.db);
        fs::remove_dir_all(&root).expect("remove scratch dir");

        assert_eq!(first, second, "the id belongs to the path, not the buffer");
    }

    /// #181: `indexed` is keyed by `ProjectIndex::canonicalize` output,
    /// but every lookup used the URI's raw path. A client that reached
    /// the workspace through a linked directory — every macOS client,
    /// since `/var` links to `/private/var` — therefore missed the key
    /// and `open` minted a second `SourceFile` for a file the index
    /// already owned. `uri_to_path` now canonicalizes, so the lookup
    /// hits whichever spelling the client used.
    #[cfg(unix)]
    #[test]
    fn opening_through_a_linked_root_reuses_the_indexed_file() {
        let root = temp_root();
        let real = root.join("real");
        fs::create_dir_all(&real).expect("create project");
        fs::write(real.join("main.leek"), "class OnDisk {}\n").expect("write entry");
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&real, &alias).expect("link the project dir");

        let mut ws = Workspace::default();
        ws.index_project_at(&real);
        let indexed_source = ws
            .indexed
            .values()
            .next()
            .expect("one indexed file")
            .source_file;

        // The client only ever knows the aliased spelling.
        let uri = path_to_uri(&alias.join("main.leek"));
        ws.open(uri.clone(), "class InBuffer {}\n".into());
        let opened_source = ws.doc(&uri).expect("open doc").source_file;
        let indexed_len = ws.indexed.len();
        let targets = ws.analysis_targets().len();
        fs::remove_dir_all(&root).expect("remove project");

        assert!(
            opened_source == indexed_source,
            "the open buffer must reuse the indexed file's SourceFile",
        );
        assert_eq!(indexed_len, 1, "no second entry for the same file");
        assert_eq!(targets, 1, "the file must not be analysed twice");
    }
}
