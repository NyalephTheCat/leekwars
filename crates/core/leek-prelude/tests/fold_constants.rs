//! Constant-fold registration.
//!
//! `FOLD_CONSTANTS` is process-global with no reset, so — like
//! `tests/libraries.rs` — the whole sequence is a single `#[test]` in its
//! own binary.

use leek_config::FoldSet;
use leek_prelude::{activate_fold_constants, active_fold_set, fold_constants};

#[test]
fn registration_replaces_by_name_and_keeps_insertion_order() {
    assert!(
        fold_constants().is_empty(),
        "folding is off until a driver registers something"
    );
    assert_eq!(
        active_fold_set(),
        FoldSet::NONE,
        "an empty map reads back as no catalog folded (#98, #184)"
    );

    activate_fold_constants([("WEAPON_PISTOL", "37"), ("CHIP_BANDAGE", "1")]);
    assert_eq!(
        active_fold_set(),
        FoldSet::LEEKWARS,
        "every production activation registers the leek-wars catalog, \
         so a non-empty map reads back as that bit"
    );
    assert_eq!(
        fold_constants(),
        vec![
            ("WEAPON_PISTOL".to_string(), "37".to_string()),
            ("CHIP_BANDAGE".to_string(), "1".to_string()),
        ],
        "registration order is preserved"
    );

    // A later registration for the same name wins *in place* — appending a
    // second entry would leave the HIR lowerer folding whichever it hit
    // first, which is the older value.
    activate_fold_constants([("WEAPON_PISTOL", "42")]);
    let after = fold_constants();
    assert_eq!(
        after.iter().filter(|(n, _)| n == "WEAPON_PISTOL").count(),
        1,
        "re-registering a name replaces rather than duplicates"
    );
    assert_eq!(
        after,
        vec![
            ("WEAPON_PISTOL".to_string(), "42".to_string()),
            ("CHIP_BANDAGE".to_string(), "1".to_string()),
        ],
        "the replaced entry keeps its original position"
    );

    // Values stay strings verbatim. This crate deliberately holds no IR
    // dependency; leek-hir's lowerer decides `.`-bearing ⇒ real, else
    // integer, so a `.` must survive the round trip unparsed.
    activate_fold_constants([("PI", "3.5")]);
    let value = fold_constants()
        .into_iter()
        .find(|(n, _)| n == "PI")
        .map(|(_, v)| v)
        .expect("PI registered");
    assert_eq!(
        value, "3.5",
        "values are opaque strings, not parsed numbers"
    );
}
