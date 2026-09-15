//! Complexity analysis as a tracked query.
//!
//! Exposes the analysis as a memoized query over the lowered HIR so a
//! tool asks for it as often as it likes — the LSP does, from hover, code
//! lens and `leek.showComplexity` — instead of calling
//! [`analyze_file`](crate::analyze_file) on every request.

use std::sync::Arc;

use crate::Complexity;
use crate::analyze::analyze_file;

/// Tracked return type — newtype over `Arc<Vec<Complexity>>` so the
/// salsa query has a single `Update`-able return.
#[derive(salsa::Update, Debug, Clone, PartialEq)]
pub struct ComplexityReport(pub Arc<Vec<Complexity>>);

/// Salsa-tracked entry point. Re-runs only when
/// [`lower_hir_query`](leek_hir::query::lower_hir_query)'s HIR
/// changes.
///
/// Answers for **one file**. A driver that wants a row per function the
/// *program* defines — every `miku analyze` over a project with includes
/// — asks `leek_db::queries::program_complexity`, which measures the
/// closure's merged HIR.
#[salsa::tracked]
pub fn complexity_query(
    db: &dyn leek_query::salsa::Db,
    file: leek_query::salsa::SourceFile,
) -> ComplexityReport {
    #[cfg(test)]
    salsa_probe::COMPLEXITY_QUERY_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hir = leek_hir::query::lower_hir_query(db, file);
    ComplexityReport(Arc::new(analyze_file(hir.hir.as_ref())))
}

#[cfg(test)]
mod salsa_probe {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    pub(super) static COMPLEXITY_QUERY_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub(super) static SERIAL: Mutex<()> = Mutex::new(());
}

#[cfg(test)]
mod salsa_tests {
    use std::sync::atomic::Ordering;

    use leek_query::salsa::{LeekDb, SourceFile};
    use salsa::Setter;

    use super::{complexity_query, salsa_probe::COMPLEXITY_QUERY_CALLS, salsa_probe::SERIAL};

    fn source(db: &mut LeekDb, text: &str) -> SourceFile {
        SourceFile::new(db, String::new(), 1, text.into(), 4, false, false, 0)
    }

    #[test]
    fn the_cascade_caches_complexity() {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut db = LeekDb::default();
        let file = source(
            &mut db,
            "function sum(arr) { var t = 0 for (var x in arr) { t = t + x } return t }\n",
        );
        let before = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = complexity_query(&db, file);
        let after_first = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = complexity_query(&db, file);
        let after_second = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);

        assert_eq!(
            after_first - before,
            1,
            "the first call executes the analysis once"
        );
        assert_eq!(
            after_second - after_first,
            0,
            "a second identical call must hit the salsa cache — this is what \
             lets the LSP ask for the report on every hover and code lens"
        );
    }

    #[test]
    fn semantic_edit_reruns_complexity() {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut db = LeekDb::default();
        let file = source(&mut db, "function f() { return 1 }\n");
        let before = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = complexity_query(&db, file);
        let after_first = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);

        file.set_text(&mut db)
            .to("function f(n) { for (var i = 0; i < n; i++) { } return 1 }\n".into());

        let _ = complexity_query(&db, file);
        let after_second = COMPLEXITY_QUERY_CALLS.load(Ordering::Relaxed);

        assert_eq!(after_first - before, 1);
        assert_eq!(
            after_second - after_first,
            1,
            "a semantic change must re-execute the analysis"
        );
    }
}

#[cfg(test)]
mod tests {
    use leek_query::salsa::{LeekDb, SourceFile};

    use super::complexity_query;
    use super::salsa_probe::SERIAL;

    #[test]
    fn the_query_measures_the_lowered_hir() {
        // The counting tests above watch a process-global counter this
        // call also bumps, so it takes the same lock they do.
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let db = LeekDb::default();
        let file = SourceFile::new(
            &db,
            String::new(),
            1,
            "function sum(arr) {\n  var t = 0\n  for (var x in arr) { t = t + x }\n  return t\n}\n"
                .into(),
            4,
            false,
            false,
            0,
        );
        let report = complexity_query(&db, file).0;
        let sum = report
            .iter()
            .find(|c| c.name == "sum")
            .expect("sum analysed");
        assert!(
            matches!(sum.big_o, crate::BigO::Linear(ref v) if v.name == "arr"),
            "got {:?}",
            sum.big_o
        );
    }
}
