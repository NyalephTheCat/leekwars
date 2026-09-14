//! Deterministic path → [`SourceId`] interning for include resolution.
//!
//! The include walker needs an id for every file it discovers, and the
//! same file reached twice — through two includers, or through two
//! spellings of its path — must come back with the *same* id or the
//! spans it produces name two different sources. A plain counter gets
//! that wrong as soon as more than one entry file is compiled in one
//! process: `miku test` numbers its test files 1, 2, 3, … while each
//! file's includes take the ids just above the entry's, so file 2's
//! own id is also file 1's first include's id.
//!
//! An interner fixes both halves: it is keyed by
//! [`canonical_or_normalized`] (the one path rule the include graph,
//! the folder and the driver already share, see `docs/semantics.md`
//! §1), and it is a *value* the caller owns, so every file compiled in
//! one run can share a single id space by sharing one interner.
//!
//! [`SourceInterner`] is the interface the pipeline step depends on;
//! [`PathInterner`] is the only implementation the toolchain needs.
//! Interning takes `&self` so the step can hold the interner behind an
//! [`Arc`](std::sync::Arc) and stay `Send + Sync` without the caller
//! threading a mutable borrow through the pipeline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use leek_span::SourceId;
use leek_span::paths::canonical_or_normalized;

/// Hands out a [`SourceId`] for a file path.
///
/// Implementations must be idempotent: interning the same file twice
/// returns the same id for the interner's lifetime.
pub trait SourceInterner: Send + Sync {
    /// The id for `path`, allocating one if this is the first time the
    /// interner has seen it.
    fn intern(&self, path: &Path) -> SourceId;
}

/// The toolchain's [`SourceInterner`]: a canonical-path map plus a
/// monotonic counter.
///
/// Ids are handed out in first-seen order starting from the value the
/// constructor was given, and never reused for a different path.
pub struct PathInterner {
    state: Mutex<State>,
}

/// The map and the counter, together under one lock so a concurrent
/// pair of [`PathInterner::intern`] calls cannot hand the same id to
/// two different paths.
struct State {
    ids: BTreeMap<PathBuf, SourceId>,
    /// The id the next unseen path gets.
    next: u32,
}

impl State {
    /// Take the next id and advance the counter.
    fn fresh(&mut self) -> SourceId {
        let id = SourceId::new(self.next).expect("source id counter is never zero");
        self.next = self.next.saturating_add(1);
        id
    }

    /// Keep `next` strictly above an id that arrived from outside the
    /// counter, so a later [`PathInterner::intern`] cannot reissue it.
    fn advance_past(&mut self, id: SourceId) {
        self.next = self.next.max(id.get().saturating_add(1));
    }
}

impl PathInterner {
    /// An empty interner whose first path gets [`SourceId`] 1.
    #[must_use]
    pub fn new() -> Self {
        Self::starting_at(1)
    }

    /// An empty interner whose first path gets id `start`.
    ///
    /// `start` is clamped to 1 because [`SourceId`] rejects zero. Use
    /// this when the entry file's id is already fixed by the caller's
    /// `Input` and its includes must follow on from it.
    #[must_use]
    pub fn starting_at(start: u32) -> Self {
        Self {
            state: Mutex::new(State {
                ids: BTreeMap::new(),
                next: start.max(1),
            }),
        }
    }

    /// Bind `path` to `id`, replacing any id it already had, and keep
    /// the counter above `id`.
    ///
    /// For ids minted elsewhere that the interner must agree with: the
    /// LSP's salsa inputs are created when a document is opened or
    /// indexed, and the include graph has to name those files by the
    /// ids salsa already gave them.
    pub fn assign(&self, path: &Path, id: SourceId) {
        let mut state = self.lock();
        state.ids.insert(canonical_or_normalized(path), id);
        state.advance_past(id);
    }

    /// A fresh id bound to no path, for a source that has no path at
    /// all (an unsaved editor buffer) but must not collide with the
    /// interned ones.
    pub fn reserve(&self) -> SourceId {
        self.lock().fresh()
    }

    /// The lock, recovering from poisoning: nothing under it can panic,
    /// and a source id is not a value a caller can decline to produce.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for PathInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceInterner for PathInterner {
    fn intern(&self, path: &Path) -> SourceId {
        let key = canonical_or_normalized(path);
        let mut state = self.lock();
        if let Some(id) = state.ids.get(&key) {
            return *id;
        }
        let id = state.fresh();
        state.ids.insert(key, id);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_path_interns_to_the_same_id() {
        let interner = PathInterner::new();
        let first = interner.intern(Path::new("/a.leek"));
        assert_eq!(interner.intern(Path::new("/a.leek")), first);
    }

    /// The property the include walker depends on: an id belongs to a
    /// path, not to the order the walker happened to reach it in. Two
    /// interners fed the same paths in opposite orders disagree about
    /// *which* id each path got, but each interner must stay
    /// self-consistent — and the ids must be distinct per path.
    #[test]
    fn two_orders_give_each_path_one_stable_id() {
        let forward = PathInterner::new();
        let a_first = forward.intern(Path::new("/a.leek"));
        let b_second = forward.intern(Path::new("/b.leek"));

        let backward = PathInterner::new();
        let b_first = backward.intern(Path::new("/b.leek"));
        let a_second = backward.intern(Path::new("/a.leek"));

        assert_ne!(a_first, b_second, "distinct paths get distinct ids");
        assert_ne!(b_first, a_second, "distinct paths get distinct ids");
        assert_eq!(forward.intern(Path::new("/a.leek")), a_first);
        assert_eq!(forward.intern(Path::new("/b.leek")), b_second);
        assert_eq!(backward.intern(Path::new("/b.leek")), b_first);
        assert_eq!(backward.intern(Path::new("/a.leek")), a_second);
    }

    /// Two spellings of one file are one file: the include graph keys
    /// on [`canonical_or_normalized`], so the interner must too, or a
    /// file reached as `dir/../dir/x.leek` gets a second id.
    #[test]
    fn two_spellings_of_one_path_share_an_id() {
        let interner = PathInterner::new();
        let direct = interner.intern(Path::new("/dir/x.leek"));
        let indirect = interner.intern(Path::new("/dir/sub/../x.leek"));
        assert_eq!(direct, indirect);
    }

    #[test]
    fn a_counter_start_fixes_the_first_id() {
        let interner = PathInterner::starting_at(7);
        assert_eq!(interner.intern(Path::new("/a.leek")).get(), 7);
        assert_eq!(interner.intern(Path::new("/b.leek")).get(), 8);
    }

    #[test]
    fn an_assigned_id_wins_and_pushes_the_counter_past_itself() {
        let interner = PathInterner::new();
        interner.assign(Path::new("/a.leek"), SourceId::new(9).unwrap());
        assert_eq!(interner.intern(Path::new("/a.leek")).get(), 9);
        assert_eq!(
            interner.intern(Path::new("/b.leek")).get(),
            10,
            "a fresh path must not reuse an assigned id"
        );
    }

    #[test]
    fn a_reserved_id_is_never_handed_to_a_path() {
        let interner = PathInterner::new();
        let reserved = interner.reserve();
        assert_ne!(interner.intern(Path::new("/a.leek")), reserved);
    }
}
