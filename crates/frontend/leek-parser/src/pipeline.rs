//! Pipeline integration: parser as a [`Step`].

use leek_diagnostics::Diagnostic;
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStepStopOnError};
use leek_syntax::SyntaxNode;
use leek_syntax::language::GreenNode;
use leek_syntax::version::Version;
use leek_syntax::version::version_from_byte;

use crate::ast::{AstNode, SourceFile};
use crate::parse_tokens_with_classes;

/// The parser's green tree.
#[derive(Debug, Clone)]
pub struct GreenTreeArtifact(pub GreenNode);
impl Artifact for GreenTreeArtifact {}

impl GreenTreeArtifact {
    /// Wrap as a rowan red-tree root.
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.0.clone())
    }
}

/// AST view (`SourceFile`) cast from the green tree.
///
/// Always present once the [`Parse`] step has run: `grammar::source_file`
/// opens a `SourceFile` node before any production and closes it on every
/// path, so the root cast cannot fail. Recovery from a syntax error builds
/// an `ErrorNode` *inside* that root.
#[derive(Debug, Clone)]
pub struct AstArtifact(pub SourceFile);
impl Artifact for AstArtifact {}

/// Class names declared anywhere in the program — the include
/// closure's `class IDENT` declarations. Published *before* the
/// [`Parse`] step (by `leek-resolver`'s `ResolveIncludes`) so the
/// entry parse can recognize a lowercase class from an included file
/// as a type head (`testClass tc = …`), mirroring upstream's
/// program-wide `getDefinedClass` lookup. Classes declared in the
/// file being parsed are found by the parser's own token pre-scan
/// and don't need this artifact.
#[derive(Debug, Clone, Default)]
pub struct KnownClassesArtifact(pub Vec<String>);
impl Artifact for KnownClassesArtifact {}

/// Shared parse outcome for a single source file (disk or buffer).
#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub green: GreenNode,
    pub ast: SourceFile,
    pub diagnostics: Vec<Diagnostic>,
}

/// Parse `text` at `version`, returning a green tree, its AST view,
/// and diagnostics. Used by include resolution and the project index
/// so every file goes through the same parse path.
pub fn parse_file(text: &str, source: leek_span::SourceId, version: Version) -> ParsedFile {
    parse_file_with_classes(text, source, version, &[])
}

/// Like [`parse_file`] but with extra known class names from the rest
/// of the program (see [`KnownClassesArtifact`]).
pub fn parse_file_with_classes(
    text: &str,
    source: leek_span::SourceId,
    version: Version,
    extra_classes: &[String],
) -> ParsedFile {
    let lexed = leek_lexer::lex(text, source, version);
    let mut result = parse_tokens_with_classes(
        text,
        source,
        &lexed.tokens,
        version,
        crate::ParseFeatures::from_env(),
        extra_classes,
    );
    let mut diagnostics = lexed.diagnostics;
    diagnostics.append(&mut result.diagnostics);
    let ast = SourceFile::cast(SyntaxNode::new_root(result.green.clone()))
        .expect("grammar::source_file always opens a SourceFile root");
    ParsedFile {
        green: result.green,
        ast,
        diagnostics,
    }
}

/// Parser step. Lexes internally; produces a green tree + AST view.
///
/// Sequenced after [`leek_lexer::pipeline::Lex`] when both are
/// present so that the parser's diagnostic stream stays the
/// authoritative source — the lexer's `TokensArtifact` is mainly for
/// `--emit tokens`.
pub struct Parse;

impl Step for Parse {
    fn name(&self) -> &'static str {
        "parse"
    }
    fn run(&self, cx: &mut Context) -> Result<(), StepError> {
        let (green, diagnostics) = run_parse(cx);
        cx.emit_all(diagnostics.iter().cloned());
        let ast = SourceFile::cast(SyntaxNode::new_root(green.clone()))
            .expect("grammar::source_file always opens a SourceFile root");
        cx.insert(GreenTreeArtifact(green));
        cx.insert(AstArtifact(ast));
        Ok(())
    }
}

impl RecipeStepStopOnError for Parse {
    fn build_inner(_: &RecipeParams) -> Parse {
        Parse
    }
}

impl RecipeArtifact for GreenTreeArtifact {
    type Producer = Parse;
    type Requires = (
        leek_syntax::pipeline::PragmasArtifact,
        leek_lexer::pipeline::TokensArtifact,
    );
    type Produces = (GreenTreeArtifact, AstArtifact);
}

