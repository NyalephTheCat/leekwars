//! The salsa-backed memoization layer. This module provides:
//!
//! - [`Db`] — the database trait pass crates can write tracked
//!   queries against.
//! - [`LeekDb`] — a concrete database. Single-threaded; clone forks
//!   a copy sharing storage.
//! - [`SourceFile`] — the canonical salsa input grouping
//!   `(canonical_path, source_id, text, version, strict, seed_library,
//!   flags)`. Pass crates that want tracked queries take `(db: &dyn Db,
//!   file: SourceFile)` as input and call `file.text(db)`,
//!   `file.version(db)`, etc.
//! - [`WorkspaceFiles`] — the one input that says which files a
//!   workspace currently holds, keyed by the same canonical path
//!   [`SourceFile::canonical_path`] carries.
//! - [`ProgramClasses`] — the interned program-wide class-name set a
//!   parse is keyed on, so one file can parse differently in two
//!   programs without either answer evicting the other.
//!
//! A pass crate writes one thin tracked wrapper over its own pure
//! function:
//!
//! ```ignore
//! #[salsa::tracked]
//! pub fn my_query(db: &dyn Db, file: SourceFile) -> MyResult {
//!     my_pure_fn(file.text(db), file.version_byte(db))
//! }
//! ```
//!
//! Those queries land in each pass crate (lexer, parser, …); `leek-db`
//! re-exports them all under one import path.

use std::collections::BTreeMap;
use std::sync::Arc;

use leek_span::SourceId;

/// Database trait. Anything that wants to memoize the compiler's passes
/// implements this; pass crates write their tracked
/// queries against `&dyn Db`.
#[salsa::db]
pub trait Db: salsa::Database {}

/// Concrete database. Single-threaded. `Clone` forks a copy sharing
/// the underlying salsa storage so re-using the same memoized
/// results across compilations is just `let db = source_db.clone()`.
#[salsa::db]
#[derive(Default, Clone)]
pub struct LeekDb {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for LeekDb {}

#[salsa::db]
impl Db for LeekDb {}

/// Salsa input — the per-file inputs every tracked query starts from,
/// whether the file is an editor buffer or an on-disk project file.
///
/// There is deliberately only one of these. An indexed file used to get
/// a second input of its own keyed by path; carrying the path here
/// instead means a file that is both indexed and open is *one* input,
/// so an edit cannot leave the two halves disagreeing about its text.
#[salsa::input]
pub struct SourceFile {
    /// Canonical filesystem path — the stable identity of the file
    /// across revisions and the key [`WorkspaceFiles`] maps.
    ///
    /// Empty for a source with no path at all (an `untitled:` editor
    /// buffer, a string compiled straight from a test), which is why it
    /// is a `String` rather than a `PathBuf`: a pathless buffer has no
    /// meaningful path, not a path that happens to be `""` on disk.
    #[returns(ref)]
    pub canonical_path: String,
    /// `SourceId::get()` value. Stored as `u32` because `SourceId`
    /// itself isn't yet wired through salsa's `Update` trait.
    pub source_id: u32,
    /// The file's text, shared rather than owned: a reader takes a
    /// refcount bump instead of copying the whole buffer, so compiling
    /// N indexed files does not copy N file texts.
    #[returns(ref)]
    pub text: Arc<str>,
    /// Wire `Version` through as the `u8` byte so we don't need
    /// `salsa::Update` on the enum.
    pub version_byte: u8,
    pub strict: bool,
    /// Whether the type checker should seed the typed standard-library
    /// signature headers (`stdlib.leek` / `leekwars.leek`) for this
    /// file. An input rather than a process-global so a tracked query
    /// that depends on it is invalidated when it changes — the LSP
    /// turns it on, the driver/corpus baseline leaves it off.
    pub seed_library: bool,
    /// Experimental [`leek_span::FeatureFlags`] packed as a bitmask (a
    /// primitive, so no `salsa::Update` impl is needed on the flags type).
    pub flags_bits: u8,
}

impl SourceFile {
    /// Convenience: extract a [`SourceId`] from the stored `u32`.
    pub fn source(self, db: &dyn Db) -> SourceId {
        SourceId::new(self.source_id(db)).expect("source_id was 0")
    }

