//! Pipeline integration: type checker as a [`Step`].

use leek_diagnostics::Diagnostic;
use leek_parser::pipeline::AstArtifact;
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_syntax::version::version_from_byte;

use crate::index::{InferredSignatures, TypeTable};
use crate::{Options, TypeCheckResult, check_collecting, check_collecting_files};

/// Type-check outcome.
///
/// Carries both the diagnostic list and the LSP-facing
/// [`TypeTable`]. Direct callers that only need diagnostics ignore
/// `table`.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeCheckArtifact {
    pub diagnostics: Vec<Diagnostic>,
    pub table: TypeTable,
    /// Declared/inferred function-return and class-member types for
    /// signature rendering (see [`InferredSignatures`]).
    pub signatures: InferredSignatures,
}
impl Artifact for TypeCheckArtifact {}

/// Type checker step. Reads the AST contributed by
/// [`leek_parser::pipeline::Parse`].
pub struct TypeCheck;

impl Step for TypeCheck {
    fn name(&self) -> &'static str {
        "type-check"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let TypeCheckResult {
            diagnostics,
            table,
            signatures,
        } = run_typecheck(cx);
        cx.emit_all(diagnostics.iter().cloned());
        cx.insert(TypeCheckArtifact {
            diagnostics,
            table,
            signatures,
        });
        Ok(())
    }
}

impl RecipeStep for TypeCheck {
    fn build(_: &RecipeParams) -> Box<dyn leek_pipeline::Step> {
        Box::new(TypeCheck)
    }
}

impl RecipeArtifact for TypeCheckArtifact {
    type Producer = TypeCheck;
    type Requires = (leek_resolver::pipeline::ResolveArtifact,);
    type Produces = (TypeCheckArtifact,);
}

/// Salsa-aware type-check driver.
fn run_typecheck(cx: &Context<'_>) -> TypeCheckResult {
    // The include-aware resolver has already parsed the closure and assigned
    // each file a source id. Type-check those ASTs together instead of
    // entering the single-file salsa query.
    if let Some(graph) = cx.get::<leek_resolver::pipeline::IncludeGraphArtifact>()
        && !graph.includes.is_empty()
        && let Some(entry) = cx.get::<AstArtifact>().map(|a| &a.0)
    {
        let mut files: Vec<leek_resolver::FileUnit<'_>> = graph
            .includes
            .iter()
            .map(|file| leek_resolver::FileUnit {
                ast: &file.ast,
                source: file.source,
                version: file.version,
                path: &file.path,
            })
            .collect();
        files.push(leek_resolver::FileUnit {
            ast: entry,
            source: cx.source(),
            version: version_from_byte(cx.version_byte()),
            path: &graph.entry_path,
        });
        return check_collecting_files(&files, Some(&graph.resolved), type_options(cx));
    }
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        let art = typecheck_query(db, file);
        return TypeCheckResult {
            diagnostics: art.diagnostics,
            table: art.table,
            signatures: art.signatures,
        };
    }
    let Some(ast) = cx.get::<AstArtifact>().map(|a| a.0.clone()) else {
        return TypeCheckResult::default();
    };
    check_collecting(
        &ast,
        cx.source(),
        version_from_byte(cx.version_byte()),
        type_options(cx),
    )
}

fn type_options(cx: &Context<'_>) -> Options {
    Options {
        strict: cx.strict(),
        experimental_generics: cx.flags().generics,
        seed_library: crate::seed_library_enabled(),
        experimental_prelude: cx.flags().prelude,
        experimental_types: cx.flags().types,
        experimental_interfaces: cx.flags().interfaces,
        experimental_enums: cx.flags().enums,
    }
}

