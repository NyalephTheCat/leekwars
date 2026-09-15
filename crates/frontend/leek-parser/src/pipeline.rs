//! Pipeline integration: parser as a [`Step`].
//!
//! Two parse paths live here — the direct [`Parse`] step and
//! [`parse_query`] — and they differ only in *who lexed the text*, never
//! in how the resulting diagnostics are ordered. A path that lexes its
//! own text goes through [`crate::parse_file_with`] and reports the
//! lexer's diagnostics ahead of the parser's; a path handed tokens
//! somebody else lexed reports the parser's only, because that somebody
//! already emitted the lexer's. The [entry module docs](crate::entry)
//! state the convention in full.

use leek_diagnostics::Diagnostic;
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStepStopOnError};
use leek_syntax::SyntaxNode;
use leek_syntax::language::GreenNode;
use leek_syntax::version::version_from_byte;

use crate::ast::{AstNode, SourceFile};
use crate::parse_tokens_with_classes;
use leek_pipeline::salsa::ProgramClasses;

/// The parser's green tree.
///
/// One producer today: the [`Parse`] step publishes this and
/// [`AstArtifact`] together, from a single parse. Later in R1 the pair
/// becomes two queries over one green tree — the tree stays the single
/// parse result, and the AST view is a cast on top of it — so treat
/// them as two views of one artifact, never two parses.
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
///
/// Shares its producer with [`GreenTreeArtifact`] today, and becomes the
/// second of two queries over that one green tree later in R1.
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

/// The parse entry points now live in the crate's `entry` module and are
/// re-exported here so importers of `leek_parser::pipeline::parse_file*`
/// keep compiling. New callers should use [`parse_file_with`], which takes
/// its version, [`ParseFeatures`](crate::ParseFeatures) and class names in
/// a [`ParseOptions`] instead of defaulting them off the environment; the
/// two `parse_file*` shims are deprecated for exactly that reason.
pub use crate::entry::{ParseOptions, ParsedFile, parse_file_with};
#[expect(
    deprecated,
    reason = "re-exporting the deprecation shims is the point of this statement"
)]
pub use crate::entry::{parse_file, parse_file_with_classes};

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
    if cx.get::<KnownClassesArtifact>().is_none()
        && let Some((db, file)) = cx.salsa()
    {
        // The gate above is why the class set handed over here is
        // always empty: this branch is taken only when no
        // `ResolveIncludes` step published a closure's classes. A run
        // that *has* a closure parses directly below instead of keying
        // the query on them, which is why such a run ends up with two
        // green trees for the entry (#522) — fixing that means routing
        // the include-aware pipeline through the whole-program queries
        // in `leek-db`, not shrinking this gate.
        let out = parse_query(db, file, ProgramClasses::none(db));
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
    if let Some(tokens) = cx.get::<leek_lexer::pipeline::TokensArtifact>() {
        // The `Lex` step lexed and emitted the lexer's diagnostics, so
        // this path reports the parser's only — see the [entry module
        // docs](crate::entry).
        let result = parse_tokens_with_classes(
            cx.text(),
            cx.source(),
            &tokens.0.tokens,
            version,
            features,
            extra_classes,
        );
        return (result.green, result.diagnostics);
    }
    let parsed = crate::parse_file_with(
        cx.text(),
        cx.source(),
        &crate::ParseOptions::new(version)
            .with_features(features)
            .with_extra_classes(extra_classes),
    );
    (parsed.green, parsed.diagnostics)
}

/// Tracked return value for [`parse_query`]: the green tree plus the
/// parser's own diagnostics (lex diagnostics are emitted separately by
/// the [`Lex`](leek_lexer::pipeline::Lex) step).
#[derive(salsa::Update, Debug, Clone, PartialEq, Eq)]
pub struct ParseQueryResult {
    pub green: GreenNode,
    pub diagnostics: Vec<Diagnostic>,
}

/// Salsa-tracked entry point for parsing. Re-runs when the upstream
/// [`lex_query`](leek_lexer::pipeline::lex_query) result changes, when
/// any input field this body reads off the
/// [`SourceFile`](leek_pipeline::salsa::SourceFile) changes (`text`,
/// `version_byte`, `flags_bits`; `strict` is *not* read here — it only
/// reaches the type checker), or when it is asked for a different
/// [`ProgramClasses`] set.
///
/// `classes` is a **key**, not a field on the file. The program-wide
/// defined-class set belongs to the program, so one leaf has one parse
/// per program that includes it, and an edit that leaves a program's
/// class set alone re-parses only the file that changed. Assembling the
/// set is `leek_db::queries::program_classes`; a file parsed on its own
/// passes [`ProgramClasses::none`].
///
/// Exactly which of those changes re-run this query is pinned by
/// `program_queries.rs` in `leek-db`'s tests, which watches salsa's
/// event stream: the contract now spans an input, a key and two crates,
/// so it is no longer something this crate can test on its own.
///
/// Deliberately *not* [`crate::parse_file_with`]:
///
/// * it lexes through [`leek_lexer::pipeline::lex_query`] so the memoized
///   lex is shared with the [`Lex`](leek_lexer::pipeline::Lex) step
///   instead of re-lexed here;
/// * it returns the parser's diagnostics *only*, because on this path the
///   `Lex` step already emitted the lexer's. `parse_file_with` prepends
///   them, so routing this query through it would double-report every lex
///   diagnostic.
///
/// This is the only salsa parse entry point, and it serves an indexed
/// on-disk file exactly as it serves an editor buffer: both are one
/// [`SourceFile`](leek_pipeline::salsa::SourceFile), and both reach the
/// parser through a pipeline whose `Lex` step emitted the lex
/// diagnostics once.
#[salsa::tracked]
pub fn parse_query<'db>(
    db: &'db dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
    classes: ProgramClasses<'db>,
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
        classes.names(db),
    );
    ParseQueryResult {
        green: result.green,
        diagnostics: result.diagnostics,
    }
}