    /// The canonical path, or `None` for a source that has none.
    pub fn path(self, db: &dyn Db) -> Option<&str> {
        let path = self.canonical_path(db);
        (!path.is_empty()).then_some(path.as_str())
    }
}

/// The program-wide `class IDENT` names a parse must treat as type
/// heads, interned so that it can be a tracked query *argument*.
///
/// Upstream resolves a potential type word against the whole program's
/// defined-class set, so `lowercaseClassFromInclude x = …` is a typed
/// declaration in every file of a closure that declares it. That set is
/// a property of the *program*, not of the file: the same leaf parses
/// differently under two entries that include it alongside different
/// siblings.
///
/// It is therefore a key, not a field on [`SourceFile`]. As an input
/// field it forced one answer per file — which is what made the LSP
/// maintain a workspace-wide union and re-parse every open document
/// whenever anyone typed a class name (#163). As a key, two programs'
/// parses of one leaf are two memo entries, and an edit that leaves the
/// program's class set alone re-parses only the file that changed.
///
/// Interned rather than passed as a loose `Vec<String>` because a
/// tracked query's arguments have to be `Copy`, and because interning
/// gives the set one identity: two callers that assemble the same names
/// hit the same memo.
#[salsa::interned]
pub struct ProgramClasses<'db> {
    /// The class names, sorted and deduplicated by whoever built them
    /// (the interned identity is the `Vec`, so an unsorted duplicate
    /// spelling would be a second key for the same set).
    #[returns(ref)]
    pub names: Vec<String>,
}

impl<'db> ProgramClasses<'db> {
    /// The empty set — what a file parsed on its own, outside any
    /// include closure, knows about other files' classes.
    pub fn none(db: &'db dyn Db) -> Self {
        Self::new(db, Vec::new())
    }
}

/// Salsa input: every file a workspace currently holds, keyed by the
/// canonical path each one carries in [`SourceFile::canonical_path`].
///
/// One input rather than one per file, because the interesting question
/// — "which file is at this path?" — has to re-run when *any* file
/// appears or disappears, not only when a particular one changes. A
/// query that resolves an `include("…")` against the workspace reads
/// this map; a query that only wants one known file's text reads that
/// [`SourceFile`] directly and is untouched when an unrelated file is
/// opened.
///
/// Pathless buffers are absent: they have no key, and nothing can
/// include them.
#[salsa::input]
pub struct WorkspaceFiles {
    #[returns(ref)]
    pub files: BTreeMap<String, SourceFile>,
}

impl WorkspaceFiles {
    /// A workspace holding no files yet.
    pub fn empty(db: &dyn Db) -> Self {
        Self::new(db, BTreeMap::new())
    }

    /// The file registered at `path`, if any.
    pub fn get(self, db: &dyn Db, path: &str) -> Option<SourceFile> {
        self.files(db).get(path).copied()
    }

    /// How many files the workspace holds.
    pub fn len(self, db: &dyn Db) -> usize {
        self.files(db).len()
    }

    /// Whether the workspace holds no files at all.
    pub fn is_empty(self, db: &dyn Db) -> bool {
        self.files(db).is_empty()
    }

    /// Replace the whole mapping.
    ///
    /// The maintainer rebuilds the map from the state it already keeps
    /// and writes it here. Salsa inputs do not compare — a write bumps
    /// the revision whether or not the value changed — so the comparison
    /// happens here instead, and a rebuild that changed nothing leaves
    /// every query that read this map untouched.
    pub fn set_all(self, db: &mut dyn Db, files: BTreeMap<String, SourceFile>) {
        use salsa::Setter;
        if *self.files(db) != files {
            self.set_files(db).to(files);
        }
    }
}
