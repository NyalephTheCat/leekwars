//! Shared builtin catalog — single source of truth in `catalog.yaml`.
//!
//! Generated at build time:
//! - [`JAVA_BUILTINS`] / [`lookup_java`] — static Java dispatch
//! - [`op_cost`] / [`op_cost_u64`] — interpreter per-call costs (default 1)
//! - [`op_cost_emit`] — Java emit per-call costs (default 0)
//! - [`batch_multiplier`] — element-scaled costs for batch builtins
//! - [`ALL_CATALOG_NAMES`] — every catalogued name (for contract tests)

include!(concat!(env!("OUT_DIR"), "/catalog.rs"));
include!(concat!(env!("OUT_DIR"), "/op_costs.rs"));
include!(concat!(env!("OUT_DIR"), "/batch_mult.rs"));
include!(concat!(env!("OUT_DIR"), "/registry.rs"));

/// True if `name` is a known Java-runtime static builtin function.
pub fn is_java_builtin(name: &str) -> bool {
    lookup_java(name).is_some()
}

/// True when emit should coerce `NumberClass` args with `longValue()`.
pub fn java_prefer_long(name: &str) -> bool {
    lookup_java(name).is_some_and(|b| b.return_type == "long")
}

/// True if `name` appears anywhere in the catalog (any metadata row).
pub fn is_catalogued(name: &str) -> bool {
    ALL_CATALOG_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_names_have_op_cost() {
        for name in ALL_CATALOG_NAMES {
            let _ = op_cost(name);
        }
    }

    #[test]
    fn java_entries_lookup() {
        for b in JAVA_BUILTINS {
            assert_eq!(lookup_java(b.name).map(|x| x.name), Some(b.name));
        }
    }

    #[test]
    fn abs_matches_upstream_cost() {
        assert_eq!(op_cost("abs"), 2);
        assert_eq!(op_cost("sqrt"), 8);
    }
}
