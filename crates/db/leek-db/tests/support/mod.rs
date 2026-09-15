//! Shared fixture for the query tests: a workspace of files and a
//! database that says which queries actually ran.
//!
//! Re-running is observed through salsa's own event stream
//! ([`EventDb`]) rather than a counter inside one crate's query, so a
//! test can say "that leaf's lex re-ran and the entry's did not" about
//! queries owned by three different crates — and so a query can be
//! moved between crates without rewriting the tests that watch it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use leek_db::queries::{IncludeGraph, include_graph};
use leek_db::{Db, SourceFile, WorkspaceFiles};
use leek_syntax::Version;

/// A directory that does not exist, so `canonical_or_normalized` takes
/// its lexical branch for every fixture path — the same branch the
/// folder-backed walk takes for the same paths, which is what lets the
/// two halves of a parity test be compared at all.
pub const ROOT: &str = "/leek-db-query-tests";

/// The fixture path for `name`.
pub fn vpath(name: &str) -> String {
    format!("{ROOT}/{name}")
}

// ---- A database that records which queries actually executed ----

/// A [`leek_db::Db`] that logs every `WillExecute` event.
///
/// Salsa fires that event when a query body is about to run — i.e. on
/// a miss or an invalidation, never on a hit — so the log is exactly
/// the set of queries a revision made the engine recompute.
#[salsa::db]
#[derive(Clone)]
pub struct EventDb {
    storage: salsa::Storage<Self>,
    executed: Arc<Mutex<Vec<String>>>,
}

#[salsa::db]
impl salsa::Database for EventDb {}

#[salsa::db]
impl Db for EventDb {}

impl EventDb {
    pub fn new() -> Self {
        let executed: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&executed);
        Self {
            storage: salsa::Storage::new(Some(Box::new(move |event: salsa::Event| {
                if let salsa::EventKind::WillExecute { database_key } = event.kind {
                    sink.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("{database_key:?}"));
                }
            }))),
            executed,
        }
    }

    /// Everything that executed since the last call, emptying the log.
    pub fn drain(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .executed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// How many times `query` executed in the recorded event log.
///
/// Salsa renders a tracked function's database key as `name(Id(n))`,
/// so the query's name is the prefix of the event.
pub fn ran(events: &[String], query: &str) -> usize {
    events.iter().filter(|e| e.starts_with(query)).count()
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
