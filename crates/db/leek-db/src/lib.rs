//! The query façade: one salsa database, one import path for every
//! tracked query.
//!
//! Today the database lives in [`leek_pipeline::salsa`] and each tracked
//! query lives in the pass crate that computes it, behind that crate's own
//! `salsa` feature. A caller that wants to run the memoized frontend has to
//! know all of that — which crate owns which query, and which feature to
//! turn on. This crate is the single place that knows it instead: it
//! re-exports the database items here at the root and the queries in
//! [`queries`], so a consumer writes `leek_db::{Db, LeekDb, SourceFile}`
//! and `leek_db::queries::*` and nothing else.
//!
//! ## Target shape
//!
//! - **`leek-db` owns the only salsa database.** One `Db` trait, one
//!   `LeekDb`, one set of inputs. Nothing else in the workspace defines a
//!   salsa database, so there is exactly one memo table per query.
//! - **Pass crates export pure functions.** `lex`, `parse`, `resolve`,
//!   `lower_hir`, … take their inputs by value and return their output;
//!   they know nothing about caching. The tracked wrappers around them
//!   move here over the rest of this epic, which is why every re-export
//!   below is a re-export and not a new query — moving a query later must
//!   not have to reconcile two memo tables.
//! - **Tools own their own tracked queries**, written over
//!   [`Db`] and composed from what [`queries`] exposes. That is where
//!   `leek-fmt`'s `format_query` stays: `crates/db` may not depend on
//!   `crates/tools`, so a tool's query belongs in the tool, depending
//!   *down* on this crate.
//!
//! Nothing here changes behaviour. Every item is a re-export of an item
//! that already exists somewhere else.

pub mod queries;

pub use leek_pipeline::salsa::{Db, LeekDb, SourceFile, WorkspaceFiles};

#[cfg(test)]
mod tests {
    use super::{LeekDb, SourceFile, WorkspaceFiles, queries};

    const SRC: &str = "function sum(arr) { var t = 0 for (var x in arr) { t = t + x } return t }\n";

    fn source(db: &LeekDb) -> SourceFile {
        SourceFile::new(
            db,
            "/project/sum.leek".to_string(),
            1,
            SRC.into(),
            4,
            false,
            false,
            0,
            Vec::new(),
        )
    }

    /// The façade is the *only* import a consumer needs: database, input
    /// and every query reachable through `leek_db::` alone. A re-export
    /// that resolved to a type but not a callable function would compile
    /// and then fail at the first caller, so call each one.
    #[test]
    fn every_query_runs_off_one_database() {
        let db = LeekDb::default();
        let file = source(&db);

        assert!(queries::pragma_query(&db, file).diagnostics.is_empty());
        assert!(!queries::lex_query(&db, file).tokens.is_empty());
        assert!(queries::parse_query(&db, file).diagnostics.is_empty());
        let _ = queries::resolve_query(&db, file);
        let _ = queries::typecheck_query(&db, file);
        assert!(!queries::lower_hir_query(&db, file).hir.defs.is_empty());
        let _ = queries::lower_mir_query(&db, file);
        assert!(!queries::complexity_query(&db, file).0.is_empty());
    }

    /// An indexed on-disk file used to need an input and a parse query of
    /// its own. It needs neither: the canonical path rides on the same
    /// `SourceFile` every other query already takes, and
    /// [`WorkspaceFiles`] is what maps that path back to the input.
    #[test]
    fn a_path_keyed_file_is_an_ordinary_source_file() {
        let mut db = LeekDb::default();
        let file = source(&db);
        assert_eq!(file.path(&db), Some("/project/sum.leek"));

        let files = WorkspaceFiles::empty(&db);
        let mut map = std::collections::BTreeMap::new();
        map.insert(file.canonical_path(&db).clone(), file);
        files.set_all(&mut db, map);

        assert!(files.get(&db, "/project/sum.leek") == Some(file));
        assert!(queries::parse_query(&db, file).diagnostics.is_empty());
    }
}