impl RecipeArtifact for AstArtifact {
    type Producer = Parse;
    type Requires = (
        leek_syntax::pipeline::PragmasArtifact,
        leek_lexer::pipeline::TokensArtifact,
    );
    type Produces = (GreenTreeArtifact, AstArtifact);
}

/// Salsa-aware parse driver. When the pipeline is driven through
/// [`Pipeline::run_memoized`](leek_pipeline::Pipeline::run_memoized),
/// dispatches into [`parse_query`] which itself calls
/// [`leek_lexer::pipeline::lex_query`] — so the two stages share a
/// single memoized lex.
///
/// On the direct path we keep the existing optimization of reusing
/// [`leek_lexer::pipeline::TokensArtifact`] when an earlier
/// [`Lex`](leek_lexer::pipeline::Lex) step has already produced one.
fn run_parse(cx: &Context<'_>) -> (GreenNode, Vec<Diagnostic>) {
    #[cfg(feature = "salsa")]
    if cx.get::<KnownClassesArtifact>().is_none()
        && let Some((db, file)) = cx.salsa()
    {
        let out = parse_query(db, file);
        return (out.green, out.diagnostics);
    }
    let version = version_from_byte(cx.version_byte());
    let features = crate::ParseFeatures::from(cx.flags());
    // Class names from the include closure, when a `ResolveIncludes`
    // step ran ahead of us (see [`KnownClassesArtifact`]).
    let empty: Vec<String> = Vec::new();
    let extra_classes = cx
        .get::<KnownClassesArtifact>()
        .map_or(&empty[..], |a| &a.0[..]);
    let result = if let Some(tokens) = cx.get::<leek_lexer::pipeline::TokensArtifact>() {
        parse_tokens_with_classes(
            cx.text(),
            cx.source(),
            &tokens.0.tokens,
            version,
            features,
            extra_classes,
        )
    } else {
        let lexed = leek_lexer::lex(cx.text(), cx.source(), version);
        let mut result = parse_tokens_with_classes(
            cx.text(),
            cx.source(),
            &lexed.tokens,
            version,
            features,
            extra_classes,
        );
        let mut diags = lexed.diagnostics;
        diags.append(&mut result.diagnostics);
        result.diagnostics = diags;
        result
    };
    (result.green, result.diagnostics)
}

/// Tracked return value for [`parse_query`]: the green tree plus the
/// parser's own diagnostics (lex diagnostics are emitted separately by
/// the [`Lex`](leek_lexer::pipeline::Lex) step).
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseQueryResult {
    pub green: GreenNode,
    pub diagnostics: Vec<Diagnostic>,
}

/// Salsa-tracked entry point for parsing. Re-runs when the upstream
/// [`lex_query`](leek_lexer::pipeline::lex_query) result changes, or when
/// any input field this body reads off the
/// [`SourceFile`](leek_pipeline::salsa::SourceFile) changes: `text`,
/// `version_byte`, `flags_bits` or `extra_classes`. (`strict` is *not*
/// read here — it only reaches the type checker.)
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn parse_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> ParseQueryResult {
    #[cfg(test)]
    salsa_probe::PARSE_QUERY_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let lex = leek_lexer::pipeline::lex_query(db, file);
    let text = file.text(db);
    let source = file.source(db);
    let version = version_from_byte(file.version_byte(db));
    let features =
        crate::ParseFeatures::from(leek_span::FeatureFlags::from_bits(file.flags_bits(db)));
    let result = parse_tokens_with_classes(
        text,
        source,
        &lex.tokens,
        version,
        features,
        file.extra_classes(db),
    );
    ParseQueryResult {
        green: result.green,
        diagnostics: result.diagnostics,
    }
}

/// Salsa-tracked parse for an on-disk project file. Re-runs when
/// [`ProjectFile`](leek_pipeline::salsa::ProjectFile)'s text changes.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn parse_project_file_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::ProjectFile,
) -> ParseQueryResult {
    let version = version_from_byte(file.version_byte(db));
    let features =
        crate::ParseFeatures::from(leek_span::FeatureFlags::from_bits(file.flags_bits(db)));
    let text = file.text(db);
    let source = file.source(db);
    let lexed = leek_lexer::lex(text, source, version);
    let mut result = parse_tokens_with_classes(
        text,
        source,
        &lexed.tokens,
        version,
        features,
        file.extra_classes(db),
    );
    let mut diags = lexed.diagnostics;
    diags.append(&mut result.diagnostics);
    result.diagnostics = diags;
    ParseQueryResult {
        green: result.green,
        diagnostics: result.diagnostics,
    }
}

