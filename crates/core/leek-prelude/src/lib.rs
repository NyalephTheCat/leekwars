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
//! All loading is gated behind [`FeatureFlags::prelude`](leek_span::FeatureFlags::prelude)
//! (the `LEEK_EXPERIMENTAL_PRELUDE` environment variable) so the default
//! compile path and the corpus baseline are unchanged.
//!
//! Which headers and which fold constants are active is process-global
//! mutable state, so everything derived from it is versioned by
//! [`generation`]: [`merged_header`] and [`fold_constants_cached`] hand
//! back a shared snapshot plus the generation it was taken at, and any
//! consumer that caches further work (a parse, a lowering) stores that
//! number with it and re-runs once it no longer matches.

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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Monotonic counter bumped whenever the active libraries or the active
/// fold constants actually change.
///
/// It is the cache key for everything derived from that state: comparing
/// one integer replaces re-joining (and then content-hashing) a ~64 KB
/// header on every file. It is equally the *invalidation* key for results
/// that outlive a call — a consumer that recorded generation `g` and now
/// reads a different one must re-run, so a `--library` activated after a
/// query cannot leave that query silently serving HIR lowered without the
/// library in it.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The current library / fold-constant generation.
///
/// Sample it *before* the work whose result you intend to keep, and store
/// it next to that result; a later sample that still matches means no
/// `activate_*` call landed in between. [`merged_header`] and
/// [`fold_constants_cached`] return the generation of their snapshot for
/// exactly this purpose.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// Process-global set of library signature headers to merge into every
/// lowered file (their bodiless functions + `@<backend>-dispatch:`
/// directives become available, exactly like the implicit prelude).
/// Populated by a driver when a `--library` is requested — e.g.
/// `--library leekwars` activates [`LEEKWARS_SRC`] so combat functions
/// dispatch through their directives. Mirrors how the resolver tracks
/// dynamically-registered builtins process-globally.
static ACTIVE_LIBRARIES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Memoized [`merged_header`] answers, indexed by `prelude_enabled`
/// (slot 0 = off, slot 1 = on) and tagged with the [`generation`] each was
/// built at. The inner `Option` is the "nothing is active" answer, which is
/// worth memoizing too: it is what the default compile path asks for on
/// every single file.
static MERGED_HEADER: Mutex<[Option<(u64, Option<Arc<str>>)>; 2]> = Mutex::new([None, None]);

