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
pub fn charge_query<'db>(
    db: &'db dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
    opts: ChargeOptsKey,
) -> ChargedHir {
    let hir = leek_hir::pipeline::lower_hir_query(db, file);
    let charged = add_charges(hir.hir.as_ref(), opts.into());
    ChargedHir(Arc::new(charged))
}
