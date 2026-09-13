//! Rewrites a pass cannot apply must be reported, never dropped.
//!
//! Two rewrites of one pass can land on the same bytes — a
//! `subArray`/`arraySlice` call whose end argument is itself
//! rewritten swallows every rewrite inside it. The edit set rejects
//! the inner ones; a pass that ignores the rejection emits source
//! that still compiles but no longer means what it did, with nothing
//! to tell the user which site to check. Every rejection is a
//! `MigrationSkipped` (W0513) warning at the site instead.

use leek_diagnostics::{Diagnostic, codes};
use leek_migrate::{V3ToV4, V4ToV3, run_pass};
use leek_span::SourceId;

fn id() -> SourceId {
    SourceId::new(1).unwrap()
}

fn skipped(diagnostics: &[Diagnostic]) -> Vec<&Diagnostic> {
    diagnostics
        .iter()
        .filter(|d| d.code == codes::MIGRATION_SKIPPED)
        .collect()
}

/// The inner `subArray` sits inside the text the outer call's end
/// argument is rewritten with, so it cannot be renamed too.
#[test]
fn nested_sub_array_rewrite_is_reported_not_dropped() {
    let out = run_pass(&V3ToV4, "var s = subArray(a, 0, subArray(b, 0, 1))\n", id());
    // The outer call migrated; the inner one is untouched.
    assert!(
        out.text
            .contains("arraySlice(a, 0, (subArray(b, 0, 1)) + 1)"),
        "got: {}",
        out.text
    );
    let skipped = skipped(&out.diagnostics);
    assert_eq!(skipped.len(), 1, "diagnostics: {:?}", out.diagnostics);
    assert!(
        skipped[0].message.contains("`subArray` → `arraySlice`"),
        "got: {}",
        skipped[0].message
    );
}

/// Same shape, but the swallowed rewrite is the two-name callback
/// swap. Half a swap gives both parameters the same name; the pair
/// is pushed atomically, so neither name moves.
#[test]
fn swallowed_callback_swap_is_reported_and_leaves_both_names() {
    let out = run_pass(
        &V3ToV4,
        "var s = subArray(a, 0, arrayMap(m, (k, v) -> k))\n",
        id(),
    );
    assert!(
        out.text.contains("arrayMap(m, (k, v) -> k)"),
        "half-applied swap: {}",
        out.text
    );
    let skipped = skipped(&out.diagnostics);
    assert_eq!(skipped.len(), 1, "diagnostics: {:?}", out.diagnostics);
    assert!(
        skipped[0].message.contains("callback parameter swap"),
        "got: {}",
        skipped[0].message
    );
}

/// The downgrade direction has the same hazard, through
/// `arraySlice`'s end-index compensation.
#[test]
fn swallowed_callback_swap_is_reported_on_downgrade() {
    let out = run_pass(
        &V4ToV3,
        "var s = arraySlice(a, 0, arrayMap(m, (k, v) -> k))\n",
        id(),
    );
    assert!(
        out.text.contains("arrayMap(m, (k, v) -> k)"),
        "half-applied swap: {}",
        out.text
    );
    assert_eq!(
        skipped(&out.diagnostics).len(),
        1,
        "diagnostics: {:?}",
        out.diagnostics
    );
}

/// Rewrites that don't collide stay silent — the warning must not
/// fire on every migrated call.
#[test]
fn applied_rewrites_report_nothing() {
    let out = run_pass(
        &V3ToV4,
        "var s = subArray(a, 0, 2)\nvar t = arrayMap(m, (k, v) -> k)\n",
        id(),
    );
    assert!(
        out.text.contains("arraySlice(a, 0, (2) + 1)"),
        "got: {}",
        out.text
    );
    assert!(
        out.text.contains("arrayMap(m, (v, k) -> k)"),
        "got: {}",
        out.text
    );
    assert!(
        skipped(&out.diagnostics).is_empty(),
        "diagnostics: {:?}",
        out.diagnostics
    );
}
