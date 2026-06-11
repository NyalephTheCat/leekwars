//! Pipeline integration: lexer as a [`Step`].

use leek_pipeline::{Artifact, Context};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_syntax::pipeline::version_from_byte;

use crate::LexResult;

/// Token stream + lex diagnostics.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone)]
pub struct TokensArtifact(pub LexResult);
impl Artifact for TokensArtifact {}

// Lexer step. Reads the input text; produces tokens + lex
// diagnostics. Independent of any earlier step.
leek_pipeline::define_step!(Lex, "lex", TokensArtifact, run_lex_step);

impl RecipeStep for Lex {
    fn build(_: &RecipeParams) -> Box<dyn leek_pipeline::Step> {
        Box::new(Lex)
    }
}

impl RecipeArtifact for TokensArtifact {
    type Producer = Lex;
    type Requires = (leek_syntax::pipeline::PragmasArtifact,);
    type Produces = (TokensArtifact,);
}

fn run_lex_step(cx: &Context<'_>) -> (LexResult, Vec<leek_diagnostics::Diagnostic>) {
    let mut result = run_lex(cx);
    let diags = std::mem::take(&mut result.diagnostics);
    (result, diags)
}

/// Salsa-aware lex driver. When the pipeline is driven through
/// [`Pipeline::run_memoized`](leek_pipeline::Pipeline::run_memoized),
/// dispatches into the [`lex_query`] tracked query so identical re-runs
/// are cache hits. Otherwise falls through to a direct [`crate::lex`]
/// call.
fn run_lex(cx: &Context<'_>) -> LexResult {
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        return lex_query(db, file);
    }
    crate::lex(cx.text(), cx.source(), version_from_byte(cx.version_byte()))
}

/// Salsa-tracked entry point. Re-runs only when the input
/// [`SourceFile`](leek_pipeline::salsa::SourceFile)'s text or version
/// byte changes.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn lex_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> LexResult {
    #[cfg(test)]
    crate::salsa_probe::LEX_QUERY_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let text = file.text(db);
    let source = file.source(db);
    let version = version_from_byte(file.version_byte(db));
    crate::lex(text, source, version)
}

#[cfg(all(test, feature = "salsa"))]
mod salsa_tests {
    use std::sync::atomic::Ordering;

    use leek_pipeline::Pipeline;
    use leek_pipeline::salsa::{LeekDb, SourceFile};
    use salsa::Setter;

    use super::{Lex, TokensArtifact};
    use crate::salsa_probe::{LEX_QUERY_CALLS, SERIAL};

    /// Build a `SourceFile` from a text snippet at a fixed source id.
    fn source(db: &mut LeekDb, text: &str) -> SourceFile {
        SourceFile::new(db, 1, text.to_string(), 4, false, 0, Vec::new())
    }

    #[test]
    fn identical_inputs_hit_cache() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(&mut db, "var x = 5;");
        let pipeline = Pipeline::new().with(Lex);

        let before = LEX_QUERY_CALLS.load(Ordering::Relaxed);
        let run1 = pipeline.run_memoized(&db, file);
        let after_first = LEX_QUERY_CALLS.load(Ordering::Relaxed);
        let run2 = pipeline.run_memoized(&db, file);
        let after_second = LEX_QUERY_CALLS.load(Ordering::Relaxed);

        assert_eq!(
            after_first - before,
            1,
            "first run should execute the query"
        );
        assert_eq!(
            after_second - after_first,
            0,
            "second identical run should hit the salsa cache"
        );

        let tokens1 = &run1.get::<TokensArtifact>().unwrap().0.tokens;
        let tokens2 = &run2.get::<TokensArtifact>().unwrap().0.tokens;
        assert_eq!(tokens1, tokens2);
    }

    #[test]
    fn changing_text_reruns_query() {
        let _guard = SERIAL.lock().unwrap();
        let mut db = LeekDb::default();
        let file = source(&mut db, "var x = 5;");
        let pipeline = Pipeline::new().with(Lex);

        let before = LEX_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let after_first = LEX_QUERY_CALLS.load(Ordering::Relaxed);

        // Mutating the salsa input invalidates any tracked-query
        // result that read it.
        file.set_text(&mut db).to("var y = 6;".to_string());

        let _ = pipeline.run_memoized(&db, file);
        let after_second = LEX_QUERY_CALLS.load(Ordering::Relaxed);

        assert_eq!(after_first - before, 1);
        assert_eq!(
            after_second - after_first,
            1,
            "changing input text must re-execute the query"
        );
    }
}
