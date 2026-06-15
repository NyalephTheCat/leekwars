//! Complexity model for **native** (builtin) functions and methods.
//!
//! Leekscript's standard library is a large set of native functions
//! (`sort`, `arrayMap`, `substring`, …) whose cost the user can't see
//! by reading source — there's no Leekscript body to walk. This module
//! gives each of them a [`CostExpr`] in terms of its argument sizes so
//! the analyser can fold native calls into a caller's formula instead
//! of giving up with [`CostExpr::Unknown`].
//!
//! ## Two sources of truth
//!
//! 1. **A curated asymptotic table** ([`curated_growth`]). For the
//!    builtins whose textbook complexity is non-linear — `sort`
//!    (`n·log n`), `arrayIntersect` (`n·m`) — and for the linear
//!    scans whose shape we want to pin precisely (`reverse`,
//!    `indexOf`, `concat`, …).
//!
//! 2. **The shared builtin catalog** ([`leek_builtins`], generated
//!    from `catalog.yaml`). Every catalogued native carries a per-call
//!    `op_cost` (a constant) and, for element-scaled "batch" builtins,
//!    a `batch_mult` (a per-element multiplier). When a name isn't in
//!    the curated table we fall back to the catalog: a `batch_mult`
//!    means *linear in the first argument*; otherwise just the
//!    constant base cost.
//!
//! The catalog fallback is what makes the user's request work — *every*
//! native now has a defined complexity instead of silently costing
//! zero. The curated table only exists to upgrade the handful of
//! builtins whose true asymptotics the flat `batch_mult` can't express.
//!
//! Higher-order builtins (`arrayMap` and friends, see [`is_hof`]) are
//! deliberately **not** handled here: their cost is `count(arr) ·
//! lambda_body_cost`, and the lambda body can only be walked with the
//! analyser's caller context. The walker handles those directly.

use crate::big_o::{BigO, big_o};
use crate::cost_expr::{CostExpr, SizeVar};

/// Total symbolic cost of a native (builtin) call, given the *size* of
/// each argument as a [`CostExpr`] (`Const(0)` where the size is
/// unknown or the argument isn't a container).
///
/// The result combines the catalog per-call base cost with the growth
/// term (curated shape, else catalog `batch_mult`). Returns
/// [`CostExpr::Const`] for a pure-constant native.
///
/// Does **not** handle higher-order builtins — callers must check
/// [`is_hof`] first and walk the lambda body themselves.
pub fn native_call_cost(name: &str, arg_sizes: &[CostExpr]) -> CostExpr {
    CostExpr::sum(vec![
        CostExpr::Const(base_cost(name)),
        native_growth(name, arg_sizes),
    ])
}

/// The size-dependent growth term of a native call (no per-call base).
/// Curated shapes take precedence; otherwise the catalog's `batch_mult`
/// gives a linear-in-`arg0` term, and a non-batch native grows by
/// `Const(0)`.
pub fn native_growth(name: &str, arg_sizes: &[CostExpr]) -> CostExpr {
    if let Some(g) = curated_growth(name, arg_sizes) {
        return g;
    }
    catalog_growth(name, arg_sizes)
}

/// Per-call base cost from the shared catalog (`op_cost`). Floors at
/// `1` so even an uncatalogued native is charged for the call itself
/// (the analyser's per-call overhead).
pub fn base_cost(name: &str) -> u64 {
    leek_builtins::op_cost_u64(name).max(1)
}

/// Asymptotic class of a native function in terms of a generic size
/// variable over its first argument (named after `arg0_name`, e.g. the
/// receiver/array parameter). This is the directly *queryable* form of
/// a native's complexity — handy for documentation and tooling that
/// wants "what's the big-O of `sort`?" without a call site.
///
/// Two-argument shapes (`arrayIntersect`, `concat`) report over the
/// first *and* second size variables, so the second is named `arg1_name`.
pub fn native_big_o(name: &str, arg0_name: &str, arg1_name: &str) -> BigO {
    let sizes = [
        CostExpr::Size(SizeVar::new(0, arg0_name)),
        CostExpr::Size(SizeVar::new(1, arg1_name)),
    ];
    big_o(&native_growth(name, &sizes))
}

/// `true` for builtins that take a callback invoked once per element of
/// their first argument. Their cost is `count(arr) · body_cost` and the
/// analyser walks the lambda body itself; [`native_growth`] returns
/// `Const(0)` for these so it can't double-count.
pub fn is_hof(name: &str) -> bool {
    matches!(
        name,
        "arrayMap"
            | "arrayFilter"
            | "arrayReduce"
            | "arrayReduceRight"
            | "arrayFoldLeft"
            | "arrayFoldRight"
            | "arrayForeach"
            | "forEach"
            | "arrayIter"
            | "arrayPartition"
            | "arrayEvery"
            | "arraySome"
            | "mapFilter"
            | "mapMap"
            | "mapForEach"
            | "setForEach"
            | "setForeach"
            | "intervalMap"
            | "intervalFilter"
            | "intervalForeach"
            | "intervalForEach"
            | "intervalReduce"
            | "intervalReduceRight"
    )
}

