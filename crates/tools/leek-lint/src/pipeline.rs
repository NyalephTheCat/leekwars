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

// ---- Salsa-tracked entry points ----

/// Salsa-tracked linter entry point: the findings for one file, with
/// `// @allow(LXXXX)` suppression already applied.
///
/// Everything [`Lint`] does apart from reading and writing a
/// [`Context`], over memoized inputs — so the LSP, which asks for a
/// file's lints on every keystroke and again for every code action,
/// pays for one traversal per edit instead of one per request.
///
/// Re-runs when the file's HIR changes or when its green tree does.
/// Both: the HIR is what the passes walk, and the tree is where the
/// `@allow` annotations live — they sit in comment trivia the HIR does
/// not carry, so a keystroke that only adds an `@allow` comment leaves
/// the HIR equal and must still re-suppress.
///
/// The version comes off the file input rather than out of the key; see
/// [`LintOptions::from_groups`](crate::LintOptions::from_groups) for why that
/// is not the same thing as keying on a whole `LintOptions`.
#[salsa::tracked]
pub fn lint_query(
    db: &dyn leek_pipeline::salsa::Db,
    file: leek_pipeline::salsa::SourceFile,
    groups: crate::LintGroups,
) -> std::sync::Arc<Vec<Diagnostic>> {
    use leek_pipeline::salsa::ProgramClasses;

    let hir = leek_hir::pipeline::lower_hir_query(db, file);
    // The same empty class set every other single-file query parses
    // under, so this reads the memo they filled rather than opening a
    // second one. `@allow` scanning does not depend on the class set.
    let green = leek_parser::pipeline::parse_query(db, file, ProgramClasses::none(db)).green;
    let root = SyntaxNode::new_root(green);
    let opts = crate::LintOptions::from_groups(groups, file.version_byte(db));
    std::sync::Arc::new(crate::lint_file(&hir.hir, Some(&root), &opts))
}

/// One file's complete diagnostic stream: everything the compiler
/// frontend found, then the lints.
///
/// This is the half of the ordering rule that could not live in
/// `leek-db`. That crate is `crates/db` (layer rank 3) and this one is
/// `crates/tools` (rank 6), so `leek-db` may not call into the linter —
/// `cargo xtask check-layers` rejects the edge, and there is no
/// allowlist entry for it. The split is therefore:
/// [`leek_db::diagnostics::diagnostics_without_lints`] produces the
/// pragma → lex → parse → resolve → typecheck → HIR → MIR sequence, and
/// this function, in the tool, depending **down** on `leek-db`, appends
/// [`lint_query`]'s findings. Every edge points downward and exactly one
/// function ever does the appending, so the order still lives in one
/// place.
///
/// Lints last, which is where a recipe puts them: `LintFindings`
/// requires `HirArtifact`, so the `Lint` step is planned after every
/// stage above and emits into the run's diagnostic stream after all of
/// them.
#[salsa::tracked]
pub fn diagnostics_with_lints(
    db: &dyn leek_db::Db,
    file: leek_db::SourceFile,
    groups: crate::LintGroups,
) -> std::sync::Arc<Vec<Diagnostic>> {
    let mut out = leek_db::queries::diagnostics_without_lints(db, file)
        .as_ref()
        .clone();
    out.extend(lint_query(db, file, groups).as_ref().iter().cloned());
    std::sync::Arc::new(out)
}

/// One *program*'s lint findings: the whole include closure's merged HIR,
/// linted once.
///
/// Deliberately not [`lint_query`] per file of the closure, because that
/// is not what the pipeline does and this has to match it. The `Lint`
/// step reads whatever `HirArtifact` the run produced, which on the
/// include-aware path is the *merged* program HIR — every included file's
/// functions, classes and globals folded into the entry's tree — and it
/// reads `GreenTreeArtifact`, which is the **entry's** tree alone. So a
/// lint fires once for the program, and an `// @allow(LXXXX)` comment
/// suppresses it only when it sits in the entry file. Linting each file
/// separately would report a finding per file that declares the
/// construct, and would honour an `@allow` inside an include; both are
/// behaviour changes wearing a refactor's clothes.
///
/// The version comes off the entry's own input rather than out of
/// `entry_version`, matching `Context::version_byte` — drivers settle the
/// language version onto the input before anything runs, so the two agree,
/// and following the input is what keeps this identical to the step if
/// they ever stop agreeing.
#[salsa::tracked]
pub fn program_lint_query(
    db: &dyn leek_db::Db,
    files: leek_db::WorkspaceFiles,
    entry: leek_db::SourceFile,
    entry_version: leek_syntax::Version,
    groups: crate::LintGroups,
) -> std::sync::Arc<Vec<Diagnostic>> {
    let hir = leek_db::queries::lower_program(
        db,
        files,
        entry,
        entry_version,
        leek_pipeline::OptLevel::O0,
    );
    // The program's own parse key, so this reads the memo the whole-program
    // passes filled rather than opening a second one under an empty set.
    let classes = leek_db::queries::program_classes(db, files, entry, entry_version);
    let green = leek_db::queries::parse_query(db, entry, classes).green;
    let root = SyntaxNode::new_root(green);
    let opts = crate::LintOptions::from_groups(groups, entry.version_byte(db));
    std::sync::Arc::new(crate::lint_file(&hir.hir, Some(&root), &opts))
}

/// One program's complete diagnostic stream: everything the compiler
/// frontend found across the include closure, then the lints.
///
/// The include-aware counterpart of [`diagnostics_with_lints`], and the
/// same layering argument puts it here rather than in `leek-db`:
/// [`leek_db::queries::program_diagnostics`] produces the frontend's
/// stream and this crate, depending **down**, appends the findings.
///
/// A consumer wanting one file's slice filters with
/// [`leek_db::queries::for_source`]; a program stream reports the whole
/// program, so a type error inside an included file is raised against
/// *that* file's `SourceId`.
#[salsa::tracked]
pub fn program_diagnostics_with_lints(
    db: &dyn leek_db::Db,
    files: leek_db::WorkspaceFiles,
    entry: leek_db::SourceFile,
    entry_version: leek_syntax::Version,
    groups: crate::LintGroups,
) -> std::sync::Arc<Vec<Diagnostic>> {
    let mut out = leek_db::queries::program_diagnostics(db, files, entry, entry_version)
        .as_ref()
        .clone();
    out.extend(
        program_lint_query(db, files, entry, entry_version, groups)
            .as_ref()
            .iter()
            .cloned(),
    );
    std::sync::Arc::new(out)
}
