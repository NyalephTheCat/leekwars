//! Pipeline integration: linter as a [`Step`].
//!
//! Sequenced after [`leek_hir::pipeline::LowerHir`] (and typically
//! after `TypeCheck` so types are populated). Lint findings are
//! emitted as ordinary [`Diagnostic`]s into the pipeline's
//! diagnostic stream — there's no separate artifact in v0.1.
//!
//! A non-empty [`LintFindings`] artifact is also inserted for
//! consumers that want the findings as data (e.g. the LSP's code-
//! action provider, once that lands).

use leek_diagnostics::Diagnostic;
use leek_hir::pipeline::HirArtifact;
use leek_parser::pipeline::GreenTreeArtifact;
use leek_pipeline::{Artifact, Context, RecipeArtifact, RecipeParams, RecipeStep, Step, StepError};
use leek_syntax::SyntaxNode;

/// Optional artifact: the raw list of lint findings, in case a
/// caller wants them as data rather than as diagnostics.
#[derive(Debug, Clone, Default)]
pub struct LintFindings(pub Vec<Diagnostic>);
impl Artifact for LintFindings {}

/// Lint pipeline step. Carries the opt-in groups requested by the
/// recipe ([`RecipeParams::lints`]); the language version is read
/// from the [`Context`] at run time.
pub struct Lint {
    pedantic: bool,
    nursery: bool,
}

impl RecipeStep for Lint {
    fn build(params: &RecipeParams) -> Box<dyn leek_pipeline::Step> {
        Box::new(Lint {
            pedantic: params.lints.pedantic,
            nursery: params.lints.nursery,
        })
    }
}

impl RecipeArtifact for LintFindings {
    type Producer = Lint;
    type Requires = (HirArtifact,);
    type Produces = (LintFindings,);
}

impl Step for Lint {
    fn name(&self) -> &'static str {
        "lint"
    }

    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let Some(hir) = cx.get::<HirArtifact>() else {
            // No HIR (parse failed) — nothing to lint. Not an error.
            return Ok(());
        };
        let opts = crate::LintOptions {
            pedantic: self.pedantic,
            nursery: self.nursery,
            version: cx.version_byte(),
        };
        // `// @allow(LXXXX)` suppression needs the green tree. The lint
        // step runs after Parse so the artifact is normally present; a
        // pipeline wired without it just gets no suppression.
        let root = cx
            .get::<GreenTreeArtifact>()
            .map(|green| SyntaxNode::new_root(green.0.clone()));
        let findings = crate::lint_file(hir.0.as_ref(), root.as_ref(), &opts);

        cx.emit_all(findings.iter().cloned());
        cx.insert(LintFindings(findings));
        Ok(())
    }
}
