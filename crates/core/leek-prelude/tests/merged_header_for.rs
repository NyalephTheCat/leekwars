//! The pure, configuration-keyed header join (#98, #184).
//!
//! Its own test binary, following the convention `tests/libraries.rs`
//! documents — every other integration test here writes the crate's
//! process-globals, and cargo gives each `tests/*.rs` its own process. Here
//! that isolation is the assertion rather than a precaution: **nothing in
//! this file calls an `activate_*` function**, so a `merged_header_for` that
//! peeked at `ACTIVE_LIBRARIES` instead of at its argument would answer
//! "no libraries" to every call below and fail. Do not add an `activate_*`
//! call to this file.

use std::sync::Arc;

use leek_config::LibrarySet;
use leek_prelude::{LEEKWARS_SRC, PRELUDE_SRC, STDLIB_SRC, library_src, merged_header_for};

/// Both libraries, built through the public `leek-config` API.
fn both() -> LibrarySet {
    let mut set = LibrarySet::NONE;
    set.insert(LibrarySet::LEEKWARS);
    set.insert(LibrarySet::STDLIB);
    set
}

#[test]
fn an_empty_configuration_merges_to_nothing() {
    assert_eq!(
        merged_header_for(LibrarySet::NONE, false),
        None,
        "no libraries and no prelude must merge to nothing — `Some(\"\")` \
         would make HIR lowering parse an empty prelude unit on every file"
    );
}

#[test]
fn the_prelude_alone_merges_to_exactly_the_prelude() {
    // A join of one part is the identity: no separator, no trailing newline.
    let merged = merged_header_for(LibrarySet::NONE, true).expect("the prelude alone is Some");
    assert_eq!(&*merged, PRELUDE_SRC);
}

#[test]
fn a_library_merges_behind_the_prelude() {
    // The answer comes from the argument, not from the process: no library
    // is active in this binary, so a globals-reading implementation would
    // hand back `PRELUDE_SRC` alone here.
    let merged = merged_header_for(LibrarySet::LEEKWARS, true).expect("Some");
    assert_eq!(&*merged, &format!("{PRELUDE_SRC}\n{LEEKWARS_SRC}"));

    // Order is load-bearing: leek-hir parses the join as one prelude unit
    // and later definitions shadow earlier ones, so a library header must be
    // able to override a prelude signature and not the other way round.
    assert!(merged.starts_with(PRELUDE_SRC));
}

#[test]
fn a_library_without_the_prelude_is_just_that_library() {
    let merged = merged_header_for(LibrarySet::STDLIB, false).expect("Some");
    assert_eq!(&*merged, STDLIB_SRC);
}

#[test]
fn libraries_join_in_bit_order() {
    let merged = merged_header_for(both(), false).expect("Some");
    assert_eq!(&*merged, &format!("{LEEKWARS_SRC}\n{STDLIB_SRC}"));

    let with_prelude = merged_header_for(both(), true).expect("Some");
    assert_eq!(
        &*with_prelude,
        &format!("{PRELUDE_SRC}\n{LEEKWARS_SRC}\n{STDLIB_SRC}")
    );
}

#[test]
fn repeated_calls_share_one_arc() {
    // The point of the memo table: the ~64 KB join, and the content hash
    // `parse_signature_header` takes of it, are paid once per configuration.
    let first = merged_header_for(LibrarySet::LEEKWARS, false).expect("Some");
    let again = merged_header_for(LibrarySet::LEEKWARS, false).expect("Some");
    assert!(
        Arc::ptr_eq(&first, &again),
        "two calls with one configuration must share an Arc, not re-join"
    );
}

#[test]
fn each_bit_names_its_own_header() {
    assert_eq!(library_src(LibrarySet::LEEKWARS), Some(LEEKWARS_SRC));
    assert_eq!(library_src(LibrarySet::STDLIB), Some(STDLIB_SRC));
    // Neither the empty set nor a multi-library one names one source.
    assert_eq!(library_src(LibrarySet::NONE), None);
    assert_eq!(library_src(both()), None);
}
