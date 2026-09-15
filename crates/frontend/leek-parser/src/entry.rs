//! Pure parse entry points: text and options in, [`ParsedFile`] out.
//!
//! Nothing here reads a database or — given [`parse_file_with`] — the
//! environment, so a driver that already knows its
//! [`FeatureFlags`](leek_span::FeatureFlags) can parse a file without
//! standing one up. The memoized door onto the same grammar is
//! [`parse_query`](crate::query::parse_query).
//!
//! # Where lex diagnostics are reported
//!
//! Lexing and parsing each produce diagnostics, and exactly one place on
//! any given path must report the lexer's, or a bad character is reported
//! twice. The convention across the crate is:
//!
//! * [`parse_file_with`] — and everything that delegates to it — **owns**
//!   the sequence "lex, parse the tokens, prepend the lexer's diagnostics
//!   to the parser's". Anything that lexes its own text reports both.
//! * A caller that hands in tokens it lexed itself
//!   ([`parse_tokens_with_classes`](crate::parse_tokens_with_classes) and
//!   friends) already owns the lex diagnostics, and gets the parser's only.
//! * On the salsa path, [`parse_query`](crate::query::parse_query)
//!   follows the second rule — the [`Lex`](leek_lexer::query::Lex) step
//!   already emitted the lexer's. Every salsa-driven file, indexed on disk
//!   or open in an editor, runs through that step, so there is no path on
//!   which the query itself has to merge them.

use leek_diagnostics::Diagnostic;
use leek_syntax::SyntaxNode;
use leek_syntax::language::GreenNode;
use leek_syntax::version::Version;

use crate::ast::{AstNode, SourceFile};
use crate::{ParseFeatures, parse_tokens_with_classes};

/// Shared parse outcome for a single source file (disk or buffer).
#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub green: GreenNode,
    pub ast: SourceFile,
    pub diagnostics: Vec<Diagnostic>,
}

/// Everything a parse needs beyond the text and its [`SourceId`] — the
/// language version, the experimental grammar relaxations, and the class
/// names declared elsewhere in the program.
///
/// Bundled into one struct so the parse entry points don't grow a new
/// positional argument (and a new `_with_x` wrapper) per input, and so
/// that adding an input is a struct field rather than a signature break.
/// Every field is an explicit value: nothing here defaults itself off the
/// environment, which is what lets an include parse inherit the pipeline's
/// [`FeatureFlags`](leek_span::FeatureFlags) instead of silently diverging
/// from it.
///
/// [`SourceId`]: leek_span::SourceId
#[derive(Debug, Clone, Copy)]
pub struct ParseOptions<'a> {
    /// Language version — selects keyword gating and grammar shape. Derive
    /// it from [`parse_pragmas`](leek_syntax::parse_pragmas) when the text
    /// carries a `// @version:` pragma.
    pub version: Version,
    /// Experimental grammar relaxations. [`ParseFeatures::default()`] is
    /// the shipped grammar; a driver that honours `LEEK_EXPERIMENTAL_*`
    /// reads [`FeatureFlags::from_env`](leek_span::FeatureFlags::from_env)
    /// once at its own boundary and passes the result to [`with_flags`].
    ///
    /// [`with_flags`]: ParseOptions::with_flags
    pub features: ParseFeatures,
    /// Class names declared in *other* files of the same program (the
    /// include closure), so `testClass tc = …` parses as a typed
    /// declaration. Classes declared in this file are found by the
    /// parser's own token pre-scan and need not be listed. See
    /// [`KnownClassesArtifact`](crate::query::KnownClassesArtifact).
    pub extra_classes: &'a [String],
}

impl<'a> ParseOptions<'a> {
    /// Options for `version` with the shipped grammar and no cross-file
    /// class names.
    pub fn new(version: Version) -> Self {
        Self {
            version,
            features: ParseFeatures::default(),
            extra_classes: &[],
        }
    }

    /// Set the experimental [`ParseFeatures`].
    pub fn with_features(mut self, features: ParseFeatures) -> Self {
        self.features = features;
        self
    }

    /// Set the experimental features from a [`FeatureFlags`] the caller
    /// already threaded through its pipeline.
    ///
    /// [`FeatureFlags`]: leek_span::FeatureFlags
    pub fn with_flags(self, flags: leek_span::FeatureFlags) -> Self {
        self.with_features(ParseFeatures::from(flags))
    }

    /// Set the program-wide class names (see [`extra_classes`]).
    ///
    /// [`extra_classes`]: ParseOptions::extra_classes
    pub fn with_extra_classes(self, extra_classes: &'a [String]) -> Self {
        Self {
            extra_classes,
            ..self
        }
    }
}

/// Parse `text` at `version`, returning a green tree, its AST view,
/// and diagnostics.
///
/// Reads the experimental toggles from the environment; prefer
/// [`parse_file_with`] wherever the caller already has options to pass.
#[deprecated(note = "reads LEEK_EXPERIMENTAL_* from the process environment; call \
            parse_file_with(text, source, &ParseOptions::new(version)) and pass \
            the flags your entry boundary already read")]
pub fn parse_file(text: &str, source: leek_span::SourceId, version: Version) -> ParsedFile {
    parse_file_with(
        text,
        source,
        &ParseOptions::new(version).with_flags(leek_span::FeatureFlags::from_env()),
    )
}

/// Like [`parse_file`] but with extra known class names from the rest
/// of the program (see
/// [`KnownClassesArtifact`](crate::query::KnownClassesArtifact)).
#[deprecated(note = "reads LEEK_EXPERIMENTAL_* from the process environment; call \
            parse_file_with with ParseOptions::with_extra_classes and pass the \
            flags your entry boundary already read")]
pub fn parse_file_with_classes(
    text: &str,
    source: leek_span::SourceId,
    version: Version,
    extra_classes: &[String],
) -> ParsedFile {
    parse_file_with(
        text,
        source,
        &ParseOptions::new(version)
            .with_flags(leek_span::FeatureFlags::from_env())
            .with_extra_classes(extra_classes),
    )
}

/// The one implementation of "lex `text`, parse the tokens with
/// `options`, and return the green tree, its AST view, and the lexer's
/// diagnostics followed by the parser's".
///
/// Every other parse path that lexes its own text delegates here, so the
/// diagnostic ordering and the lex-diagnostic ownership described in the
/// [module docs](self) are decided in exactly one place. This is also the
/// function drivers should reach for: it takes its feature toggles in
/// [`ParseOptions`] rather than off the environment or a pipeline
/// `Context`.
pub fn parse_file_with(
    text: &str,
    source: leek_span::SourceId,
    options: &ParseOptions<'_>,
) -> ParsedFile {
    let lexed = leek_lexer::lex(text, source, options.version);
    let mut result = parse_tokens_with_classes(
        text,
        source,
        &lexed.tokens,
        options.version,
        options.features,
        options.extra_classes,
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
