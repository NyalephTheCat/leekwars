//! v3 → v2 downgrade — no syntactic divergence between v2 and v3
//! is in scope today, so this pass exists purely to keep the chain
//! traversable and stamp the `@version:2` pragma.
//!
//! Mirrors [`super::v2_to_v3`].

use leek_diagnostics::Diagnostic;
use leek_rewrite::EditSet;
use leek_span::SourceId;
use leek_syntax::Version;

use crate::MigrationPass;

pub struct V3ToV2;

impl MigrationPass for V3ToV2 {
    fn name(&self) -> &'static str {
        "v3-to-v2"
    }
    fn from_version(&self) -> Version {
        Version::V3
    }
    fn to_version(&self) -> Version {
        Version::V2
    }
    fn collect_edits(
        &self,
        _source: &str,
        _source_id: SourceId,
        _edits: &mut EditSet,
        _diagnostics: &mut Vec<Diagnostic>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_pass;

    #[test]
    fn body_unchanged_pragma_bumped() {
        let src = "// @version:3\nvar x = 1\nreturn x\n";
        let out = run_pass(&V3ToV2, src, SourceId::new(1).unwrap()).text;
        assert!(out.starts_with("// @version:2\n"), "got: {out}");
        assert!(out.contains("var x = 1"));
        assert!(out.contains("return x"));
    }
}
