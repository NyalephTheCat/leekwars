//! Pipeline integration: pragma preprocessing as a [`Step`].
//!
//! [`version_from_byte`], which decodes the pipeline's `version_byte`
//! into a [`Version`](crate::version::Version), lives in
//! [`crate::version`] and is re-exported here for the passes that still
//! reach for it through this module.

use leek_diagnostics::Diagnostic;
use leek_pipeline::{Artifact, Context};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};

use crate::pragma::{Pragmas, parse_pragmas};

pub use crate::version::version_from_byte;

/// Output of [`Pragma`].
#[derive(Debug, Clone)]
pub struct PragmasArtifact(pub Pragmas);
impl Artifact for PragmasArtifact {}

// Pragma preprocessing — extracts `// @version`, `// @strict`, …
//
// The context's `Input::version_byte` / `Input::strict` are the
// authoritative language settings. Drivers settle them once, before the
// pipeline runs, with `leek_span::pragma::LanguageSettings::resolve`
// (override > pragma > default); no pass re-derives the version from
// pragmas. This step only contributes the parsed pragmas (experimental
// features) plus any pragma diagnostics.
leek_pipeline::define_step!(Pragma, "pragma", PragmasArtifact, run_pragma);

impl RecipeStep for Pragma {
    fn build(_: &RecipeParams) -> Box<dyn leek_pipeline::Step> {
        Box::new(Pragma)
    }
}

impl RecipeArtifact for PragmasArtifact {
    type Producer = Pragma;
    type Requires = ();
    type Produces = (PragmasArtifact,);
}

/// Salsa-aware pragma driver. Dispatches to [`pragma_query`] when the
/// pipeline is driven through
/// [`Pipeline::run_memoized`](leek_pipeline::Pipeline::run_memoized);
/// otherwise calls [`parse_pragmas`] directly.
fn run_pragma(cx: &Context<'_>) -> (Pragmas, Vec<Diagnostic>) {
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        let out = pragma_query(db, file);
        return (out.pragmas, out.diagnostics);
    }
    parse_pragmas(cx.text(), cx.source())
}

/// Tracked return value: pragmas + their parse-time diagnostics.
/// Single-struct return so the salsa-tracked query is well-formed.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PragmaResult {
    pub pragmas: Pragmas,
    pub diagnostics: Vec<Diagnostic>,
}

/// Salsa-tracked entry point for pragma preprocessing. Re-runs only
/// when the input [`SourceFile`](leek_pipeline::salsa::SourceFile)'s
/// text changes.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn pragma_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> PragmaResult {
    let (pragmas, diagnostics) = parse_pragmas(file.text(db), file.source(db));
    PragmaResult {
        pragmas,
        diagnostics,
    }
}
