//! The generation counter and the two caches it keys.
//!
//! Like `tests/libraries.rs` and `tests/fold_constants.rs`, this is one
//! `#[test]` in its own binary: `activate_library` /
//! `activate_fold_constants` write to process-globals with no reset, and
//! every assertion below is about what the *previous* step did to them, so
//! a sibling test in this binary would race it.

use std::sync::Arc;

use leek_prelude::{
    LEEKWARS_SRC, PRELUDE_SRC, activate_fold_constants, activate_library, fold_constants,
    fold_constants_cached, generation, merged_header, merged_header_src,
};

/// The generator banner naming the upstream Java class, present in
/// `LEEKWARS_SRC` and in no other header.
const LEEKWARS_MARKER: &str = "FightFunctions.java";

#[test]
fn one_counter_keys_the_header_and_fold_caches() {
    // ---- Memoized within a generation -----------------------------------
    let gen0 = generation();
    let (first, gen_first) = merged_header(true).expect("the implicit prelude alone is Some");
    let (again, gen_again) = merged_header(true).expect("Some");
    assert!(
        Arc::ptr_eq(&first, &again),
        "two calls at one generation must share an Arc, not re-join ~64 KB \
         for `parse_signature_header` to hash again"
    );
    assert_eq!((gen_first, gen_again), (gen0, gen0));

    // The "nothing is active" answer is memoized too — it is what the
    // default compile path asks for on every file.
    assert!(
        merged_header(false).is_none(),
        "no libraries and no prelude merge to nothing"
    );

    let (fold_first, fold_gen) = fold_constants_cached();
    let (fold_again, fold_gen_again) = fold_constants_cached();
    assert!(
        Arc::ptr_eq(&fold_first, &fold_again),
        "a lowering run must be able to hold one Arc instead of re-locking \
         and deep-copying per file"
    );
    assert!(fold_first.is_empty(), "folding is opt-in");
    assert_eq!((fold_gen, fold_gen_again), (gen0, gen0));

    // ---- The cached and uncached forms agree ----------------------------
    assert_eq!(merged_header_src(true).as_deref(), Some(&*first));
    assert_eq!(fold_constants(), *fold_first);

    // ---- A library activation bumps and invalidates ---------------------
    activate_library(LEEKWARS_SRC);
    let gen1 = generation();
    assert_eq!(gen1, gen0 + 1, "activating a new library moves the counter");

    let (with_library, gen_with_library) = merged_header(true).expect("Some");
    assert_eq!(gen_with_library, gen1);
    assert!(
        !Arc::ptr_eq(&first, &with_library),
        "the pre-activation header must not survive: it is missing the \
         library that was just requested"
    );
    assert!(with_library.contains(LEEKWARS_MARKER));
    assert!(with_library.starts_with(PRELUDE_SRC));

    // The other slot is keyed on the same counter, so its `None` is gone too.
    let (no_prelude, gen_no_prelude) =
        merged_header(false).expect("an active library makes the prelude-off answer Some as well");
    assert_eq!(gen_no_prelude, gen1);
    assert_eq!(
        &*no_prelude, LEEKWARS_SRC,
        "a one-part join is the identity"
    );

    // …and the new answer is itself memoized.
    assert!(Arc::ptr_eq(
        &with_library,
        &merged_header(true).expect("Some").0
    ));
    assert_eq!(merged_header_src(true).as_deref(), Some(&*with_library));

    // Re-activating the same header is a no-op, counter included: a driver
    // that passes `--library leekwars` twice must not pay for a re-parse.
    activate_library(LEEKWARS_SRC);
    assert_eq!(generation(), gen1);
    assert!(Arc::ptr_eq(
        &with_library,
        &merged_header(true).expect("Some").0
    ));

    // ---- A fold activation bumps and invalidates ------------------------
    activate_fold_constants([("WEAPON_PISTOL", "37")]);
    let gen2 = generation();
    assert_eq!(gen2, gen1 + 1, "registering a constant moves the counter");

    let (folds, gen_folds) = fold_constants_cached();
    assert_eq!(gen_folds, gen2);
    assert!(
        !Arc::ptr_eq(&fold_first, &folds),
        "the empty snapshot must not outlive the registration"
    );
    assert_eq!(
        *folds,
        vec![("WEAPON_PISTOL".to_string(), "37".to_string())]
    );
    assert_eq!(fold_constants(), *folds);
    assert!(Arc::ptr_eq(&folds, &fold_constants_cached().0));

    // One counter keys both caches, so a fold registration invalidates the
    // header as well. The text is unchanged — only the generation moved —
    // which is the conservative half of the trade: a stale answer is never
    // served, an unnecessary rebuild sometimes is.
    let (after_fold, gen_after_fold) = merged_header(true).expect("Some");
    assert_eq!(gen_after_fold, gen2);
    assert!(!Arc::ptr_eq(&with_library, &after_fold));
    assert_eq!(&*after_fold, &*with_library);

    // Re-registering an identical pair changes nothing and bumps nothing.
    activate_fold_constants([("WEAPON_PISTOL", "37")]);
    assert_eq!(generation(), gen2);
    assert!(Arc::ptr_eq(&folds, &fold_constants_cached().0));

    // Changing a value does bump.
    activate_fold_constants([("WEAPON_PISTOL", "42")]);
    assert_eq!(generation(), gen2 + 1);
    let (changed, gen_changed) = fold_constants_cached();
    assert_eq!(gen_changed, gen2 + 1);
    assert_eq!(
        *changed,
        vec![("WEAPON_PISTOL".to_string(), "42".to_string())]
    );
}
