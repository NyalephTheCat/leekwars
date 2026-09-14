//! The registry contract: the lint list, the diagnostic catalog and the
//! `@allow` name lookup must agree.
//!
//! Before the `lint_rules!` registry existed these were three hand-maintained
//! lists (the `pub mod` block, `all_passes`'s `Box::new(…)` column, and the
//! catalog's `lint:` section) with nothing checking them against each other,
//! so forgetting one was silent. Two of the three are now generated; this test
//! guards the third — the catalog, which lives in another crate.

use std::collections::{HashMap, HashSet};

use leek_diagnostics::codes;
use leek_lint::allow::code_to_rule_name;
use leek_lint::{all_metas, all_passes};

/// Catalog entries in the lint range. The `L` prefix is the range's
/// definition (see `AllowMap::suppress`, which treats it the same way).
fn catalog_lint_ids() -> Vec<&'static str> {
    codes::CATALOG
        .iter()
        .map(|m| m.id)
        .filter(|id| id.starts_with('L'))
        .collect()
}

#[test]
fn every_catalog_lint_code_has_exactly_one_pass() {
    let mut by_code: HashMap<&str, Vec<&str>> = HashMap::new();
    for meta in all_metas() {
        by_code.entry(meta.code.id()).or_default().push(meta.name);
    }
    for id in catalog_lint_ids() {
        match by_code.get(id) {
            None => panic!(
                "catalog code {id} has no lint pass — add the rule module to \
                 `lint_rules!` in crates/tools/leek-lint/src/rules/mod.rs, \
                 or drop {id} from catalog.yaml"
            ),
            Some(names) if names.len() > 1 => {
                panic!("catalog code {id} is claimed by more than one pass: {names:?}")
            }
            Some(_) => {}
        }
    }
}

#[test]
fn every_registered_pass_has_a_catalog_entry() {
    let known: HashSet<&str> = catalog_lint_ids().into_iter().collect();
    for meta in all_metas() {
        assert!(
            known.contains(meta.code.id()),
            "lint `{}` emits {} which is not in the `lint:` section of \
             crates/core/leek-diagnostics/catalog.yaml",
            meta.name,
            meta.code.id()
        );
    }
}

#[test]
fn rule_names_are_unique_kebab_case() {
    let mut seen: HashSet<&str> = HashSet::new();
    for meta in all_metas() {
        assert!(
            seen.insert(meta.name),
            "duplicate lint name `{}`",
            meta.name
        );
        assert!(
            is_kebab_case(meta.name),
            "lint name `{}` is not lowercase kebab-case — it is what users \
             type in `@allow(…)`",
            meta.name
        );
    }
}

/// `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`, spelled out so the test needs no regex
/// dependency.
fn is_kebab_case(s: &str) -> bool {
    let mut segments = s.split('-');
    let Some(first) = segments.next() else {
        return false;
    };
    let alnum = |seg: &str| {
        !seg.is_empty()
            && seg
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    };
    first.starts_with(|c: char| c.is_ascii_lowercase()) && alnum(first) && segments.all(alnum)
}

#[test]
fn names_round_trip_through_allow() {
    for meta in all_metas() {
        assert_eq!(
            code_to_rule_name(meta.code.id()),
            Some(meta.name),
            "`@allow({})` would not resolve to {}",
            meta.name,
            meta.code.id()
        );
    }
}

#[test]
fn every_registered_code_has_an_explanation() {
    for meta in all_metas() {
        let explain = meta.code.explain().unwrap_or_else(|| {
            panic!(
                "lint {} (`{}`) has no crates/core/leek-diagnostics/explain/{}.md",
                meta.code.id(),
                meta.name,
                meta.code.id()
            )
        });
        assert!(
            !explain.trim().is_empty(),
            "explain/{}.md is empty",
            meta.code.id()
        );
    }
}

#[test]
fn registry_and_all_passes_agree() {
    let metas = all_metas();
    let passes = all_passes();
    assert_eq!(metas.len(), passes.len());
    for (meta, pass) in metas.iter().zip(&passes) {
        assert_eq!(
            meta.code,
            pass.meta().code,
            "registration built a pass whose `meta()` disagrees with its `declare_lint!`"
        );
        assert_eq!(meta.group, pass.meta().group);
    }
}

// Deliberately *not* asserted: that a lint's `LintGroup` implies its catalog
// severity. The data contradicts any such rule — `Style` holds both hints
// (L0003, L0004) and warnings (L0008, L0020), and `Suspicious` holds a hint
// (L0018). Group decides default-on vs opt-in; severity decides how loud a
// finding is. Inventing a coupling here would only force the data to be bent
// to fit it.
