//! Guard against drift between *dispatched* builtins (`call_builtin`) and the
//! `KNOWN_BUILTIN_NAMES` list that `is_known_builtin` checks. A function that
//! `call_builtin` handles but `is_known_builtin` doesn't recognize can't be
//! used as a first-class function value (`var f = setForEach`) — it resolves to
//! null instead. `setForEach` and `setIsSupersetOf` were such gaps, and so were
//! `keys`, `values`, `log2`, `log10`, `stringRepeat` and `stringCharCodeAt`
//! until this test stopped hard-coding 17 names and read the dispatchers.
//!
//! The dispatched set is derived from the source at *compile* time
//! (`include_str!`), so the test is hermetic — no IO, no build script — and
//! generalizes to every name instead of the handful someone remembered. This
//! is the same shape as the other table-drift checks in the repo
//! (`leek-resolver/src/builtins.rs`, `tools/game-builtin-extract.sh --check`).
//!
//! Constants (`PI`, `TYPE_ARRAY`, `CELL_EMPTY`, …) are deliberately out of
//! scope: they come from `dispatch_constant` / `lookup_constant`, a separate
//! mechanism from the callable dispatch this guards.

use leek_runtime::is_known_builtin;

/// The dispatch tables, verbatim. Each is a `match (name, args.len())`, so a
/// dispatched name appears as `("name", N) =>` or `("a" | "b", N) =>`.
const ARITY_DISPATCHERS: &[(&str, &str)] = &[
    ("array.rs", include_str!("../src/builtins/array.rs")),
    ("map.rs", include_str!("../src/builtins/map.rs")),
    ("misc.rs", include_str!("../src/builtins/misc.rs")),
    ("string.rs", include_str!("../src/builtins/string.rs")),
];

/// `core.rs` holds two `match name` tables; only the second one
/// (`dispatch_unary_math`) is callable dispatch.
const CORE: &str = include_str!("../src/builtins/core.rs");
const UNARY_MATH_FN: &str = "pub(crate) fn dispatch_unary_math";

/// Extraction floor. The scan reads match arms, so a refactor into a macro or
/// a table would silently find nothing and the test would pass vacuously.
/// Failing loudly below the floor is the point.
const MIN_EXTRACTED: usize = 200;

/// Pull the string literals out of one match-arm pattern — `"a" | "b"` gives
/// two names. Anything that isn't a plain identifier-shaped literal is
/// skipped, so an arm matching on a computed string can't poison the set.
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
        // `"a" | "b", 2) => …` — the tuple pattern ends at the first `)`
        // (names are string literals, so none can contain one), and the arm
        // body that follows may itself be parenthesised.
        let Some((inside, after)) = rest.split_once(')') else {
            continue;
        };
        if !after.trim_start().starts_with("=>") {
            continue;
        }
        let Some((pat, arity)) = inside.rsplit_once(',') else {
            continue;
        };
        let arity = arity.trim();
        if arity.is_empty() || !arity.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        out.extend(names_in_pattern(pat));
    }
    out
}

/// Every name a bare `"name" =>` arm dispatches in `src`.
fn bare_dispatched(src: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in src.lines() {
        let line = line.trim();
        if !line.starts_with('"') {
            continue;
        }
        let Some((pat, _)) = line.split_once("=>") else {
            continue;
        };
        out.extend(names_in_pattern(pat));
    }
    out
}

/// Every builtin name reachable through `call_builtin`'s callable dispatch,
/// with the file it came from.
fn dispatched_names() -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&str, &str)> = Vec::new();
    for (file, src) in ARITY_DISPATCHERS {
        out.extend(arity_dispatched(src).into_iter().map(|n| (*file, n)));
    }
    let math = &CORE[CORE
        .find(UNARY_MATH_FN)
        .expect("core.rs no longer declares dispatch_unary_math — update this test")..];
    out.extend(bare_dispatched(math).into_iter().map(|n| ("core.rs", n)));
    out
}

#[test]
fn every_dispatched_builtin_is_known() {
    let dispatched = dispatched_names();
    assert!(
        dispatched.len() >= MIN_EXTRACTED,
        "only extracted {} dispatched names (floor is {MIN_EXTRACTED}) — the dispatch tables \
         were probably restructured and this scan no longer reads them",
        dispatched.len()
    );
    let missing: Vec<String> = dispatched
        .iter()
        .filter(|(_, n)| !is_known_builtin(n))
        .map(|(file, n)| format!("{n} ({file})"))
        .collect();
    assert!(
        missing.is_empty(),
        "dispatched builtins missing from KNOWN_BUILTIN_NAMES — each one is unusable as a \
         function value (`var f = {}` resolves to null): {missing:?}",
        missing[0].split(' ').next().unwrap_or_default()
    );
}

/// The `set*` family specifically: the gap this test was originally written
/// for. Kept as a named case so a regression there reads as itself.
#[test]
fn every_set_builtin_is_known() {
    let set_names: Vec<&str> = dispatched_names()
        .into_iter()
        .map(|(_, n)| n)
        .filter(|n| n.starts_with("set"))
        .collect();
    assert!(
        set_names.len() >= 17,
        "expected the whole set* family, found {set_names:?}"
    );
    let missing: Vec<&str> = set_names
        .into_iter()
        .filter(|n| !is_known_builtin(n))
        .collect();
    assert!(
        missing.is_empty(),
        "dispatched set builtins missing from KNOWN_BUILTIN_NAMES: {missing:?}",
    );
}

/// A duplicate entry is dead weight and hides a copy-paste: the list is a
/// linear scan, so a name listed twice is never noticed at runtime.
#[test]
fn the_known_list_has_no_duplicates() {
    let src = include_str!("../src/builtins/mod.rs");
    let start = src
        .find("const KNOWN_BUILTIN_NAMES: &[&str] = &[")
        .expect("KNOWN_BUILTIN_NAMES moved — update this test");
    let body = &src[start..start + src[start..].find("];").expect("unterminated list")];
    let mut names: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix('"')?.split('"').next())
        .collect();
    assert!(
        names.len() >= MIN_EXTRACTED,
        "only read {} entries out of KNOWN_BUILTIN_NAMES",
        names.len()
    );
    let total = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        total,
        names.len(),
        "KNOWN_BUILTIN_NAMES has duplicate entries"
    );
}