/// Salsa-tracked entry point for type checking. Re-runs when the upstream
/// [`parse_query`](leek_parser::pipeline::parse_query)'s green tree
/// changes, or when an input field this body reads off the
/// [`SourceFile`](leek_pipeline::salsa::SourceFile) changes: `strict`,
/// `flags_bits`, `source_id` or `version_byte`.
#[cfg(feature = "salsa")]
#[salsa::tracked]
pub fn typecheck_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
) -> TypeCheckArtifact {
    use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
    use leek_syntax::SyntaxNode;

    #[cfg(test)]
    crate::salsa_probe::TYPECHECK_QUERY_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let parse = leek_parser::pipeline::parse_query(db, file);
    let Some(ast) = AstSourceFile::cast(SyntaxNode::new_root(parse.green.clone())) else {
        return TypeCheckArtifact::default();
    };
    let flags = leek_pipeline::FeatureFlags::from_bits(file.flags_bits(db));
    let opts = Options {
        strict: file.strict(db),
        experimental_generics: flags.generics,
        seed_library: crate::seed_library_enabled(),
        experimental_prelude: flags.prelude,
        experimental_types: flags.types,
        experimental_interfaces: flags.interfaces,
        experimental_enums: flags.enums,
    };
    let TypeCheckResult {
        diagnostics,
        table,
        signatures,
    } = check_collecting(
        &ast,
        file.source(db),
        version_from_byte(file.version_byte(db)),
        opts,
    );
    TypeCheckArtifact {
        diagnostics,
        table,
        signatures,
    }
}

/// Which [`SourceFile`](leek_pipeline::salsa::SourceFile) inputs
/// [`typecheck_query`] depends on.
///
/// The interesting cases are the ones the parser is indifferent to:
/// `strict` and the experimental `flags_bits` change what type-checking
/// *means* without changing a single token, so the green tree comes back
/// backdated and unchanged while this query still has to re-run. Getting
/// that wrong leaves the LSP showing yesterday's diagnostics after a
/// `// @strict` pragma is added.
#[cfg(all(test, feature = "salsa"))]
mod salsa_invalidation_tests {
    use std::sync::atomic::Ordering;

    use leek_parser::pipeline::Parse;
    use leek_pipeline::Pipeline;
    use leek_pipeline::salsa::{LeekDb, SourceFile};
    use salsa::Setter;

    use super::TypeCheck;
    use crate::salsa_probe::{SERIAL, TYPECHECK_QUERY_CALLS};

    const SRC: &str = "var x = 5;\nreturn x + 1;\n";

    /// Prime the cache, apply `edit`, run again, and report how many times
    /// `typecheck_query` executed on the second run.
    fn reruns_after(edit: impl FnOnce(&mut LeekDb, SourceFile)) -> usize {
        let _guard = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut db = LeekDb::default();
        let file = SourceFile::new(&db, 1, SRC.to_string(), 4, false, 0, Vec::new());
        // No `Resolve` step: without an `IncludeGraphArtifact` in the
        // context `TypeCheck` takes the single-file salsa branch, which is
        // the one under test.
        let pipeline = Pipeline::new().with(Parse).with(TypeCheck);

        let before = TYPECHECK_QUERY_CALLS.load(Ordering::Relaxed);
        let _ = pipeline.run_memoized(&db, file);
        let primed = TYPECHECK_QUERY_CALLS.load(Ordering::Relaxed);
        assert_eq!(primed - before, 1, "the first run must execute the query");

        edit(&mut db, file);

        let _ = pipeline.run_memoized(&db, file);
        TYPECHECK_QUERY_CALLS.load(Ordering::Relaxed) - primed
    }

    #[test]
    fn an_untouched_input_reuses_the_cached_type_check() {
        assert_eq!(reruns_after(|_, _| {}), 0);
    }

    #[test]
    fn flipping_strict_rechecks_even_though_the_tree_is_unchanged() {
        assert_eq!(
            reruns_after(|db, file| {
                file.set_strict(db).to(true);
            }),
            1,
            "`strict` is read straight off the input, not through the parse"
        );
    }

    #[test]
    fn changing_the_experimental_feature_flags_rechecks() {
        assert_eq!(
            reruns_after(|db, file| {
                file.set_flags_bits(db).to(0b1111_1111);
            }),
            1,
            "the experimental flags select the inference rules"
        );
    }

    #[test]
    fn an_edit_that_changes_no_token_does_not_recheck() {
        // Re-setting `text` to the same string bumps the revision, so the
        // parse re-runs — but its green tree is unchanged, salsa backdates
        // it, and type checking is skipped. This is the whole point of
        // routing the type checker through `parse_query` rather than
        // keying it on the raw text.
        assert_eq!(
            reruns_after(|db, file| {
                file.set_text(db).to(SRC.to_string());
            }),
            0
        );
    }

    #[test]
    fn a_semantic_edit_rechecks() {
        assert_eq!(
            reruns_after(|db, file| {
                file.set_text(db)
                    .to("var y = \"s\";\nreturn y + 1;\n".to_string());
            }),
            1
        );
    }
}
