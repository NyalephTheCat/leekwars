//! Pipeline integration: formatter as a [`Step`].
//!
//! Mirrors [`leek_parser::pipeline`] — the formatter ships a
//! direct-call step plus a salsa-tracked [`format_query`] when the
//! `salsa` feature is enabled. The step inserts a
//! [`FormattedArtifact`] into the pipeline context.

use std::sync::Arc;

use leek_parser::pipeline::GreenTreeArtifact;
use leek_pipeline::{Artifact, Context, RecipeArtifact, RecipeParams, RecipeStep, Step, StepError};
use leek_syntax::language::GreenNode;
use leek_syntax::pipeline::version_from_byte;

use crate::FormatOptions;

/// Formatter output: the rendered source text.
#[derive(Debug, Clone)]
pub struct FormattedArtifact(pub Arc<String>);
impl Artifact for FormattedArtifact {}

/// Formatter pipeline step. Sequenced after
/// [`leek_parser::pipeline::Parse`] so the green tree is available
/// in the context.
///
/// The default constructor uses [`FormatOptions::default`]. Build
/// with [`Fmt::with_options`] to format with non-default settings
/// (e.g. options loaded from `Miku.toml`).
#[derive(Default)]
pub struct Fmt {
    opts: FormatOptions,
}

impl Fmt {
    /// New step with the given options.
    pub fn with_options(opts: FormatOptions) -> Self {
        Self { opts }
    }
}

impl RecipeStep for Fmt {
    fn build(_: &RecipeParams) -> Box<dyn leek_pipeline::Step> {
        Box::new(Fmt::default())
    }
}

impl RecipeArtifact for FormattedArtifact {
    type Producer = Fmt;
    type Requires = (GreenTreeArtifact,);
    type Produces = (FormattedArtifact,);
}

impl Step for Fmt {
    fn name(&self) -> &'static str {
        "fmt"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let text = run_format(cx, &self.opts);
        cx.insert(FormattedArtifact(Arc::new(text)));
        Ok(())
    }
}

fn run_format(cx: &Context<'_>, opts: &FormatOptions) -> String {
    // The salsa-tracked path always uses defaults; non-default
    // options short-circuit to the direct path so the user's
    // settings actually take effect.
    #[cfg(feature = "salsa")]
    if opts == &FormatOptions::default()
        && let Some((db, file)) = cx.salsa()
    {
        let out = format_query(db, file);
        return out.text.as_ref().clone();
    }

    // Direct path: prefer the green tree the Parse step already
    // produced; otherwise re-run `parse()` from raw text.
    let green = parse_or_reuse(cx);
    crate::format(&green, opts)
}

fn parse_or_reuse(cx: &Context<'_>) -> GreenNode {
    if let Some(g) = cx.get::<GreenTreeArtifact>() {
        return g.0.clone();
    }
    let version = version_from_byte(cx.version_byte());
    leek_parser::parse(cx.text(), cx.source(), version).green
}

// ---- Salsa-tracked entry point ----

#[cfg(feature = "salsa")]
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatQueryResult {
    pub text: Arc<String>,
}

/// Salsa-tracked formatter entry point. Re-runs only when
/// [`leek_parser::pipeline::parse_query`]'s result changes — which
/// itself only re-runs when the input file's text changes.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn format_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> FormatQueryResult {
    let parsed = leek_parser::pipeline::parse_query(db, file);
    let text = crate::format(&parsed.green, &FormatOptions::default());
    FormatQueryResult {
        text: Arc::new(text),
    }
}
