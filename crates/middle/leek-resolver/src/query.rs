//! Name resolution as a tracked query.

use leek_diagnostics::Diagnostic;
use leek_syntax::version::version_from_byte;

use crate::index::ResolveTable;
use crate::{Options, ResolveResult, resolve_collecting};

/// Resolver outcome.
///
/// Carries both the diagnostic list and the LSP-facing
/// [`ResolveTable`] of symbols + references. Direct callers that
/// only need diagnostics ignore `table`.
#[derive(salsa::Update, Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveArtifact {
    pub diagnostics: Vec<Diagnostic>,
    pub table: ResolveTable,
}

/// Salsa-tracked entry point for name resolution. Re-runs only when
/// the upstream [`parse_query`](leek_parser::query::parse_query)'s
/// green tree changes or the `strict` flag flips.
#[salsa::tracked]
pub fn resolve_query(
    db: &dyn leek_query::salsa::Db,
    file: leek_query::salsa::SourceFile,
) -> ResolveArtifact {
    use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
    use leek_query::salsa::ProgramClasses;
    use leek_syntax::SyntaxNode;

    let parse = leek_parser::query::parse_query(db, file, ProgramClasses::none(db));
    let Some(ast) = AstSourceFile::cast(SyntaxNode::new_root(parse.green.clone())) else {
        return ResolveArtifact::default();
    };
    // Pragmas only contribute experimental opt-ins here; the version and
    // strict mode come from the salsa input (the settled `Input`). Reuse the
    // memoized pragma query instead of re-scanning the text.
    let pragmas = leek_syntax::query::pragma_query(db, file).pragmas;
    // The dynamically-registered builtins are still a process-global, and
    // this reads it — untracked — from inside a tracked query. What changed
    // is the *frequency*: one read here, at the top of the query body,
    // instead of one lock per unresolved name during the walk. A later slice
    // takes the registry off the salsa input instead, so registering a
    // builtin invalidates the memo rather than being silently missed by it.
    let builtins = crate::builtins::snapshot_dynamic_builtins();
    let opts = Options::from_settings(
        Some(&pragmas),
        leek_span::FeatureFlags::from_bits(file.flags_bits(db)),
        file.strict(db),
    )
    .with_builtins(builtins);
    let ResolveResult { diagnostics, table } = resolve_collecting(
        &ast,
        file.source(db),
        version_from_byte(file.version_byte(db)),
        opts,
    );
    ResolveArtifact { diagnostics, table }
}
