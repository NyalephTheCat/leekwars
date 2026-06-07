//! v2 → v3 migration.
//!
//! Currently a pure pragma bump: no syntactic rewrites are
//! required. v3 introduces first-class builtins as values
//! (`var f = cos`), but v2 sources that never used that pattern
//! transition cleanly. Lexer-level changes between v2 and v3
//! (the v1-only `/*/` quirk, the v1-only `function` alias, etc.)
//! don't bite a v2 source.
//!
//! We keep the pass as a structured no-op so chained migrations
//! flow through it cleanly and the pragma update fires.

use leek_diagnostics::Diagnostic;
use leek_rewrite::EditSet;
use leek_span::SourceId;
use leek_syntax::Version;

use crate::MigrationPass;

pub struct V2ToV3;

impl MigrationPass for V2ToV3 {
    fn name(&self) -> &'static str {
        "v2-to-v3"
    }
    fn from_version(&self) -> Version {
        Version::V2
    }
    fn to_version(&self) -> Version {
        Version::V3
    }
    fn collect_edits(
        &self,
        _source: &str,
        _source_id: SourceId,
        _edits: &mut EditSet,
        _diagnostics: &mut Vec<Diagnostic>,
    ) {
        // No source-level rewrites needed today; the pragma update
        // wrapping this pass handles `@version:2` → `@version:3`.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_pass;

    #[test]
    fn body_unchanged_pragma_bumped() {
        let out = run_pass(
            &V2ToV3,
            "// @version:2\nvar x = 1\n",
            SourceId::new(1).unwrap(),
        );
        assert_eq!(out.text, "// @version:3\nvar x = 1\n");
    }
}
