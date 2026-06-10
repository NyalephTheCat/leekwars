//! Pipeline integration: type checker as a [`Step`].

use leek_diagnostics::Diagnostic;
use leek_parser::pipeline::AstArtifact;
use leek_pipeline::{Artifact, Context, Step, StepError};
use leek_pipeline::{RecipeArtifact, RecipeParams, RecipeStep};
use leek_syntax::pipeline::version_from_byte;

use crate::index::{InferredSignatures, TypeTable};
use crate::{Options, TypeCheckResult, check_collecting};

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
    #[cfg(feature = "salsa")]
    if let Some((db, file)) = cx.salsa() {
        let art = typecheck_query(db, file);
        return TypeCheckResult {
            diagnostics: art.diagnostics,
            table: art.table,
            signatures: art.signatures,
        };
    }
    let Some(ast) = cx.get::<AstArtifact>().and_then(|a| a.0.clone()) else {
        return TypeCheckResult::default();
    };
    let opts = Options {
        strict: cx.strict(),
        experimental_generics: cx.flags().generics,
        seed_library: crate::seed_library_enabled(),
        experimental_prelude: cx.flags().prelude,
    };
    check_collecting(
        &ast,
        cx.source(),
        version_from_byte(cx.version_byte()),
        opts,
    )
}

/// Salsa-tracked entry point for type checking. Re-runs only when the
/// upstream [`parse_query`](leek_parser::pipeline::parse_query)'s
/// green tree changes or the `strict` flag flips.
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
