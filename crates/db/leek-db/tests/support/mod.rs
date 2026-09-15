//! Shared fixture for the query tests: a workspace of files and a
//! database that says which queries actually ran.
//!
//! The database half is `leek_db::testing`: `EventDb` and `ran` were
//! promoted out of this file into the library behind the `testing`
//! feature, so a consumer outside this crate (the LSP's incremental
//! tests) watches the *same* mechanism instead of a second copy of it.
//! They are re-exported here so the test files that already import them
//! from `support` keep working.

// Each test binary that includes this module uses a different slice of
// it — `diagnostics.rs` never edits a file, `include_queries.rs` never
// asks for `EventDb`'s log — and dead-code analysis runs per binary, so
// every unused-in-this-target item would otherwise be a `-D warnings`
// error in whichever target happens not to want it.
#![allow(dead_code, unused_imports)]

use std::collections::BTreeMap;
use std::sync::Arc;

use leek_db::queries::{IncludeGraph, include_graph};
use leek_db::{SourceFile, WorkspaceFiles};
use leek_syntax::Version;

pub use leek_db::testing::{EventDb, ran};

/// A directory that does not exist, so `canonical_or_normalized` takes
/// its lexical branch for every fixture path — the same branch the
/// folder-backed walk takes for the same paths, which is what lets the
/// two halves of a parity test be compared at all.
pub const ROOT: &str = "/leek-db-query-tests";

/// The fixture path for `name`.
pub fn vpath(name: &str) -> String {
    format!("{ROOT}/{name}")
}

// ---- Fixture ----

pub struct Fixture {
    pub db: EventDb,
    pub files: WorkspaceFiles,
    pub inputs: BTreeMap<String, SourceFile>,
}

impl Fixture {
    /// A workspace holding `files`, each at `<ROOT>/<name>`, numbered
    /// from `SourceId` 1 in the order given.
    pub fn new(files: &[(&str, &str)]) -> Self {
        let mut db = EventDb::new();
        let workspace = WorkspaceFiles::empty(&db);
        let mut inputs = BTreeMap::new();
        let mut map = BTreeMap::new();
        for (index, (name, text)) in files.iter().enumerate() {
            let path = vpath(name);
            let file = SourceFile::new(
                &db,
                path.clone(),
                u32::try_from(index + 1).expect("fixture is small"),
                (*text).into(),
                u8::from(Version::V4),
                false,
                false,
                0,
            );
            inputs.insert(path.clone(), file);
            map.insert(path, file);
        }
        workspace.set_all(&mut db, map);
        Self {
            db,
            files: workspace,
            inputs,
        }
    }

    pub fn file(&self, name: &str) -> SourceFile {
        self.inputs[&vpath(name)]
    }

    /// The file's current text, as the database holds it.
    pub fn text(&self, name: &str) -> Arc<str> {
        Arc::clone(self.file(name).text(&self.db))
    }

    pub fn edit(&mut self, name: &str, text: &str) {
        use salsa::Setter;
        self.inputs[&vpath(name)]
            .set_text(&mut self.db)
            .to(text.into());
    }

    pub fn graph(&self, entry: &str, version: Version) -> IncludeGraph {
        include_graph(&self.db, self.files, self.file(entry), version)
    }
}