/// Activate a library header for HIR merge. Idempotent — and idempotent
/// *without* bumping [`generation`], so a driver that re-requests the same
/// `--library` does not discard a merged header (nor, downstream, the parse
/// of it) that is still correct.
pub fn activate_library(src: &'static str) {
    let mut libs = ACTIVE_LIBRARIES.lock().expect("library lock");
    if !libs.iter().any(|s| std::ptr::eq(*s, src)) {
        libs.push(src);
        // Bumped while the lock is still held, so a reader that samples the
        // counter under that same lock can never pair the new library set
        // with the generation from before the push.
        GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

/// The combined source of all active library headers (and, when
/// `prelude_enabled`, the implicit [`PRELUDE_SRC`]), or `None` when nothing
/// is active — paired with the [`generation`] it was built at. Callers pass
/// [`FeatureFlags::prelude`](leek_span::FeatureFlags::prelude) for the
/// argument. Lowering parses this as a prelude and merges it ahead of the
/// user file.
///
/// The join is memoized per `(prelude_enabled, generation)`, so repeated
/// calls hand back the very same `Arc` instead of re-concatenating ~64 KB
/// — which matters because the first thing done with the result is a
/// content-hash lookup in `leek_parser::parse_signature_header`'s cache.
/// Keep the returned generation beside anything derived from the text:
/// once [`generation`] has moved past it, the derived value is stale.
pub fn merged_header(prelude_enabled: bool) -> Option<(Arc<str>, u64)> {
    let slot = usize::from(prelude_enabled);
    let generation = generation();
    if let Some((built_at, text)) = cached_header(slot)
        && built_at == generation
    {
        return text.map(|text| (text, generation));
    }
    // Built under the library lock alone, then published under the cache
    // lock alone. Neither is ever held across the other, so the two need no
    // lock order between them. Poisoning stays what this crate already does
    // with it — `expect` — rather than adding a fourth policy (#176).
    let (generation, built) = build_merged_header(prelude_enabled);
    publish_header(slot, generation, built)
}

/// The memoized entry for `slot`, if there is one: the generation it was
/// built at, and the answer as of then (`None` = nothing was active).
fn cached_header(slot: usize) -> Option<(u64, Option<Arc<str>>)> {
    MERGED_HEADER.lock().expect("merged header cache lock")[slot].clone()
}

/// Join the active headers, and report the generation they were read at.
fn build_merged_header(prelude_enabled: bool) -> (u64, Option<Arc<str>>) {
    let libs = ACTIVE_LIBRARIES.lock().expect("library lock");
    let generation = GENERATION.load(Ordering::Acquire);
    let mut parts: Vec<&str> = Vec::new();
    if prelude_enabled {
        parts.push(PRELUDE_SRC);
    }
    parts.extend(libs.iter().copied());
    let joined = if parts.is_empty() {
        None
    } else {
        Some(Arc::<str>::from(parts.join("\n")))
    };
    (generation, joined)
}

/// Store a freshly built header and return whichever entry won, so two
/// builders racing at the same generation still agree on one `Arc`. A build
/// that lost a race to a *newer* generation does not clobber it.
fn publish_header(
    slot: usize,
    generation: u64,
    built: Option<Arc<str>>,
) -> Option<(Arc<str>, u64)> {
    let mut cache = MERGED_HEADER.lock().expect("merged header cache lock");
    let entry = &mut cache[slot];
    match entry {
        Some((built_at, _)) if *built_at >= generation => {}
        _ => *entry = Some((generation, built)),
    }
    let (built_at, text) = entry.as_ref().expect("just filled");
    text.clone().map(|text| (text, *built_at))
}

/// The combined source of all active library headers (and, when
/// `prelude_enabled`, the implicit [`PRELUDE_SRC`]), or `None` when
/// nothing is active.
///
/// The owned-`String` view of [`merged_header`]; prefer that one wherever
/// the caller can hold the `Arc`, since this copies the ~64 KB back out.
pub fn merged_header_src(prelude_enabled: bool) -> Option<String> {
    merged_header(prelude_enabled).map(|(text, _)| text.to_string())
}

/// Process-global `name → value-string` map of constants to fold to
/// literals during HIR lowering. Populated by a driver when constant
/// folding is requested (e.g. `leekc --fold-constants` with the leek-wars
/// constant values). Values are kept as strings so this leaf crate stays
/// free of any IR dependency; the HIR lowerer parses each into a literal
/// (a `.`-bearing value → real, else integer). Empty ⇒ folding is off, so
/// the default path and the corpus baseline are unchanged.
static FOLD_CONSTANTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Memoized [`fold_constants_cached`] snapshot, tagged with the
/// [`generation`] it was taken at.
static FOLD_SNAPSHOT: Mutex<Option<(u64, Arc<Vec<(String, String)>>)>> = Mutex::new(None);

/// Register constants to fold (`("WEAPON_PISTOL", "37")`). Idempotent per
/// name; later registrations for the same name win. [`generation`] is
/// bumped only when a name is added or a value really changes, so
/// re-registering an identical set costs downstream caches nothing.
pub fn activate_fold_constants<I, K, V>(pairs: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut map = FOLD_CONSTANTS.lock().expect("fold lock");
    let mut changed = false;
    for (k, v) in pairs {
        let (k, v) = (k.into(), v.into());
        if let Some(slot) = map.iter_mut().find(|(n, _)| *n == k) {
            if slot.1 != v {
                slot.1 = v;
                changed = true;
            }
        } else {
            map.push((k, v));
            changed = true;
        }
    }
    if changed {
        // Under the lock, for the reason given in `activate_library`.
        GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

/// A shared snapshot of the active fold constants as `(name, value-string)`
/// pairs, with the [`generation`] it was taken at. The snapshot is empty
/// when folding isn't active.
///
/// A lowering run should take this once and hold the `Arc` for its whole
/// duration rather than call [`fold_constants`] per file: that one re-locks
/// the mutex and deep-copies every pair on each call.
pub fn fold_constants_cached() -> (Arc<Vec<(String, String)>>, u64) {
    let generation = generation();
    {
        let cache = FOLD_SNAPSHOT.lock().expect("fold snapshot lock");
        if let Some((taken_at, pairs)) = &*cache
            && *taken_at == generation
        {
            return (Arc::clone(pairs), generation);
        }
    }
    // As in `merged_header`: the constants lock and the snapshot lock are
    // taken one after the other, never nested.
    let (generation, pairs) = {
        let map = FOLD_CONSTANTS.lock().expect("fold lock");
        (GENERATION.load(Ordering::Acquire), Arc::new(map.clone()))
    };
    let mut cache = FOLD_SNAPSHOT.lock().expect("fold snapshot lock");
    match &*cache {
        Some((taken_at, _)) if *taken_at >= generation => {}
        _ => *cache = Some((generation, pairs)),
    }
    let (taken_at, pairs) = cache.as_ref().expect("just filled");
    (Arc::clone(pairs), *taken_at)
}

/// Snapshot the active fold constants as `(name, value-string)` pairs.
/// Empty when folding isn't active.
///
/// The owned-`Vec` view of [`fold_constants_cached`]; prefer that one
/// wherever the caller can hold the `Arc`.
pub fn fold_constants() -> Vec<(String, String)> {
    fold_constants_cached().0.as_ref().clone()
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
