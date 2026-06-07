//! Source-level migration passes between Leekscript language
//! versions.
//!
//! Each [`MigrationPass`] is a single-step transformation from one
//! [`Version`] to the next. The passes are deliberately CST-
//! anchored: they parse with the source version's grammar, walk
//! for known patterns, push targeted [`leek_rewrite::EditSet`]
//! entries, and apply them. Everything outside the rewritten
//! tokens — whitespace, comments, doc-comments, code that doesn't
//! match the pattern — is preserved byte-for-byte.
//!
//! Chain multiple passes via [`migrate_text`] to bridge several
//! versions: e.g. `migrate_text(v1_source, V1, V4)` applies
//! v1→v2, then v2→v3, then v3→v4 in sequence, re-parsing between
//! each so the next pass sees the post-migration syntax.
//!
//! Each pass also keeps the `// @version:N` pragma in sync. If
//! one is present it's updated to the target version; if absent,
//! a top-of-file `// @version:N` is inserted.

pub mod passes;
mod pragma;

use leek_diagnostics::Diagnostic;
use leek_rewrite::EditSet;
use leek_span::SourceId;
use leek_syntax::Version;

pub use passes::{V1ToV2, V2ToV1, V2ToV3, V3ToV2, V3ToV4, V4ToV3};

/// One-step migration outcome.
pub struct MigrationOutput {
    /// The post-migration source. Identical to the input outside
    /// the regions a pass rewrote.
    pub text: String,
    /// Diagnostics raised by the pass (warnings about patterns it
    /// couldn't fully migrate, etc.). Errors here are advisory —
    /// the migration still produced `text`.
    pub diagnostics: Vec<Diagnostic>,
}

/// A single version-to-version migration.
///
/// Implementors walk a CST under [`MigrationPass::from_version`]
/// and contribute byte-range edits to the [`EditSet`]. The
/// trait's framework (see [`run_pass`]) handles parsing, applying
/// the edits, and updating the `// @version` pragma.
pub trait MigrationPass: Sync {
    fn name(&self) -> &'static str;
    // Paired with `to_version`; the `from_*` name is intentional API, not a conversion ctor.
    #[allow(clippy::wrong_self_convention)]
    fn from_version(&self) -> Version;
    fn to_version(&self) -> Version;

    /// Walk the source's CST and contribute edits.
    fn collect_edits(
        &self,
        source: &str,
        source_id: SourceId,
        edits: &mut EditSet,
        diagnostics: &mut Vec<Diagnostic>,
    );
}

/// Run a single pass: parse, collect edits, apply, then update
/// the `// @version:N` pragma so the result advertises its new
/// target version.
///
/// Always returns a [`MigrationOutput`]; if the pass contributes
/// no edits and no pragma is present, the result text equals the
/// input.
pub fn run_pass(pass: &dyn MigrationPass, source: &str, source_id: SourceId) -> MigrationOutput {
    let mut edits = EditSet::new(source.len());
    let mut diagnostics = Vec::new();
    pass.collect_edits(source, source_id, &mut edits, &mut diagnostics);
    let after_pass = edits.apply(source);

    // Tack on the pragma update as a separate edit round, so the
    // pass body doesn't need to know where the `@version` line
    // lives.
    let pragma_updated = pragma::set_version(&after_pass, pass.to_version());
    MigrationOutput {
        text: pragma_updated,
        diagnostics,
    }
}

/// Chain every pass that brings `from` to `to`, in either
/// direction. Re-parses between passes so each one sees the
/// surface its `from_version` expects.
///
/// Upgrade chain: V1 → V2 → V3 → V4.
/// Downgrade chain: V4 → V3 → V2 → V1.
///
/// Same-version input is a no-op apart from normalizing the
/// `// @version:N` pragma.
pub fn migrate_text(
    source: &str,
    source_id: SourceId,
    from: Version,
    to: Version,
) -> MigrationOutput {
    use std::cmp::Ordering;
    let direction = match version_byte(from).cmp(&version_byte(to)) {
        Ordering::Equal => {
            let text = pragma::set_version(source, to);
            return MigrationOutput {
                text,
                diagnostics: Vec::new(),
            };
        }
        Ordering::Less => Direction::Upgrade,
        Ordering::Greater => Direction::Downgrade,
    };

    let mut current = source.to_string();
    let mut diagnostics = Vec::new();
    let mut here = from;
    while version_byte(here) != version_byte(to) {
        let pass = pass_for(here, direction);
        let MigrationOutput {
            text,
            diagnostics: pass_diags,
        } = run_pass(pass.as_ref(), &current, source_id);
        current = text;
        diagnostics.extend(pass_diags);
        here = pass.to_version();
    }
    MigrationOutput {
        text: current,
        diagnostics,
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Upgrade,
    Downgrade,
}

fn pass_for(from: Version, direction: Direction) -> Box<dyn MigrationPass> {
    match (from, direction) {
        (Version::V1, Direction::Upgrade) => Box::new(passes::V1ToV2),
        (Version::V2, Direction::Upgrade) => Box::new(passes::V2ToV3),
        (Version::V3, Direction::Upgrade) => Box::new(passes::V3ToV4),
        (Version::V4, Direction::Upgrade) => unreachable!("nothing above V4"),
        (Version::V4, Direction::Downgrade) => Box::new(passes::V4ToV3),
        (Version::V3, Direction::Downgrade) => Box::new(passes::V3ToV2),
        (Version::V2, Direction::Downgrade) => Box::new(passes::V2ToV1),
        (Version::V1, Direction::Downgrade) => unreachable!("nothing below V1"),
    }
}

fn version_byte(v: Version) -> u8 {
    match v {
        Version::V1 => 1,
        Version::V2 => 2,
        Version::V3 => 3,
        Version::V4 => 4,
    }
}
