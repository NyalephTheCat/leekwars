//! Optional salsa-backed memoization layer.
//!
//! Enable via the `salsa` feature on `leek-pipeline`. This module
//! provides:
//!
//! - [`Db`] — the database trait pass crates can write tracked
//!   queries against.
//! - [`LeekDb`] — a concrete database. Single-threaded; clone forks
//!   a copy sharing storage.
//! - [`SourceFile`] — the canonical salsa input grouping
//!   `(source_id, text, version, strict)`. Pass crates that want
//!   tracked queries take `(db: &dyn Db, file: SourceFile)` as input
//!   and call `file.text(db)`, `file.version(db)`, etc.
//!
//! The pipeline itself doesn't force memoization on any step. A step
//! that wants caching does:
//!
//! ```ignore
//! impl Step for MyPass {
//!     fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
//!         let out = if let Some((db, file)) = cx.salsa() {
//!             my_tracked_query(db, file)        // memoized
//!         } else {
//!             my_pure_fn(cx.text(), cx.version()) // direct
//!         };
//!         cx.insert(MyArtifact(out));
//!         Ok(())
//!     }
//! }
//! ```
//!
//! The per-crate tracked queries land in each pass crate (lexer,
//! parser, …) when those crates opt in to the salsa feature
//! themselves. See `doc/pipeline.md` for the migration shape.

use leek_span::SourceId;

/// Database trait. Anything that wants to back pipeline steps with
/// salsa caching implements this; pass crates write their tracked
/// queries against `&dyn Db`.
#[salsa::db]
pub trait Db: salsa::Database {}

/// Concrete database. Single-threaded. `Clone` forks a copy sharing
/// the underlying salsa storage so re-using the same memoized
/// results across pipeline runs is just `let db = source_db.clone()`.
#[salsa::db]
#[derive(Default, Clone)]
pub struct LeekDb {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for LeekDb {}

#[salsa::db]
impl Db for LeekDb {}

/// Salsa input — the per-file inputs every tracked query starts
/// from. The `id` field doubles as the [`SourceId`] when reconstructed.
#[salsa::input]
pub struct SourceFile {
    /// `SourceId::get()` value. Stored as `u32` because `SourceId`
    /// itself isn't yet wired through salsa's `Update` trait.
    pub source_id: u32,
    #[returns(ref)]
    pub text: String,
    /// Wire `Version` through as the `u8` byte so we don't need
    /// `salsa::Update` on the enum.
    pub version_byte: u8,
    pub strict: bool,
    /// Experimental [`leek_span::FeatureFlags`] packed as a bitmask (a
    /// primitive, so no `salsa::Update` impl is needed on the flags type).
    pub flags_bits: u8,
    /// Class names declared elsewhere in the program (other files of
    /// the include closure / project). The parser treats these as
    /// valid type heads — `lowercaseClassFromOtherFile x = …` —
    /// mirroring upstream's program-wide `getDefinedClass` lookup.
    /// Keep sorted + deduped so salsa's equality check is stable.
    #[returns(ref)]
    pub extra_classes: Vec<String>,
}

impl SourceFile {
    /// Convenience: extract a [`SourceId`] from the stored `u32`.
    pub fn source(self, db: &dyn Db) -> SourceId {
        SourceId::new(self.source_id(db)).expect("source_id was 0")
    }
}

/// Salsa input for an on-disk project file keyed by canonical path.
///
/// Used for incremental analysis of files that are indexed but not
/// necessarily open in the editor. Tracked queries take this input
/// instead of [`SourceFile`] when the caller is working from the
/// project index rather than an LSP buffer.
#[salsa::input]
pub struct ProjectFile {
    /// Canonical filesystem path (stable cache key).
    #[returns(ref)]
    pub canonical_path: String,
    /// `SourceId::get()` value for this file.
    pub source_id: u32,
    #[returns(ref)]
    pub text: String,
    pub version_byte: u8,
    pub strict: bool,
    /// Experimental [`leek_span::FeatureFlags`] packed as a bitmask.
    pub flags_bits: u8,
    /// Class names declared elsewhere in the project — see
    /// [`SourceFile::extra_classes`].
    #[returns(ref)]
    pub extra_classes: Vec<String>,
}

impl ProjectFile {
    pub fn source(self, db: &dyn Db) -> SourceId {
        SourceId::new(self.source_id(db)).expect("source_id was 0")
    }

    pub fn path(self, db: &dyn Db) -> &str {
        self.canonical_path(db)
    }
}
