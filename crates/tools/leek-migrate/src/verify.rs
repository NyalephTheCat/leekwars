//! Post-migration verification.
//!
//! A migration's contract is semantic preservation: source that
//! compiles and runs under the `from` version must, after migration,
//! compile and run identically under the `to` version. Passes rewrite
//! what they know how to rewrite — but plenty of constructs have no
//! equivalent at the target version (classes below v2, map literals
//! below v3, the v4 `map*` builtin family below v4, …) and silently
//! emitting source that no longer compiles is the worst outcome.
//!
//! This module is the safety net: compile the ORIGINAL under `from`
//! and the MIGRATED text under `to` through the full frontend
//! (parser + resolver + HIR lowering). If the original was clean and
//! the migrated text has errors, each one is surfaced as a
//! `MigrationCompileBreak` warning carrying the underlying error.
//! If the original already had errors, verification is skipped —
//! garbage in, garbage out.

use leek_diagnostics::{Diagnostic, Severity, codes};
use leek_pipeline::Input;
use leek_recipes::{RecipeParams, Target};
use leek_span::SourceId;
use leek_syntax::Version;

/// Error diagnostics from compiling `text` to HIR under `version`.
fn compile_errors(text: &str, source_id: SourceId, version: Version) -> Vec<Diagnostic> {
    let input = Input {
        source: source_id,
        text: text.to_string().into(),
        version_byte: crate::version_byte(version),
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    };
    let Ok(pipeline) = leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive()) else {
        return Vec::new();
    };
    pipeline
        .run(input)
        .diagnostics()
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .cloned()
        .collect()
}

/// Compare the original (under `from`) against the migrated text
/// (under `to`); report every error that the migration introduced.
/// Spans point into the MIGRATED text.
pub(crate) fn verify_migration(
    original: &str,
    migrated: &str,
    source_id: SourceId,
    from: Version,
    to: Version,
) -> Vec<Diagnostic> {
    if !compile_errors(original, source_id, from).is_empty() {
        // The original didn't compile cleanly — nothing to preserve.
        return Vec::new();
    }
    compile_errors(migrated, source_id, to)
        .into_iter()
        .map(|e| {
            Diagnostic::warning(
                codes::MIGRATION_COMPILE_BREAK,
                e.span,
                format!(
                    "migrated source no longer compiles at @version:{}: [{}] {}",
                    crate::version_byte(to),
                    e.code.0,
                    e.message,
                ),
            )
            .with_note(
                "this construct has no automatic rewrite for the target version — \
                 migrate it by hand",
            )
        })
        .collect()
}
