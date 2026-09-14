//! Pure parse entry points: text and flags in, [`ParsedFile`] out.
//!
//! Nothing here reads a pipeline [`Context`](leek_pipeline::Context) or —
//! given [`parse_file_with`] — the environment, so a driver that already
//! knows its [`FeatureFlags`](leek_span::FeatureFlags) can parse a file
//! without standing up a [`Step`](leek_pipeline::Step) first.

use leek_diagnostics::Diagnostic;
use leek_syntax::SyntaxNode;
use leek_syntax::language::GreenNode;
use leek_syntax::version::Version;

use crate::ast::{AstNode, SourceFile};
use crate::parse_tokens_with_classes;

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
///
/// Reads the experimental toggles from the environment; prefer
/// [`parse_file_with`] wherever the caller already has flags to pass.
pub fn parse_file(text: &str, source: leek_span::SourceId, version: Version) -> ParsedFile {
    parse_file_with_classes(text, source, version, &[])
}

/// Like [`parse_file`] but with extra known class names from the rest
/// of the program (see
/// [`KnownClassesArtifact`](crate::pipeline::KnownClassesArtifact)).
pub fn parse_file_with_classes(
    text: &str,
    source: leek_span::SourceId,
    version: Version,
    extra_classes: &[String],
) -> ParsedFile {
    parse_file_with(
        text,
        source,
        version,
        leek_span::FeatureFlags::from_env(),
        extra_classes,
    )
}

/// The flags-explicit parse entry point: lex `text`, parse the tokens
/// with `flags` and `extra_classes`, and return the green tree, its AST
/// view, and the lexer's diagnostics followed by the parser's.
///
/// This is the function drivers should reach for — it takes its feature
/// toggles as an argument rather than off the environment or a pipeline
/// `Context`. The two wrappers above differ from it only in filling
/// `flags` from the environment and `extra_classes` with nothing.
pub fn parse_file_with(
    text: &str,
    source: leek_span::SourceId,
    version: Version,
    flags: leek_span::FeatureFlags,
    extra_classes: &[String],
) -> ParsedFile {
    let lexed = leek_lexer::lex(text, source, version);
    let mut result = parse_tokens_with_classes(
        text,
        source,
        &lexed.tokens,
        version,
        crate::ParseFeatures::from(flags),
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
