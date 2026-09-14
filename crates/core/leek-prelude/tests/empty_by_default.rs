//! The "nothing is active" contract.
//!
//! `ACTIVE_LIBRARIES` and `FOLD_CONSTANTS` are process-global with no
//! reset, so this is the only place the untouched state can be observed.
//! It gets its own test binary for that reason: cargo gives each
//! `tests/*.rs` its own process, and a sibling test that activated a
//! library would make every assertion here vacuous.
//!
//! Do not add an `activate_*` call to this file.

use leek_prelude::{PRELUDE_SRC, fold_constants, merged_header_src};

#[test]
fn nothing_is_active_until_a_driver_asks() {
    assert_eq!(
        merged_header_src(false),
        None,
        "no libraries and no prelude must merge to nothing — `Some(\"\")` \
         would make HIR lowering parse an empty prelude unit on every file"
    );
    assert!(
        fold_constants().is_empty(),
        "constant folding is opt-in; a non-empty default would change the \
         corpus baseline"
    );
}

#[test]
fn the_prelude_alone_merges_to_exactly_the_prelude() {
    // No separator, no trailing newline of its own: the merge is a join,
    // and with one part a join must be the identity.
    assert_eq!(merged_header_src(true), Some(PRELUDE_SRC.to_string()));
}
