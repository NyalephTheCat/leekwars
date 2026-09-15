//! HIR → MIR lowering as a tracked query.

use std::sync::Arc;

use leek_query::OptLevel;

use crate::MirProgram;
use crate::lower::lower_and_optimize;

/// Tracked return: MIR program plus lowering diagnostics.
#[derive(salsa::Update, Debug, Clone, PartialEq)]
pub struct LowerMirQueryResult {
    pub program: Arc<MirProgram>,
    pub diagnostics: Vec<leek_diagnostics::Diagnostic>,
}

/// Tracked return: `Arc<MirProgram>` newtype, salsa-friendly.
#[derive(salsa::Update, Debug, Clone, PartialEq)]
pub struct LoweredMir(pub Arc<MirProgram>);

/// Salsa-tracked entry point. Re-runs only when
/// [`lower_hir_query`](leek_hir::query::lower_hir_query)'s HIR
/// changes.
///
/// Answers for **one file** at [`OptLevel::O0`]: the cached program is
/// the one analysis drivers read. A codegen driver asks
/// `leek_db::queries::lower_program_mir`, which is keyed on the opt level
/// and lowers the whole closure's merged HIR — lowering from the memoized
/// HIR rather than cloning this program and optimizing the copy.
#[salsa::tracked]
pub fn lower_mir_query(
    db: &dyn leek_query::salsa::Db,
    file: leek_query::salsa::SourceFile,
) -> LowerMirQueryResult {
    #[cfg(test)]
    salsa_probe::LOWER_MIR_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hir = leek_hir::query::lower_hir_query(db, file);
    let (program, diagnostics) = lower_and_optimize(hir.hir.as_ref(), OptLevel::O0);
    LowerMirQueryResult {
        program: Arc::new(program),
        diagnostics,
    }
}

#[cfg(test)]
mod salsa_probe {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    pub(super) static LOWER_MIR_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub(super) static SERIAL: Mutex<()> = Mutex::new(());
}

#[cfg(test)]
mod salsa_tests {
    use std::sync::atomic::Ordering;

    use leek_query::salsa::{LeekDb, SourceFile};
    use salsa::Setter;

    use super::lower_mir_query;
    use super::salsa_probe::{LOWER_MIR_CALLS, SERIAL};

    fn source(db: &mut LeekDb, text: &str) -> SourceFile {
        SourceFile::new(db, String::new(), 1, text.into(), 4, false, false, 0)
    }

    #[test]
    fn the_cascade_caches_mir() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(
            &mut db,
            "function add(a, b) { return a + b; }\nvar x = add(1, 2);\n",
        );
        let before = LOWER_MIR_CALLS.load(Ordering::Relaxed);
        let _ = lower_mir_query(&db, file);
        let after_first = LOWER_MIR_CALLS.load(Ordering::Relaxed);
        let _ = lower_mir_query(&db, file);
        let after_second = LOWER_MIR_CALLS.load(Ordering::Relaxed);

        assert_eq!(
            after_first - before,
            1,
            "first call executes lower_mir once"
        );
        assert_eq!(
            after_second - after_first,
            0,
            "second identical call must hit the salsa cache all the way through"
        );
    }

    #[test]
    fn semantic_edit_reruns_mir() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(&mut db, "var x = 5;");
        let before = LOWER_MIR_CALLS.load(Ordering::Relaxed);
        let _ = lower_mir_query(&db, file);
        let after_first = LOWER_MIR_CALLS.load(Ordering::Relaxed);

        file.set_text(&mut db).to("var y = 6;".into());

        let _ = lower_mir_query(&db, file);
        let after_second = LOWER_MIR_CALLS.load(Ordering::Relaxed);

        assert_eq!(after_first - before, 1);
        assert_eq!(
            after_second - after_first,
            1,
            "semantic change must re-execute"
        );
    }
}
