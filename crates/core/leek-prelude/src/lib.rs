//! Embedded experimental Leekscript library headers.
//!
//! Two consumers share these assets and the dependency graph forbids
//! one from reaching into the other (`leek-hir` depends on `leek-types`),
//! so the source strings live here in a leaf crate:
//!
//! - [`PRELUDE_SRC`] — the hand-written starter with `@java-backend` /
//!   `@native-backend` directives; **`leek-hir`** merges it for codegen
//!   so its builtins lower on every backend.
//! - [`STDLIB_SRC`] / [`LEEKWARS_SRC`] — the generated *typed* signature
//!   headers (named params, Doxygen docs, and the manual generic pass:
//!   `push<T>(Array<T>, T)`, `first<T>(Array<T>) -> T`, …). **`leek-types`**
//!   loads `STDLIB_SRC` so library calls infer precise return types in
//!   user code.
//!
//! All loading is gated behind [`enabled`] (the `LEEK_EXPERIMENTAL_PRELUDE`
//! environment variable) so the default compile path and the corpus
//! baseline are unchanged.

use leek_span::SourceId;

/// Hand-written prelude with backend directives (codegen view).
pub const PRELUDE_SRC: &str = include_str!("prelude.leek");

/// Generated typed standard-library signatures (inference view).
pub const STDLIB_SRC: &str = include_str!("stdlib.leek");

/// Generated typed leek-wars game-function signatures.
pub const LEEKWARS_SRC: &str = include_str!("leekwars.leek");

/// Distinct source id for library spans so they never collide with the
/// user file (conventionally id 1).
pub fn source_id() -> SourceId {
    SourceId::new(0xF00D).expect("nonzero")
}

use std::sync::Mutex;

/// Process-global set of library signature headers to merge into every
/// lowered file (their bodiless functions + `@<backend>-dispatch:`
/// directives become available, exactly like the implicit prelude).
/// Populated by a driver when a `--library` is requested — e.g.
/// `--library leekwars` activates [`LEEKWARS_SRC`] so combat functions
/// dispatch through their directives. Mirrors how the resolver tracks
/// dynamically-registered builtins process-globally.
static ACTIVE_LIBRARIES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Activate a library header for HIR merge. Idempotent.
pub fn activate_library(src: &'static str) {
    let mut libs = ACTIVE_LIBRARIES.lock().expect("library lock");
    if !libs.iter().any(|s| std::ptr::eq(*s, src)) {
        libs.push(src);
    }
}

/// The combined source of all active library headers (and, when
/// [`enabled`], the implicit [`PRELUDE_SRC`]), or `None` when nothing is
/// active. Lowering parses this as a prelude and merges it ahead of the
/// user file.
pub fn merged_header_src(prelude_enabled: bool) -> Option<String> {
    let libs = ACTIVE_LIBRARIES.lock().expect("library lock");
    let mut parts: Vec<&str> = Vec::new();
    if prelude_enabled {
        parts.push(PRELUDE_SRC);
    }
    parts.extend(libs.iter().copied());
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Process-global `name → value-string` map of constants to fold to
/// literals during HIR lowering. Populated by a driver when constant
/// folding is requested (e.g. `leekc --fold-constants` with the leek-wars
/// constant values). Values are kept as strings so this leaf crate stays
/// free of any IR dependency; the HIR lowerer parses each into a literal
/// (a `.`-bearing value → real, else integer). Empty ⇒ folding is off, so
/// the default path and the corpus baseline are unchanged.
static FOLD_CONSTANTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Register constants to fold (`("WEAPON_PISTOL", "37")`). Idempotent per
/// name; later registrations for the same name win.
pub fn activate_fold_constants<I, K, V>(pairs: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut map = FOLD_CONSTANTS.lock().expect("fold lock");
    for (k, v) in pairs {
        let (k, v) = (k.into(), v.into());
        if let Some(slot) = map.iter_mut().find(|(n, _)| *n == k) {
            slot.1 = v;
        } else {
            map.push((k, v));
        }
    }
}

/// Snapshot the active fold constants as `(name, value-string)` pairs.
/// Empty when folding isn't active.
pub fn fold_constants() -> Vec<(String, String)> {
    FOLD_CONSTANTS.lock().expect("fold lock").clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_spans_cannot_be_mistaken_for_a_real_or_sentinel_source() {
        // Library spans are reported against the user's file only if this
        // id collides with one, so it must miss every id anyone else hands
        // out: `ProjectIndex` counts up from 1, and leek-span reserves the
        // top two for the manifest and for "no location".
        let id = source_id();
        assert_eq!(id.get(), 0xF00D);
        assert_ne!(id, leek_span::SourceId::new(1).expect("nonzero"));
        assert_ne!(id, leek_span::Span::MANIFEST_SOURCE);
        assert_ne!(id, leek_span::Span::SYNTHETIC_SOURCE);
    }
}
