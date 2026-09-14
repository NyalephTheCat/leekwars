//! Pipeline integration: charge insertion as a [`Step`].

use std::sync::Arc;

use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::{Artifact, Context, Step, StepError};

use crate::{ChargeOpts, add_charges};

/// HIR with static [`Stmt::Charge`](leek_hir::Stmt::Charge) inserted.
/// Stored as a distinct artifact so a single run can produce both
/// the canonical and the charged HIR (e.g. Java-exact reads HIR;
/// the interpreter reads charged HIR). Held by `Arc` so the salsa
/// cache hit stays pointer-cheap.
#[derive(Debug, Clone)]
pub struct ChargedHirArtifact(pub Arc<HirFile>);
impl Artifact for ChargedHirArtifact {}

/// Insert static [`Stmt::Charge`](leek_hir::Stmt::Charge) markers
/// over the canonical HIR contributed by
/// [`leek_hir::pipeline::LowerHir`]. The original [`HirArtifact`] is
/// untouched.
pub struct Charge {
    pub opts: ChargeOpts,
}

impl Charge {
    pub fn new(opts: ChargeOpts) -> Self {
        Self { opts }
    }
    pub fn default_opts() -> Self {
        Self {
            opts: ChargeOpts::default(),
        }
    }
}

impl Step for Charge {
    fn name(&self) -> &'static str {
        "charge"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let charged = run_charge(self.opts, cx);
        if let Some(charged) = charged {
            cx.insert(ChargedHirArtifact(charged));
        }
        Ok(())
    }
}

fn run_charge(opts: ChargeOpts, cx: &Context<'_>) -> Option<Arc<HirFile>> {
    // NOTE(#428): unlike `LowerHir` / `TypeCheck`, this branch does not
    // check for an include-aware run, so appending `Charge` to a memoized
    // `pipeline_with_includes` would charge HIR lowered from the entry file
    // alone. Dormant: the only `run_memoized` caller is the LSP, which
    // never appends this step.
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        return Some(charge_query(db, file, opts.into()).0);
    }
    let hir = cx.get::<HirArtifact>()?;
    Some(Arc::new(add_charges(hir.0.as_ref(), opts)))
}

/// Tracked return type — newtype over `Arc<HirFile>` so the macro
/// has a single salsa-friendly return.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq)]
pub struct ChargedHir(pub Arc<HirFile>);

/// Salsa-friendly wrapper for [`ChargeOpts`]. Carries the same
/// content but adds the derives the salsa-tracked query input
/// position requires. (We could derive on `ChargeOpts` directly but
/// keeping the salsa derive local to `pipeline.rs` keeps salsa out of
/// the `lib.rs` API surface.)
#[cfg(feature = "salsa")]
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChargeOptsKey {
    pub per_stmt: u64,
    pub per_expr: u64,
}

#[cfg(feature = "salsa")]
impl From<ChargeOpts> for ChargeOptsKey {
    fn from(o: ChargeOpts) -> Self {
        Self {
            per_stmt: o.per_stmt,
            per_expr: o.per_expr,
        }
    }
}

#[cfg(feature = "salsa")]
impl From<ChargeOptsKey> for ChargeOpts {
    fn from(k: ChargeOptsKey) -> Self {
        ChargeOpts {
            per_stmt: k.per_stmt,
            per_expr: k.per_expr,
        }
    }
}

/// Salsa-tracked entry point. Re-runs only when
/// [`lower_hir_query`](leek_hir::pipeline::lower_hir_query)'s HIR
/// changes or the options key flips.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn charge_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
    opts: ChargeOptsKey,
) -> ChargedHir {
    let hir = leek_hir::pipeline::lower_hir_query(db, file);
    let charged = add_charges(hir.hir.as_ref(), opts.into());
    ChargedHir(Arc::new(charged))
}

// `charge_query` is `#[cfg(feature = "salsa")]`, and nothing in the
// workspace requests `leek-charge/salsa` — so until the self
// dev-dependency in `Cargo.toml` landed, this query was never compiled by
// `cargo test --workspace` nor linted by `clippy --all-targets`. Refusing
// to build the test target without the feature keeps it that way.
#[cfg(all(test, not(feature = "salsa")))]
compile_error!(
    "leek-charge's test build needs the `salsa` feature — restore the self \
     dev-dependency in crates/middle/leek-charge/Cargo.toml"
);

#[cfg(all(test, feature = "salsa"))]
mod salsa_tests {
    use leek_hir::pipeline::LowerHir;
    use leek_lexer::pipeline::Lex;
    use leek_parser::pipeline::Parse;
    use leek_pipeline::Pipeline;
    use leek_pipeline::salsa::{LeekDb, SourceFile};
    use leek_syntax::pipeline::Pragma;
    use salsa::Setter;

    use super::{Charge, ChargedHirArtifact};

    fn pipeline() -> Pipeline {
        Pipeline::new()
            .with(Pragma)
            .with(Lex)
            .with(Parse)
            .with(LowerHir::default())
            .with(Charge::default_opts())
    }

    #[test]
    fn the_charged_hir_survives_the_salsa_path() {
        // The salsa branch returns the query's `Arc` rather than charging
        // the in-context `HirArtifact`. A wrong branch here publishes no
        // artifact at all, which the interpreter sees as nothing to run.
        let db = LeekDb::default();
        let file = SourceFile::new(
            &db,
            1,
            "function add(a, b) { return a + b; }\nvar x = add(1, 2);\n".to_string(),
            4,
            false,
            0,
            Vec::new(),
        );
        let run = pipeline().run_memoized(&db, file);
        assert!(
            run.get::<ChargedHirArtifact>().is_some(),
            "the salsa branch must still publish a ChargedHirArtifact"
        );
    }

    #[test]
    fn an_edit_is_reflected_in_the_charged_hir() {
        // Guards the cache key: charging is memoized on the source file, so
        // a stale entry would hand the interpreter the previous program.
        let mut db = LeekDb::default();
        let file = SourceFile::new(&db, 1, "var x = 1;\n".to_string(), 4, false, 0, Vec::new());
        let pipeline = pipeline();

        let first = pipeline
            .run_memoized(&db, file)
            .get::<ChargedHirArtifact>()
            .expect("charged")
            .0
            .clone();

        file.set_text(&mut db)
            .to("var x = 1;\nvar y = 2;\nvar z = 3;\n".to_string());

        let second = pipeline
            .run_memoized(&db, file)
            .get::<ChargedHirArtifact>()
            .expect("charged")
            .0
            .clone();

        assert_ne!(
            first, second,
            "a semantic edit must not return the cached charged HIR"
        );
    }
}
