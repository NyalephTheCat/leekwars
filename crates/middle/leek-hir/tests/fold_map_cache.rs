//! `fold_map` parses each catalog's value strings once per [`FoldSet`], not
//! once per lowered file (#191), and reads nothing but its argument to do it
//! (#98, #226).
//!
//! That second property is why this test no longer registers anything:
//! before, it had to drive `leek_prelude::activate_fold_constants` and live
//! in its own process so two tests could not race on the generation counter.
//! With the catalogs named by the argument there is no counter and no
//! process-global left to disturb — the assertions below hold in any order,
//! in any process.

use std::sync::Arc;

use leek_config::FoldSet;
use leek_hir::ir::Literal;

#[test]
fn the_parsed_map_is_shared_per_fold_set() {
    // Folding nothing is the default compile path: an empty map, so the fold
    // pass is a no-op — and still one shared allocation, not a fresh one per
    // lowered file.
    let none = leek_hir::fold_map(FoldSet::NONE);
    assert!(none.is_empty(), "FoldSet::NONE must fold nothing");
    assert!(
        Arc::ptr_eq(&none, &leek_hir::fold_map(FoldSet::NONE)),
        "a second call for the same set must not rebuild the map"
    );

    // The leek-wars catalog applies the value rules the lowering pass used
    // inline: `.`-bearing → real, else integer. `leek-environment`'s own
    // tests pin these two values.
    let leekwars = leek_hir::fold_map(FoldSet::LEEKWARS);
    assert_eq!(leekwars.get("WEAPON_PISTOL"), Some(&Literal::Int(37)));
    assert_eq!(leekwars.get("EROSION_DAMAGE"), Some(&Literal::Real(0.05)));

    // Each set memoizes in its own slot, so the two never alias.
    assert!(
        Arc::ptr_eq(&leekwars, &leek_hir::fold_map(FoldSet::LEEKWARS)),
        "the leek-wars map must be memoized in its turn"
    );
    assert!(
        !Arc::ptr_eq(&none, &leekwars),
        "two different fold sets must not share one map"
    );
}