/// The two parse paths agree on the tree, and disagree about lex
/// diagnostics only where the [entry module docs](crate::entry) say they
/// should.
#[cfg(test)]
mod parse_path_agreement_tests {
    use leek_diagnostics::codes;
    use leek_pipeline::Pipeline;
    use leek_pipeline::salsa::{LeekDb, ProgramClasses, SourceFile};
    use leek_syntax::version::version_from_byte;

    use super::{Parse, parse_query};

    /// A clean file, and one that needs a program class set to parse its
    /// declaration *and* trips the parser on a second statement — so the
    /// comparison covers both a green tree built from cross-file inputs
    /// and one built through error recovery.
    const CASES: [(&str, &[&str]); 2] = [
        ("class c {}\nc x = new c();\n", &[]),
        ("fromAnotherFile y = 1;\nvar = ;\n", &["fromAnotherFile"]),
    ];

    /// `parse_file_with` and `parse_query` are two doors onto one grammar:
    /// the LSP reaches the tree through the query and `leekc` through the
    /// pure entry point, and a user who saw different syntax errors from
    /// the editor and the compiler would rightly call it a bug.
    #[test]
    fn the_pure_entry_point_and_the_tracked_query_build_the_same_tree() {
        for (text, classes) in CASES {
            let classes: Vec<String> = classes.iter().map(|&c| c.to_string()).collect();
            let db = LeekDb::default();
            let file = SourceFile::new(&db, String::new(), 1, text.into(), 4, false, false, 0);
            let tracked = parse_query(&db, file, ProgramClasses::new(&db, classes.clone()));
            let pure = crate::parse_file_with(
                text,
                file.source(&db),
                &crate::ParseOptions::new(version_from_byte(4)).with_extra_classes(&classes),
            );
            assert_eq!(
                tracked.green, pure.green,
                "same inputs, same tree: {text:?}"
            );
            // Neither fixture lexes badly, so the one documented
            // difference between the two paths doesn't show here.
            assert_eq!(
                tracked.diagnostics, pure.diagnostics,
                "no lex diagnostics to disagree about: {text:?}"
            );
        }
    }

    /// An indexed on-disk file used to have a parse query of its own that
    /// re-lexed and merged the lexer's diagnostics in, because nothing
    /// else reported them for a file with no editor buffer. It is gone,
    /// and this is the guard on what replaced it: the indexed file is an
    /// ordinary [`SourceFile`] driven through the ordinary pipeline, and
    /// the [`Lex`](leek_lexer::pipeline::Lex) step in front of [`Parse`]
    /// emits each lex diagnostic — exactly once, since `parse_query`
    /// still reports none of them itself.
    #[test]
    fn the_indexed_path_still_reports_each_lex_diagnostic_once() {
        let text = "var s = \"unclosed;\n";
        let db = LeekDb::default();
        let file = SourceFile::new(
            &db,
            "/project/a.leek".to_string(),
            1,
            text.into(),
            4,
            false,
            false,
            0,
        );
        let lexed = leek_lexer::lex(text, file.source(&db), version_from_byte(4));
        assert_eq!(
            lexed
                .diagnostics
                .iter()
                .filter(|d| d.code == codes::STRING_NOT_CLOSED)
                .count(),
            1,
            "fixture must raise exactly one lex diagnostic to count"
        );

        let run = Pipeline::new()
            .with(leek_lexer::pipeline::Lex)
            .with(Parse)
            .run_memoized(&db, file);
        assert_eq!(
            run.diagnostics()
                .iter()
                .filter(|d| d.code == codes::STRING_NOT_CLOSED)
                .count(),
            1,
            "the Lex step reports it once, and the parse query not at all"
        );
    }

    /// The counterpart: `parse_query` leaves lex diagnostics to the `Lex`
    /// step, so it must *not* report them itself. Were it routed through
    /// `parse_file_with` the pipeline would show every one of them twice.
    #[test]
    fn the_buffer_query_leaves_lex_diagnostics_to_the_lex_step() {
        let text = "var s = \"unclosed;\n";
        let db = LeekDb::default();
        let file = SourceFile::new(&db, String::new(), 1, text.into(), 4, false, false, 0);
        let out = parse_query(&db, file, ProgramClasses::none(&db));
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| d.code == codes::STRING_NOT_CLOSED),
            "the Lex step owns this one: {:?}",
            out.diagnostics
        );
    }
}
