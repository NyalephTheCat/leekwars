//! Pipeline integration: parser as a [`Step`].
//!
//! Three parse paths live here — the direct [`Parse`] step, [`parse_query`]
//! and [`parse_project_file_query`] — and they differ only in *who lexed
//! the text*, never in how the resulting diagnostics are ordered. A path
//! that lexes its own text goes through [`crate::parse_file_with`] and
//! reports the lexer's diagnostics ahead of the parser's; a path handed
//! tokens somebody else lexed reports the parser's only, because that
//! somebody already emitted the lexer's. The [entry module
//! docs](crate::entry) state the convention in full.

use leek_diagnostics::Diagnostic;
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStepStopOnError};
use leek_syntax::SyntaxNode;
use leek_syntax::language::GreenNode;
use leek_syntax::version::version_from_byte;

use crate::ast::{AstNode, SourceFile};
use crate::parse_tokens_with_classes;

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
///
/// Deliberately *not* [`crate::parse_file_with`], and deliberately
/// asymmetric with [`parse_project_file_query`] below:
///
/// * it lexes through [`leek_lexer::pipeline::lex_query`] so the memoized
///   lex is shared with the [`Lex`](leek_lexer::pipeline::Lex) step
///   instead of re-lexed here;
/// * it returns the parser's diagnostics *only*, because on this path the
///   `Lex` step already emitted the lexer's. `parse_file_with` prepends
///   them, so routing this query through it would double-report every lex
///   diagnostic. `parse_project_file_query` *does* merge them because
///   nothing else emits them for an on-disk project file.
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
///
/// Unlike [`parse_query`] this goes through [`crate::parse_file_with`],
/// so its diagnostics are the lexer's followed by the parser's: an
/// indexed on-disk file has no [`Lex`](leek_lexer::pipeline::Lex) step
/// running over it, so nothing else would report the lexer's. Each lex
/// diagnostic therefore appears exactly once here, not twice.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn parse_project_file_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::ProjectFile,
) -> ParseQueryResult {
    let parsed = crate::parse_file_with(
        file.text(db),
        file.source(db),
        &crate::ParseOptions::new(version_from_byte(file.version_byte(db)))
            .with_flags(leek_span::FeatureFlags::from_bits(file.flags_bits(db)))
            .with_extra_classes(file.extra_classes(db)),
    );
    ParseQueryResult {
        green: parsed.green,
        diagnostics: parsed.diagnostics,
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
        let file = SourceFile::new(&db, 1, SRC.to_string(), 4, false, false, 0, Vec::new());
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

/// The three parse paths agree on the tree, and disagree about lex
/// diagnostics only where the [entry module docs](crate::entry) say they
/// should.
#[cfg(all(test, feature = "salsa"))]
mod parse_path_agreement_tests {
    use leek_diagnostics::codes;
    use leek_pipeline::salsa::{LeekDb, ProjectFile, SourceFile};
    use leek_syntax::version::version_from_byte;

    use super::salsa_probe::SERIAL;
    use super::{parse_project_file_query, parse_query};

    /// A clean file, and one that needs `extra_classes` to parse its
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
        // `parse_query` bumps the probe counter the invalidation tests
        // read deltas off.
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (text, classes) in CASES {
            let classes: Vec<String> = classes.iter().map(|&c| c.to_string()).collect();
            let db = LeekDb::default();
            let file = SourceFile::new(
                &db,
                1,
                text.to_string(),
                4,
                false,
                false,
                0,
                classes.clone(),
            );
            let tracked = parse_query(&db, file);
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

    /// Nothing lexes an indexed on-disk file but
    /// `parse_project_file_query` itself, so it is the one salsa path that
    /// merges the lexer's diagnostics in — and it must merge them *once*.
    /// Two copies of the lex+merge glue on one path is how a file ends up
    /// with two identical "string literal not closed" squiggles.
    #[test]
    fn the_project_file_query_reports_each_lex_diagnostic_once() {
        let text = "var s = \"unclosed;\n";
        let db = LeekDb::default();
        let file = ProjectFile::new(
            &db,
            "/project/a.leek".to_string(),
            1,
            text.to_string(),
            4,
            false,
            false,
            0,
            Vec::new(),
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

        let out = parse_project_file_query(&db, file);
        assert_eq!(
            out.diagnostics
                .iter()
                .filter(|d| d.code == codes::STRING_NOT_CLOSED)
                .count(),
            1,
            "the lexer's diagnostic is merged in once, not once per path"
        );
        assert_eq!(
            out.diagnostics.first().map(|d| d.code),
            Some(codes::STRING_NOT_CLOSED),
            "lex diagnostics come before the parser's"
        );
    }

    /// The counterpart: `parse_query` leaves lex diagnostics to the `Lex`
    /// step, so it must *not* report them itself. Were it routed through
    /// `parse_file_with` the pipeline would show every one of them twice.
    #[test]
    fn the_buffer_query_leaves_lex_diagnostics_to_the_lex_step() {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = "var s = \"unclosed;\n";
        let db = LeekDb::default();
        let file = SourceFile::new(&db, 1, text.to_string(), 4, false, false, 0, Vec::new());
        let out = parse_query(&db, file);
        assert!(
            !out.diagnostics
                .iter()
                .any(|d| d.code == codes::STRING_NOT_CLOSED),
            "the Lex step owns this one: {:?}",
            out.diagnostics
        );
    }
}
