//! Guard against drift between *dispatched* builtins (`call_builtin`) and the
//! `KNOWN_BUILTIN_NAMES` list that `is_known_builtin` checks. A function that
//! `call_builtin` handles but `is_known_builtin` doesn't recognize can't be
//! used as a first-class function value (`var f = setForEach`) — it resolves to
//! null instead. `setForEach` and `setIsSupersetOf` were such gaps (item 3).

use leek_runtime::is_known_builtin;

#[test]
fn all_set_builtins_are_known() {
    // The authoritative set of dispatched `set*` builtins (leek-runtime
    // `builtins/misc.rs`). Every one must be `is_known_builtin`.
    const SET_BUILTINS: &[&str] = &[
        "setPut",
        "setRemove",
        "setContains",
        "setClear",
        "setIsEmpty",
        "setSize",
        "setToArray",
        "setIsSubsetOf",
        "setIsSupersetOf",
        "setUnion",
        "setIntersection",
        "setDifference",
        "setDisjunction",
        "setFilter",
        "setMap",
        "setIter",
        "setForEach",
    ];
    let missing: Vec<&str> = SET_BUILTINS
        .iter()
        .copied()
        .filter(|n| !is_known_builtin(n))
        .collect();
    assert!(
        missing.is_empty(),
        "dispatched set builtins missing from KNOWN_BUILTIN_NAMES: {missing:?}",
    );
}
