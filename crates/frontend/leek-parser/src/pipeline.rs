//! Pipeline integration: parser as a [`Step`].

use leek_diagnostics::Diagnostic;
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStepStopOnError};
use leek_syntax::SyntaxNode;
use leek_syntax::language::GreenNode;
use leek_syntax::pipeline::version_from_byte;
use leek_syntax::version::Version;

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

/// AST view (`SourceFile`) cast from the green tree. `None` if the
/// parser failed to produce a `SourceFile` at the root (catastrophic
/// parse error).
#[derive(Debug, Clone)]
pub struct AstArtifact(pub Option<SourceFile>);
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
    pub ast: Option<SourceFile>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Parse `text` at `version`, returning a green tree, optional AST,
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
    let ast = SourceFile::cast(SyntaxNode::new_root(result.green.clone()));
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
        let ast = SourceFile::cast(SyntaxNode::new_root(green.clone()));
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
    if let Some((db, file)) = cx.salsa() {
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

/// Salsa-tracked entry point for parsing. Re-runs only when the
/// upstream [`lex_query`](leek_lexer::pipeline::lex_query) result
/// changes — which itself only re-runs when the input
/// [`SourceFile`](leek_pipeline::salsa::SourceFile)'s text changes.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn parse_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> ParseQueryResult {
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
