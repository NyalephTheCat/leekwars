//! Every builtin the runtime dispatches, the native backend can lower (#188).
//!
//! `is_generic_builtin` is this backend's machine-readable statement of which
//! builtins it supports: a name it omits falls through to
//! `Err(unsupported("builtin {name}"))` in `dispatch_builtin`, which means a
//! whole AI refuses to compile because of one call. That list was written by
//! hand and drifted — `setForEach`, `stringRepeat`, `stringCharCodeAt`,
//! `getDate`, `getTime`, `print` and `println` were all dispatched by
//! `leek_runtime::call_builtin` and rejected here, five of them only because
//! the runtime dispatches them under two names and native listed one.
//!
//! So this reads both sides from source rather than trusting a hand-kept
//! list, the same way `leek-runtime/tests/builtin_known_consistency.rs`
//! reads the dispatch tables instead of naming 17 builtins. What is *not*
//! derived is the exclusion list: a name native deliberately refuses has to
//! be written down with its reason, so "we forgot" can't hide as "we chose".

use leek_runtime::{builtin_class_name, math_sig};

/// The runtime's dispatch tables, verbatim. Each is a `match (name, arity)`,
/// so a dispatched name appears as `("name", N) =>` or `("a" | "b", N) =>`.
const DISPATCHERS: &[(&str, &str)] = &[
    (
        "array.rs",
        include_str!("../../../core/leek-runtime/src/builtins/array.rs"),
    ),
    (
        "map.rs",
        include_str!("../../../core/leek-runtime/src/builtins/map.rs"),
    ),
    (
        "misc.rs",
        include_str!("../../../core/leek-runtime/src/builtins/misc.rs"),
    ),
    (
        "string.rs",
        include_str!("../../../core/leek-runtime/src/builtins/string.rs"),
    ),
];

/// This backend's own claim, read from the `matches!` list itself.
const NATIVE_BUILTINS: &str = include_str!("../src/translate/builtins.rs");

/// Builtins `dispatch_builtin` handles before it consults
/// `is_generic_builtin` (`emit_call.rs`), each with a dedicated lowering.
const SPECIAL_CASED: &[&str] = &["abs", "signum", "min", "max", "count", "push"];

/// Names the runtime dispatches that native refuses **on purpose**. One entry,
/// one reason. Adding a name here is a decision; leaving one out is a bug.
const EXPLICIT_NATIVE_EXCLUSIONS: &[(&str, &str)] = &[
    // Nothing yet. A name belongs here only when lowering it would change
    // behaviour, not when nobody has got to it.
];

/// Extraction floor. Both sides are read by scanning source, so a refactor
/// into a macro or a table would find nothing and pass vacuously.
const MIN_DISPATCHED: usize = 150;
const MIN_CLAIMED: usize = 150;

fn names_in_pattern(pat: &str) -> Vec<&str> {
    pat.split('|')
        .map(str::trim)
        .filter_map(|p| p.strip_prefix('"')?.strip_suffix('"'))
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .collect()
}

/// Every name an `(name, arity) =>` arm dispatches in `src`.
fn arity_dispatched(src: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('(') else {
            continue;
        };
        let Some((inside, after)) = rest.split_once(')') else {
            continue;
        };
        if !after.trim_start().starts_with("=>") {
            continue;
        }
        let Some((pat, arity)) = inside.rsplit_once(',') else {
            continue;
        };
        // Arities appear as `2`, `1 | 2`, or `_` (`("print", _)`), and the
        // wildcard counts: a name dispatched at any arity is a name native
        // has to be able to lower.
        let arity = arity.trim();
        let arity_shaped = !arity.is_empty()
            && arity
                .chars()
                .all(|c| c.is_ascii_digit() || c == '_' || c == '|' || c.is_whitespace());
        if !arity_shaped {
            continue;
        }
        out.extend(names_in_pattern(pat));
    }
    out
}

fn dispatched_names() -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    for (file, src) in DISPATCHERS {
        out.extend(arity_dispatched(src).into_iter().map(|n| (*file, n)));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The string literals inside `is_generic_builtin`'s `matches!`.
fn claimed_names() -> Vec<&'static str> {
    let start = NATIVE_BUILTINS
        .find("pub(super) fn is_generic_builtin")
        .expect("is_generic_builtin moved — update this test");
    let body = &NATIVE_BUILTINS[start..];
    let end = body.find("\n}").expect("unterminated is_generic_builtin");
    body[..end]
        .lines()
        // Several entries carry a trailing `// like count/length` note, which
        // would otherwise swallow the name they annotate.
        .map(|line| line.split("//").next().unwrap_or(line))
        .flat_map(|line| names_in_pattern(line.trim().trim_start_matches('|')))
        .collect()
}

/// Whether `dispatch_builtin` can lower `name` with `link_game` off — which
/// is how `miku run`, `miku test` and the whole corpus compile.
fn native_lowers(name: &str, claimed: &[&str]) -> bool {
    claimed.contains(&name)
        || SPECIAL_CASED.contains(&name)
        || math_sig(name).is_some()
        || builtin_class_name(name).is_some()
}

#[test]
fn every_dispatched_builtin_is_lowerable_by_native() {
    let dispatched = dispatched_names();
    assert!(
        dispatched.len() >= MIN_DISPATCHED,
        "only extracted {} dispatched names (floor {MIN_DISPATCHED}) — the runtime's dispatch \
         tables were probably restructured and this scan no longer reads them",
        dispatched.len(),
    );
    let claimed = claimed_names();
    assert!(
        claimed.len() >= MIN_CLAIMED,
        "only extracted {} names from is_generic_builtin (floor {MIN_CLAIMED}) — the list was \
         probably restructured and this scan no longer reads it",
        claimed.len(),
    );

    let missing: Vec<String> = dispatched
        .iter()
        .filter(|(_, n)| !native_lowers(n, &claimed))
        .filter(|(_, n)| !EXPLICIT_NATIVE_EXCLUSIONS.iter().any(|(e, _)| e == n))
        .map(|(file, n)| format!("{n} ({file})"))
        .collect();
    assert!(
        missing.is_empty(),
        "the runtime dispatches these builtins and native rejects them as `unsupported: builtin \
         X`, failing the whole program over one call. Add each to `is_generic_builtin`, or to \
         EXPLICIT_NATIVE_EXCLUSIONS with the reason it must stay rejected: {missing:?}",
    );
}

/// The five that drifted did so because the runtime dispatches one operation
/// under two names. Named on their own so a re-regression reads as itself.
#[test]
fn the_double_named_builtins_are_lowerable_under_both_names() {
    let claimed = claimed_names();
    for (a, b) in [
        ("repeat", "stringRepeat"),
        ("charCodeAt", "stringCharCodeAt"),
        ("setIter", "setForEach"),
        ("getInstructionsCount", "getOperations"),
        ("getDate", "getTime"),
    ] {
        assert!(
            native_lowers(a, &claimed) && native_lowers(b, &claimed),
            "`{a}` and `{b}` are the same runtime operation; native must lower both or neither",
        );
    }
}

/// An exclusion with no reason is a to-do pretending to be a decision.
#[test]
fn every_exclusion_states_a_reason() {
    for (name, reason) in EXPLICIT_NATIVE_EXCLUSIONS {
        assert!(
            reason.len() > 20,
            "exclusion `{name}` needs a reason, not `{reason}`",
        );
    }
}
