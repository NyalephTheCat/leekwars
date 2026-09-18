//! Activating "the same header" twice must merge it once, even when the
//! two activations do not share a pointer.
//!
//! One `#[test]` in its own binary, for the reason `libraries.rs` gives:
//! `activate_library` writes to a process-global with no reset
//! (ARCH-02 #98 / QUAL-01 #113), so a second test here would race it.

use leek_prelude::{LEEKWARS_SRC, activate_library, generation, merged_header_src};

/// A line that occurs exactly once in `LEEKWARS_SRC` — the generator
/// banner naming the upstream Java class it was derived from.
const LEEKWARS_MARKER: &str = "FightFunctions.java";

/// `LEEKWARS_SRC` seen from another crate: identical bytes, its own
/// address. `const` items are inlined per crate, so this is what
/// `leek_session::recipes::register_leekwars` really hands in — not the
/// pointer this crate's `library_src` would return (#490).
fn foreign_copy_of(src: &str) -> &'static str {
    let copy: &'static str = Box::leak(String::from(src).into_boxed_str());
    assert!(
        !std::ptr::eq(copy, src),
        "the copy must not share an address, or this test proves nothing"
    );
    assert_eq!(copy, src, "the copy must be byte-identical");
    copy
}

#[test]
fn a_header_activated_under_two_pointers_merges_once() {
    assert_eq!(LEEKWARS_SRC.matches(LEEKWARS_MARKER).count(), 1);

    activate_library(LEEKWARS_SRC);
    let after_first = generation();

    // The same header, arriving as a different crate's copy.
    activate_library(foreign_copy_of(LEEKWARS_SRC));

    let merged = merged_header_src(false).expect("an active library merges to Some");
    assert_eq!(
        merged.matches(LEEKWARS_MARKER).count(),
        1,
        "the same header activated under two pointers must merge once, \
         or every function it declares becomes a duplicate definition"
    );
    assert_eq!(
        generation(),
        after_first,
        "a duplicate activation must not bump the generation: it would \
         discard a merged header that is still correct"
    );
}
