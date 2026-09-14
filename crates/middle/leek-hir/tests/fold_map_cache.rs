//! `fold_map` parses each registered constant's value string once per
//! [`leek_prelude::generation`], not once per lowered file (#191).
//!
//! The generation is process-global, so this lives in its own test binary
//! (its own process) and is written as one `#[test]` that walks the states in
//! order — two tests in one binary would race on the counter.

use std::sync::Arc;

use leek_hir::ir::Literal;

#[test]
fn the_parsed_map_is_shared_until_the_generation_moves() {
    // Same generation ⇒ literally the same allocation, not an equal copy.
    let first = leek_hir::fold_map();
    let second = leek_hir::fold_map();
    assert!(
        Arc::ptr_eq(&first, &second),
        "a second call at the same generation must not re-parse the values"
    );

    // Registering a genuinely new constant bumps the generation, which must
    // invalidate the parsed map rather than serve the pre-registration one.
    leek_prelude::activate_fold_constants([
        ("R1_05_FOLD_INT", "37"),
        ("R1_05_FOLD_REAL", "1.5"),
        ("R1_05_FOLD_JUNK", "not a number"),
    ]);
    let third = leek_hir::fold_map();
    assert!(
        !Arc::ptr_eq(&second, &third),
        "activate_fold_constants must invalidate the parsed map"
    );

    // …and the rebuild applies the same value rules the lowering pass used:
    // `.`-bearing → real, else integer, unparseable dropped.
    assert_eq!(third.get("R1_05_FOLD_INT"), Some(&Literal::Int(37)));
    assert_eq!(third.get("R1_05_FOLD_REAL"), Some(&Literal::Real(1.5)));
    assert_eq!(third.get("R1_05_FOLD_JUNK"), None);

    // The rebuilt map is then shared in its turn.
    assert!(
        Arc::ptr_eq(&third, &leek_hir::fold_map()),
        "the rebuilt map must be memoized at the new generation too"
    );

    // Re-registering an identical set changes nothing, so neither the
    // generation nor the map moves.
    leek_prelude::activate_fold_constants([("R1_05_FOLD_INT", "37")]);
    assert!(
        Arc::ptr_eq(&third, &leek_hir::fold_map()),
        "an idempotent re-registration must not discard the parsed map"
    );
}
