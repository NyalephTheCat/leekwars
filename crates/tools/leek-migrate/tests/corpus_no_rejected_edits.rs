//! Corpus guard: no migration pass may give up on a rewrite.
//!
//! A `MigrationSkipped` (W0513) warning means a pass worked out how
//! to rewrite a site and then could not apply the edit, because
//! another rewrite of the same pass had already claimed those bytes.
//! The result still compiles; it just isn't migrated. Over the
//! upstream corpus that must never happen — a rejection here is a
//! pass-ordering bug to fix, not a warning to ship.
//!
//! Adjacent passes only: a longer migration (`v1 → v4`) is those
//! same passes chained, so it exercises no extra rewrite code. The
//! full cross-product, with runtime comparison, lives in
//! `examples/corpus_verify.rs`, which fails on these warnings too.

use leek_diagnostics::codes;
use leek_migrate::{MigrationPass, V1ToV2, V2ToV1, V2ToV3, V3ToV2, V3ToV4, V4ToV3, run_pass};
use leek_span::SourceId;
use leek_test_corpus::embedded_manifest;

/// The passes that read version `byte` as their source version.
fn passes_from(byte: u8) -> &'static [&'static (dyn MigrationPass + Sync)] {
    match byte {
        1 => &[&V1ToV2],
        2 => &[&V2ToV3, &V2ToV1],
        3 => &[&V3ToV4, &V3ToV2],
        4 => &[&V4ToV3],
        _ => &[],
    }
}

#[test]
fn no_corpus_case_has_a_rejected_edit() {
    let mut rejected: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for case in &embedded_manifest().cases {
        for pass in passes_from(case.version) {
            checked += 1;
            let out = run_pass(*pass, &case.code, SourceId::new(1).unwrap());
            for d in out
                .diagnostics
                .iter()
                .filter(|d| d.code == codes::MIGRATION_SKIPPED)
            {
                if rejected.len() < 20 {
                    rejected.push(format!("{} [{}]: {}", case.id, pass.name(), d.message));
                }
            }
        }
    }
    assert!(checked > 0, "corpus manifest is empty");
    assert!(
        rejected.is_empty(),
        "{} migrations dropped a rewrite:\n{}",
        rejected.len(),
        rejected.join("\n")
    );
}