#[cfg(all(test, feature = "salsa"))]
mod salsa_probe {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    pub(super) static PARSE_QUERY_CALLS: AtomicUsize = AtomicUsize::new(0);
    /// The counter is process-global and cargo runs `#[test]`s on a thread
    /// pool, so every test that reads a delta takes this first.
    pub(super) static SERIAL: Mutex<()> = Mutex::new(());
}

/// Which [`SourceFile`](leek_pipeline::salsa::SourceFile) inputs
/// [`parse_query`] actually depends on.
///
/// This is the LSP's incremental contract: an input the query reads but
/// that salsa does not see it read would leave a stale parse in the cache
/// after an edit, and an input it *doesn't* read but appears to would
/// re-parse the world on every keystroke. `extra_classes` is the one with
/// teeth — leek-lsp pushes cross-file class names into it (fix #34), and a
/// parse that didn't re-run when they changed would go on rejecting
/// `lowercaseClassFromInclude x = …`.
#[cfg(all(test, feature = "salsa"))]
mod salsa_invalidation_tests {
    use std::sync::atomic::Ordering;

    use leek_pipeline::Pipeline;
    use leek_pipeline::salsa::{LeekDb, SourceFile};
    use salsa::Setter;

    use super::Parse;
    use super::salsa_probe::{PARSE_QUERY_CALLS, SERIAL};

    const SRC: &str = "class c {}\nc x = new c();\n";

    fn fixture() -> (LeekDb, SourceFile) {
        let db = LeekDb::default();
        let file = SourceFile::new(&db, 1, SRC.to_string(), 4, false, 0, Vec::new());
        (db, file)
    }

    /// Run the pipeline once to prime the cache, apply `edit`, run again,
    /// and report how many times `parse_query` executed on the second run.
    fn reruns_after(edit: impl FnOnce(&mut LeekDb, SourceFile)) -> usize {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut db, file) = fixture();
        let pipeline = Pipeline::new().with(Parse);

        let before = PARSE_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let primed = PARSE_QUERY_CALLS.load(Ordering::Relaxed);
        assert_eq!(primed - before, 1, "the first run must execute the query");

        edit(&mut db, file);

        let _ = pipeline.run_memoized(&db, file);
        PARSE_QUERY_CALLS.load(Ordering::Relaxed) - primed
    }

    #[test]
    fn an_untouched_input_reuses_the_cached_parse() {
        assert_eq!(reruns_after(|_, _| {}), 0, "no edit, no work");
    }

    /// Salsa inputs do not compare: `set_x().to(same_value)` bumps the
    /// revision and re-runs every query that read `x`, equal or not. That
    /// is why `leek_lsp::workspace::apply_language` reads each field back
    /// and writes only the ones that differ, and why
    /// `recompute_class_union` returns early on an unchanged union —
    /// dropping either guard would re-parse the open document on every
    /// `didChange`. This test is the reason those guards must stay.
    #[test]
    fn re_setting_a_field_to_its_current_value_still_reparses() {
        assert_eq!(
            reruns_after(|db, file| {
                file.set_version_byte(db).to(4);
            }),
            1,
            "a no-op write is still a write as far as salsa is concerned"
        );
    }

    #[test]
    fn changing_the_language_version_reparses() {
        // The version picks the grammar, so a cached parse from another
        // version is simply the wrong tree.
        assert_eq!(
            reruns_after(|db, file| {
                file.set_version_byte(db).to(1);
            }),
            1
        );
    }

    #[test]
    fn changing_the_experimental_feature_flags_reparses() {
        // `flags_bits` becomes `ParseFeatures`, which gates syntax
        // (generics, enums, interfaces …).
        assert_eq!(
            reruns_after(|db, file| {
                file.set_flags_bits(db).to(0b1111_1111);
            }),
            1
        );
    }

    #[test]
    fn changing_the_known_class_names_reparses() {
        assert_eq!(
            reruns_after(|db, file| {
                file.set_extra_classes(db)
                    .to(vec!["fromAnotherFile".to_string()]);
            }),
            1,
            "cross-file class names are a parse input (fix #34)"
        );
    }

    #[test]
    fn changing_strict_does_not_reparse() {
        // `strict` is a type-checker input; parsing must not depend on it,
        // or toggling it would throw away every green tree in the cache.
        assert_eq!(
            reruns_after(|db, file| {
                file.set_strict(db).to(true);
            }),
            0
        );
    }
}
