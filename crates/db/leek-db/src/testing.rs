//! Test-only scaffolding: a database that says which queries ran.
//!
//! Behind the `testing` feature (and always on for this crate's own
//! `cfg(test)` builds), so nothing here ships in a release binary.
//!
//! Every incremental claim in this epic — "editing a leaf re-parses
//! only that leaf", "a second hover runs zero queries" — is a claim
//! about *what executed*, and without something that can observe that,
//! every one of them is unfalsifiable. [`EventDb`] is that observer.
//!
//! It watches salsa's own event stream rather than a counter inside one
//! query body, which matters for two reasons. A counter can only see
//! the crate it lives in, and these tests need to say "that leaf's lex
//! re-ran and the entry's parse did not" about queries owned by three
//! different crates. And a counter moves when its query moves, so the
//! assertions would have to be rewritten every time this epic relocates
//! a query — which it does, repeatedly.
//!
//! This started as `EventDb` in `tests/support/mod.rs` (R1-17) and was
//! promoted here rather than copied, so there is still exactly one
//! mechanism: that support module now re-exports these items, and a
//! consumer outside this crate (the LSP's incremental tests) reaches
//! them with `leek-db = { features = ["testing"] }`.

use std::sync::{Arc, Mutex};

use crate::Db;

/// A [`Db`] that logs every query execution.
///
/// Salsa fires `WillExecute` when a query body is about to run — on a
/// miss or an invalidation, never on a hit — so the log is exactly the
/// set of queries a revision made the engine recompute.
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
    #[must_use]
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

impl Default for EventDb {
    fn default() -> Self {
        Self::new()
    }
}

/// How many times `query` executed in the recorded event log.
///
/// Salsa renders a tracked function's database key as `name(Id(n))`, so
/// the query's name is the prefix of the event.
#[must_use]
pub fn ran(events: &[String], query: &str) -> usize {
    events.iter().filter(|e| e.starts_with(query)).count()
}

/// A workspace of in-memory files, for a test that lowers a small
/// multi-file project.
///
/// `files` is `(path, text)`; `entry_path` must name one of them. The
/// entry takes `SourceId(1)` and the rest follow in the order given,
/// which is the numbering a `leek_session::Session` hands out and what
/// the `ResolveIncludes` step this replaced did.
///
/// Each file's language settings are settled from its own `@version` /
/// `@strict` pragma over `lang`, the `(version, strict)` defaults, which
/// is what
/// `leek_project::ProjectIndex::language_settings` does for a real
/// project — so a pragma-less include inherits the default and a pragma'd
/// one keeps its own. `seed_library` is off.
///
/// Returns the file set and the entry, ready for `lower_program` and the
/// other whole-program queries.
pub fn workspace(
    db: &mut crate::LeekDb,
    entry_path: &str,
    files: &[(&str, &str)],
    lang: (u8, bool),
    flags_bits: u8,
) -> (crate::WorkspaceFiles, crate::SourceFile) {
    use std::collections::BTreeMap;

    let key = |path: &str| {
        leek_span::paths::canonical_or_normalized(std::path::Path::new(path))
            .display()
            .to_string()
    };
    let entry_key = key(entry_path);

    let mut next = 2u32;
    let mut map: BTreeMap<String, crate::SourceFile> = BTreeMap::new();
    for (path, text) in files {
        let path = key(path);
        let id = if path == entry_key {
            1
        } else {
            let id = next;
            next += 1;
            id
        };
        let settled = leek_span::pragma::LanguageSettings::resolve(text, None, lang.0, lang.1);
        let file = crate::SourceFile::new(
            db,
            path.clone(),
            id,
            std::sync::Arc::from(*text),
            settled.version,
            settled.strict,
            false,
            flags_bits,
        );
        map.insert(path, file);
    }

    let entry = *map.get(&entry_key).expect("entry is one of the files");
    let set = crate::WorkspaceFiles::empty(db);
    set.set_all(db, map);
    (set, entry)
}
