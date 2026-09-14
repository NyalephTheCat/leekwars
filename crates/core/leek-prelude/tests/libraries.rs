//! Library activation and the shape of the merged header.
//!
//! One `#[test]` in its own binary, on purpose: `activate_library` writes
//! to a process-global with no reset (ARCH-02 #98 / QUAL-01 #113), so a
//! second test in this binary would race the first and the ordering
//! assertions would depend on which one ran.

use leek_config::LibrarySet;
use leek_prelude::{
    LEEKWARS_SRC, PRELUDE_SRC, STDLIB_SRC, activate_library, active_library_set, merged_header_for,
    merged_header_src,
};

/// A line that occurs in exactly one of the three headers — the generator
/// banner naming the upstream Java class it was derived from.
const LEEKWARS_MARKER: &str = "FightFunctions.java";
const STDLIB_MARKER: &str = "LeekFunctions.java";

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn activation_is_idempotent_ordered_and_newline_separated() {
    // Sanity: the markers really are unique per header, or the
    // duplicate-activation assertion below proves nothing.
    assert_eq!(occurrences(LEEKWARS_SRC, LEEKWARS_MARKER), 1);
    assert_eq!(occurrences(STDLIB_SRC, STDLIB_MARKER), 1);
    assert_eq!(occurrences(PRELUDE_SRC, LEEKWARS_MARKER), 0);
    assert_eq!(occurrences(PRELUDE_SRC, STDLIB_MARKER), 0);

    // `--library leekwars --library leekwars` must not merge the header
    // twice: every function in it would then be a duplicate definition.
    activate_library(LEEKWARS_SRC);
    activate_library(LEEKWARS_SRC);
    let merged = merged_header_src(false).expect("an active library merges to Some");
    assert_eq!(
        occurrences(&merged, LEEKWARS_MARKER),
        1,
        "activating the same header twice must merge it once"
    );
    assert_eq!(occurrences(&merged, STDLIB_MARKER), 0);

    // A second, distinct header joins the first in activation order.
    activate_library(STDLIB_SRC);
    let merged = merged_header_src(false).expect("Some");
    let leekwars_at = merged.find(LEEKWARS_MARKER).expect("leekwars present");
    let stdlib_at = merged.find(STDLIB_MARKER).expect("stdlib present");
    assert!(
        leekwars_at < stdlib_at,
        "libraries merge in activation order"
    );

    // With the implicit prelude enabled it comes first. Order is
    // load-bearing: leek-hir parses the join as a single prelude unit and
    // later definitions shadow earlier ones, so a library header must be
    // able to override a prelude signature and not the other way round.
    let with_prelude = merged_header_src(true).expect("Some");
    assert!(
        with_prelude.starts_with(PRELUDE_SRC),
        "the implicit prelude leads the merged header"
    );

    // Each part survives verbatim and is followed by a newline, so the
    // last declaration of one header cannot fuse with the first token of
    // the next.
    for part in [PRELUDE_SRC, LEEKWARS_SRC, STDLIB_SRC] {
        let at = with_prelude.find(part).expect("part present verbatim");
        let end = at + part.len();
        assert!(
            end == with_prelude.len() || with_prelude.as_bytes()[end] == b'\n',
            "merged parts are newline-separated, not glued together"
        );
    }

    // Documents the current (leaky) behaviour that ARCH-02 #98 /
    // QUAL-01 #113 will change: there is no way to deactivate a library,
    // so a "fresh" logical run in the same process still sees what an
    // earlier one activated. When those issues land, this becomes
    // `assert_eq!(merged_header_src(false), None)` after a reset.
    assert!(
        merged_header_src(false).is_some(),
        "activation is process-global and never cleared"
    );

    // The configuration-keyed join is the same join (#98, #184). Both
    // libraries are active here, in bit order, so the pure function fed the
    // set this process is in must produce the very same text — which is what
    // lets a later slice move a caller across without changing its output.
    let active = active_library_set();
    assert!(active.contains(LibrarySet::LEEKWARS) && active.contains(LibrarySet::STDLIB));
    for prelude_enabled in [false, true] {
        assert_eq!(
            merged_header_for(active, prelude_enabled).as_deref(),
            merged_header_src(prelude_enabled).as_deref(),
            "the pure join must match the process-global one"
        );
    }
}