/// `count(p)` / `length(p)` / `size(p)` / `mapSize(p)` — the builtins
/// that simply read a container's element count. Recognised by the
/// analyser when turning a call-site argument into a size variable.
pub fn is_size_query(name: &str) -> bool {
    matches!(name, "count" | "length" | "size" | "mapSize")
}

/// `true` if we have *any* cost model for `name` — a curated shape, a
/// higher-order shape, a size query, or a catalogued builtin. Used to
/// decide whether an `obj.name(...)` call is a native invoked through
/// method syntax (`arr.sort()`, `s.length()`) once it's been ruled out
/// as a user method.
///
/// Note the curated table uses Leekscript-level names (`sort`,
/// `reverse`) that the Java-oriented catalog sometimes spells
/// differently (`arraySort`, `arrayReverse`), so a plain
/// `leek_builtins::is_catalogued` check alone would miss them.
pub fn is_native(name: &str) -> bool {
    is_hof(name)
        || is_size_query(name)
        || curated_growth(name, &[]).is_some()
        || leek_builtins::is_catalogued(name)
}

fn first(sizes: &[CostExpr]) -> CostExpr {
    sizes.first().cloned().unwrap_or(CostExpr::Const(0))
}

fn second(sizes: &[CostExpr]) -> CostExpr {
    sizes.get(1).cloned().unwrap_or(CostExpr::Const(0))
}

/// Curated asymptotic shapes for natives whose true complexity a flat
/// per-element multiplier can't capture. `None` falls through to the
/// catalog. HOFs are excluded (the walker owns them).
fn curated_growth(name: &str, sizes: &[CostExpr]) -> Option<CostExpr> {
    if is_hof(name) {
        // Growth is contributed by the walker via the lambda body.
        return Some(CostExpr::Const(0));
    }
    let g = match name {
        // Linear in the (single) container argument.
        "reverse" | "arrayReverse" | "shuffle" | "arrayShuffle" | "subArray" | "arraySlice"
        | "fill" | "indexOf" | "lastIndexOf" | "search" | "contains" | "inArray" | "join"
        | "stringJoin" | "stringReverse" | "arrayFlatten" | "flatten" | "arrayDistinct"
        | "arrayUnique" | "arrayKeys" | "arrayValues" | "entries" | "mapKeys" | "mapValues"
        | "arrayCopy" | "clone" | "arrayMax" | "arrayMin" | "arrayCount" | "arrayProduct"
        | "arrayAvg" | "arrayAdd" | "arraySum" | "arrayAverage" => first(sizes),

        // Linear in *both* container arguments — `n + m`.
        "concat" | "arrayConcat" => CostExpr::sum(vec![first(sizes), second(sizes)]),

        // Comparison sorts — `n · log n`.
        "sort" | "arraySort" | "intervalSort" => {
            CostExpr::product(vec![first(sizes), CostExpr::Log(Box::new(first(sizes)))])
        }

        // Pairwise set operations — `n · m`.
        "arrayIntersect" | "arrayUnion" | "arrayDifference" => {
            CostExpr::product(vec![first(sizes), second(sizes)])
        }

        _ => return None,
    };
    Some(g)
}

/// Catalog-driven fallback: a `batch_mult` builtin grows linearly in
/// its first container argument (`mult · count(arg0)`); everything else
/// has no size-dependent growth.
fn catalog_growth(name: &str, sizes: &[CostExpr]) -> CostExpr {
    match leek_builtins::batch_multiplier_u64(name) {
        Some(mult) => CostExpr::product(vec![CostExpr::Const(mult), first(sizes)]),
        None => CostExpr::Const(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n() -> SizeVar {
        SizeVar::new(0, "n")
    }
    fn m() -> SizeVar {
        SizeVar::new(1, "m")
    }

    #[test]
    fn sort_is_n_log_n() {
        assert!(matches!(
            native_big_o("sort", "n", "m"),
            BigO::NLogN(v) if v.name == "n"
        ));
    }

    #[test]
    fn intersect_is_pairwise() {
        let g = native_growth(
            "arrayIntersect",
            &[CostExpr::Size(n()), CostExpr::Size(m())],
        );
        let label = big_o(&g).render();
        assert!(label.contains('n') && label.contains('m'), "got {label}");
    }

    #[test]
    fn uncurated_batch_builtin_falls_back_to_catalog_linear() {
        // `arrayClone` has a batch_mult in the catalog but no curated
        // shape — it should still come out linear in arg0.
        let g = native_growth("arrayClone", &[CostExpr::Size(n())]);
        assert!(
            matches!(big_o(&g), BigO::Linear(v) if v.name == "n"),
            "got {g}"
        );
    }

    #[test]
    fn constant_builtin_has_constant_growth_and_base() {
        // `count` is op_cost 1, no batch_mult → constant growth.
        assert!(matches!(
            big_o(&native_growth("count", &[CostExpr::Size(n())])),
            BigO::Constant
        ));
        assert!(base_cost("count") >= 1);
    }

    #[test]
    fn base_cost_floors_at_one() {
        assert!(base_cost("a-name-that-is-not-catalogued") >= 1);
    }
}
