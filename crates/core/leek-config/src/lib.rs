//! Explicit compilation configuration (#98).
//!
//! Four compiler settings are process-globals today: the active library
//! headers (`leek_prelude::activate_library`), the constants to fold
//! (`leek_prelude::activate_fold_constants`), the dynamically-registered
//! builtins (`leek_resolver::builtins::register_builtin_*`), and library
//! seeding (`leek_types::set_seed_library`). Each is a `static` somewhere,
//! so two compilations in one process cannot disagree about them, and a
//! query cache cannot key on them without hand-rolled generation counters.
//!
//! [`CompilationConfig`] is the value that replaces them: one plain,
//! `Clone`-able struct a caller builds once and threads through, so the
//! setting travels with the compilation rather than with the process.
//!
//! # Why this is its own crate
//!
//! The value has to be nameable from every layer — `leek-prelude` (core)
//! merges the headers, `leek-resolver` (middle) answers builtin lookups,
//! `leek-pipeline` (db) stores it on an input. The layering rule
//! (`cargo xtask check-layers`) forbids core from seeing db, so the type
//! cannot live in `leek-pipeline`, and putting it in any one of the
//! existing core crates would make every other consumer depend on that
//! crate's unrelated surface. Hence a leaf crate that depends on **nothing**
//! — no `leek-*`, no third-party crates.
//!
//! This crate is deliberately inert: it defines the configuration value and
//! nothing reads it yet. Later slices of the epic move each process-global
//! over to it one at a time.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

/// The set of library signature headers merged into every lowered file.
///
/// A bitset rather than a list of sources: the set of headers that exist is
/// closed and known at compile time (`leek_prelude::LEEKWARS_SRC` and
/// `leek_prelude::STDLIB_SRC` are the only two `&'static str` headers a
/// caller can activate), so naming them by bit keeps the config `Copy` and
/// makes "is this the same configuration?" a byte comparison.
///
/// Bit assignment follows how the headers are actually used:
///
/// - bit 0 is [`LEEKWARS`](Self::LEEKWARS): `leek_recipes`'s
///   `register_leekwars` is the only production caller of
///   `activate_library`, and it activates that header;
/// - bit 1 is [`STDLIB`](Self::STDLIB): the only other activation in the
///   workspace is `leek-prelude`'s own `tests/libraries.rs`, which activates
///   the stdlib header.
///
/// The implicit prelude (`leek_prelude::PRELUDE_SRC`) is **not** a bit here.
/// It is not a library: `merged_header(prelude_enabled)` takes it as a
/// parameter, and the parameter callers pass is
/// `FeatureFlags::prelude` — an experimental feature flag, which stays where
/// it is for the reason spelled out on [`CompilationConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LibrarySet(u8);

impl LibrarySet {
    /// No library header active — what the ordinary compile path uses.
    pub const NONE: Self = Self(0);
    /// The leek-wars game header, `leek_prelude::LEEKWARS_SRC` (bit 0).
    pub const LEEKWARS: Self = Self(0b0000_0001);
    /// The typed stdlib header, `leek_prelude::STDLIB_SRC` (bit 1).
    pub const STDLIB: Self = Self(0b0000_0010);

    /// Every bit this version of the type knows, for [`from_bits`](Self::from_bits).
    const KNOWN: u8 = Self::LEEKWARS.0 | Self::STDLIB.0;

    /// Whether every library in `other` is active in `self`.
    ///
    /// Subset, not intersection: `contains(NONE)` is always true, and a
    /// one-library set never contains a two-library one.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Activate every library in `other`, and report whether that changed
    /// anything.
    ///
    /// The `bool` is the caller's cue that derived work (a merged header,
    /// the parse of it) is now stale — the same distinction
    /// `leek_prelude::activate_library` draws when it bumps its generation
    /// counter only on a real change.
    pub const fn insert(&mut self, other: Self) -> bool {
        let merged = self.0 | other.0;
        let changed = merged != self.0;
        self.0 = merged;
        changed
    }

    /// The raw bits, for storing the set as a plain scalar (a salsa input
    /// field, say) next to the existing `flags_bits`.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Rebuild a set from [`bits`](Self::bits).
    ///
    /// Bits this version does not know are dropped, so `from_bits` is total
    /// and `from_bits(s.bits()) == s` holds for every set this version can
    /// build. A stored byte written by a future version therefore degrades
    /// to the libraries this one understands, rather than to a set that
    /// claims a library it cannot name.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & Self::KNOWN)
    }
}

/// The set of constant catalogs folded to literals during HIR lowering.
///
/// A bitset rather than the `name → value` map the folding pass actually
/// wants, because the catalogs are static data, not caller input:
/// `leek_environment::leekwars_constant_values()` returns
/// `Vec<(&'static str, &'static str)>` — pairs borrowed from a table baked
/// into the binary. Every production activation folds exactly that catalog
/// (`leek_recipes::activate_leekwars_constant_folding`, which is what
/// `leekc --fold-constants` drives), so a bit selecting the catalog is
/// faithful, and the map stays derivable from it rather than being a second
/// copy of the same data in the config.
///
/// If a caller ever needs to fold constants that are *not* one of these
/// catalogs, that is a new field (an explicit map) next to this one, not a
/// new bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FoldSet(u8);

impl FoldSet {
    /// Fold nothing — constants stay identifiers for the backend to resolve.
    pub const NONE: Self = Self(0);
    /// The leek-wars fight constants, `WEAPON_PISTOL` → `37` (bit 0).
    pub const LEEKWARS: Self = Self(0b0000_0001);

    /// Every bit this version of the type knows, for [`from_bits`](Self::from_bits).
    const KNOWN: u8 = Self::LEEKWARS.0;

    /// Whether every catalog in `other` is folded in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Fold every catalog in `other` too, and report whether that changed
    /// anything.
    pub const fn insert(&mut self, other: Self) -> bool {
        let merged = self.0 | other.0;
        let changed = merged != self.0;
        self.0 = merged;
        changed
    }

    /// The raw bits, for storing the set as a plain scalar.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Rebuild a set from [`bits`](Self::bits); unknown bits are dropped,
    /// exactly as in [`LibrarySet::from_bits`].
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & Self::KNOWN)
    }
}

/// Builtins registered by the host rather than baked into the resolver.
///
/// The shape is copied from the resolver's private `DynamicBuiltins`
/// registry (`leek_resolver::builtins`), field for field, so that moving a
/// host's registrations into the config is a move and not a translation.
/// The resolver keeps its own private copy for now; a later slice makes this
/// one the only one.
///
/// Fields are public because building one is the whole point: a host fills
/// in what it registers and hands it over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DynamicBuiltins {
    /// Every name that resolves as a builtin — the union of
    /// [`constants`](Self::constants), [`functions`](Self::functions) keys,
    /// and names registered for visibility alone.
    pub names: HashSet<String>,
    /// Builtin constant names (`WEAPON_PISTOL`).
    pub constants: HashSet<String>,
    /// Builtin functions, `name → (min_args, max_args, min_version)`.
    pub functions: HashMap<String, (u8, u8, u8)>,
    /// Importable libraries, `name → exported symbols`. The symbol order is
    /// the registration order and is part of the value's identity.
    pub libraries: HashMap<String, Vec<String>>,
}

impl DynamicBuiltins {
    /// Whether nothing at all is registered — the fast path for the default
    /// compile, where the resolver need only consult its static tables.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
            && self.constants.is_empty()
            && self.functions.is_empty()
            && self.libraries.is_empty()
    }

    /// A 64-bit digest of everything registered, for cheap change detection
    /// (a cache key, an "is this still the registry I compiled against?"
    /// check).
    ///
    /// Order-independent: the `HashSet`/`HashMap` fields have no meaningful
    /// iteration order, so keys are sorted before hashing and two registries
    /// that compare equal always digest equal. Ordering *within* a library's
    /// symbol list is kept, because that list is a `Vec` and two different
    /// orders are two different values under `PartialEq`.
    ///
    /// The digest is a hash, not an identity: distinct registries can
    /// collide (rarely), and the value is only comparable within one process
    /// run — `DefaultHasher`'s algorithm is explicitly not stable across
    /// Rust releases, so never persist it. Compare with `==` where a
    /// collision would be a correctness bug.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash_into(&mut hasher);
        hasher.finish()
    }

    /// Feed the registry into `hasher` in a canonical (sorted) order.
    fn hash_into(&self, hasher: &mut DefaultHasher) {
        hash_sorted_names(&self.names, hasher);
        hash_sorted_names(&self.constants, hasher);

        let mut functions: Vec<(&str, (u8, u8, u8))> = self
            .functions
            .iter()
            .map(|(name, &meta)| (name.as_str(), meta))
            .collect();
        functions.sort_unstable();
        functions.hash(hasher);

        let mut libraries: Vec<(&str, &[String])> = self
            .libraries
            .iter()
            .map(|(name, symbols)| (name.as_str(), symbols.as_slice()))
            .collect();
        libraries.sort_unstable_by_key(|&(name, _)| name);
        libraries.hash(hasher);
    }
}

/// Hash a name set in sorted order, so iteration order cannot reach the
/// digest. Slices hash their length first, which keeps the fields of a
/// registry from running together.
fn hash_sorted_names(names: &HashSet<String>, hasher: &mut DefaultHasher) {
    let mut sorted: Vec<&str> = names.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.hash(hasher);
}

/// Everything a compilation needs to know beyond its sources and their
/// feature flags.
///
/// One value, built by whoever drives the compile (a CLI, the LSP, a test)
/// and threaded down, replacing four process-globals — see the crate docs.
/// [`Default`] is the ordinary compile: no libraries, no folding, no
/// seeding, nothing registered. Later code is expected to write
/// `CompilationConfig { libraries: LibrarySet::LEEKWARS, ..Default::default() }`,
/// so that invariant is pinned by a test here.
///
/// # `FeatureFlags` is deliberately not a field
///
/// The experimental `FeatureFlags` already travel with the compilation:
/// `leek_pipeline::Input::flags` carries them in, and
/// `SourceFile::flags_bits` is the salsa input every pass reads them back
/// from (`leek_parser`, `leek_hir`, `leek_resolver` and `leek_types` all do
/// `FeatureFlags::from_bits(file.flags_bits(db))`). They are not a
/// process-global and so are not this crate's problem. Copying them in here
/// would create a second source of truth, and the first question would be
/// which one wins when they disagree — a question with no good answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompilationConfig {
    /// Library headers merged into every file.
    pub libraries: LibrarySet,
    /// Constant catalogs folded to literals during lowering.
    pub fold: FoldSet,
    /// Seed the type checker with the library's signatures — the explicit
    /// form of `leek_types::set_seed_library`, which the LSP sets so hover
    /// and inference see library functions.
    pub seed_library: bool,
    /// Builtins the host registered. `Arc` because the registry is large,
    /// shared by every file in a compile, and cloned far more often than it
    /// is built.
    pub builtins: Arc<DynamicBuiltins>,
}

impl CompilationConfig {
    /// A 64-bit digest of the whole configuration, combining every field
    /// with [`DynamicBuiltins::fingerprint`].
    ///
    /// Equal configurations digest equal; the caveats on the registry
    /// digest (collisions possible, process-local, never persist it) apply
    /// unchanged.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.libraries.bits().hash(&mut hasher);
        self.fold.bits().hash(&mut hasher);
        self.seed_library.hash(&mut hasher);
        self.builtins.hash_into(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both libraries active, built through the public API.
    fn both_libraries() -> LibrarySet {
        let mut set = LibrarySet::NONE;
        set.insert(LibrarySet::LEEKWARS);
        set.insert(LibrarySet::STDLIB);
        set
    }

    #[test]
    fn library_set_round_trips_through_its_bits() {
        for set in [
            LibrarySet::NONE,
            LibrarySet::LEEKWARS,
            LibrarySet::STDLIB,
            both_libraries(),
        ] {
            assert_eq!(LibrarySet::from_bits(set.bits()), set);
        }
        assert_eq!(LibrarySet::NONE.bits(), 0);
        assert_eq!(LibrarySet::LEEKWARS.bits(), 0b0000_0001);
        assert_eq!(LibrarySet::STDLIB.bits(), 0b0000_0010);
    }

    #[test]
    fn library_set_drops_bits_it_does_not_know() {
        assert_eq!(LibrarySet::from_bits(0b1111_1100), LibrarySet::NONE);
        assert_eq!(LibrarySet::from_bits(0b1111_1111), both_libraries());
    }

    #[test]
    fn library_set_contains_is_a_subset_test() {
        let both = both_libraries();
        assert!(both.contains(LibrarySet::LEEKWARS));
        assert!(both.contains(LibrarySet::STDLIB));
        assert!(!LibrarySet::LEEKWARS.contains(both));
        assert!(!LibrarySet::LEEKWARS.contains(LibrarySet::STDLIB));
        // The empty set is contained in everything, including itself.
        assert!(LibrarySet::NONE.contains(LibrarySet::NONE));
        assert!(LibrarySet::LEEKWARS.contains(LibrarySet::NONE));
    }

    #[test]
    fn library_set_insert_reports_only_real_changes() {
        let mut set = LibrarySet::NONE;
        assert!(set.insert(LibrarySet::LEEKWARS));
        assert!(
            !set.insert(LibrarySet::LEEKWARS),
            "re-activating an active library must not claim a change"
        );
        assert!(!set.insert(LibrarySet::NONE));
        assert!(set.insert(both_libraries()));
        assert_eq!(set, both_libraries());
    }

    #[test]
    fn fold_set_round_trips_through_its_bits() {
        for set in [FoldSet::NONE, FoldSet::LEEKWARS] {
            assert_eq!(FoldSet::from_bits(set.bits()), set);
        }
        assert_eq!(FoldSet::NONE.bits(), 0);
        assert_eq!(FoldSet::LEEKWARS.bits(), 0b0000_0001);
        assert_eq!(FoldSet::from_bits(0b1111_1110), FoldSet::NONE);
        assert_eq!(FoldSet::from_bits(0b1111_1111), FoldSet::LEEKWARS);
    }

    #[test]
    fn fold_set_insert_and_contains_agree() {
        let mut set = FoldSet::NONE;
        assert!(!set.contains(FoldSet::LEEKWARS));
        assert!(set.insert(FoldSet::LEEKWARS));
        assert!(set.contains(FoldSet::LEEKWARS));
        assert!(!set.insert(FoldSet::LEEKWARS));
    }

    /// The same registry, filled in in the given order. Enough entries that
    /// a lucky hash-order coincidence cannot pass the order test by itself.
    fn registry(reversed: bool) -> DynamicBuiltins {
        let mut names = ["alpha", "beta", "gamma", "delta", "epsilon"];
        let mut constants = ["WEAPON_PISTOL", "CHIP_BANDAGE", "AREA_CIRCLE_2"];
        if reversed {
            names.reverse();
            constants.reverse();
        }

        let mut builtins = DynamicBuiltins::default();
        for name in names {
            builtins.names.insert(name.to_string());
        }
        for name in constants {
            builtins.constants.insert(name.to_string());
            builtins.names.insert(name.to_string());
        }

        let mut functions = [("getLife", (0, 1, 1)), ("moveToward", (1, 2, 1))];
        if reversed {
            functions.reverse();
        }
        for (name, meta) in functions {
            builtins.functions.insert(name.to_string(), meta);
        }

        let mut libraries = [
            ("fight.generator", ["fightGenerate", "fightSeed"]),
            ("fight_generator", ["fightGenerate", "fightSeed"]),
        ];
        if reversed {
            libraries.reverse();
        }
        for (name, symbols) in libraries {
            builtins.libraries.insert(
                name.to_string(),
                symbols.iter().map(|s| (*s).to_string()).collect(),
            );
        }
        builtins
    }

    #[test]
    fn dynamic_builtins_fingerprint_ignores_insertion_order() {
        let forwards = registry(false);
        let backwards = registry(true);
        assert_eq!(forwards, backwards, "the two orders must build one value");
        assert_eq!(forwards.fingerprint(), backwards.fingerprint());
    }

    #[test]
    fn dynamic_builtins_fingerprint_moves_with_content() {
        let base = registry(false);
        let baseline = base.fingerprint();

        let mut extra_name = base.clone();
        extra_name.names.insert("zeta".to_string());
        assert_ne!(extra_name.fingerprint(), baseline);

        let mut extra_constant = base.clone();
        extra_constant.constants.insert("CHIP_SHOCK".to_string());
        assert_ne!(extra_constant.fingerprint(), baseline);

        let mut wider_arity = base.clone();
        wider_arity
            .functions
            .insert("getLife".to_string(), (0, 2, 1));
        assert_ne!(wider_arity.fingerprint(), baseline);

        let mut extra_symbol = base.clone();
        extra_symbol
            .libraries
            .get_mut("fight.generator")
            .expect("library registered above")
            .push("fightPreview".to_string());
        assert_ne!(extra_symbol.fingerprint(), baseline);
    }

    #[test]
    fn dynamic_builtins_fingerprint_keeps_library_symbol_order() {
        // Symbol order is part of `PartialEq`, so it has to be part of the
        // digest too — otherwise equal digests would not imply equal values.
        let mut forwards = DynamicBuiltins::default();
        forwards.libraries.insert(
            "fight.generator".to_string(),
            vec!["fightGenerate".to_string(), "fightSeed".to_string()],
        );
        let mut backwards = DynamicBuiltins::default();
        backwards.libraries.insert(
            "fight.generator".to_string(),
            vec!["fightSeed".to_string(), "fightGenerate".to_string()],
        );

        assert_ne!(forwards, backwards);
        assert_ne!(forwards.fingerprint(), backwards.fingerprint());
    }

    #[test]
    fn default_config_is_the_ordinary_compile() {
        // Every later slice writes `..Default::default()` and expects this.
        let config = CompilationConfig::default();
        assert_eq!(config.libraries, LibrarySet::NONE);
        assert_eq!(config.fold, FoldSet::NONE);
        assert!(!config.seed_library);
        assert!(config.builtins.is_empty());
        assert_eq!(*config.builtins, DynamicBuiltins::default());
        assert_eq!(config, CompilationConfig::default());
        assert_eq!(
            config.fingerprint(),
            CompilationConfig::default().fingerprint()
        );
    }

    #[test]
    fn config_fingerprint_moves_with_every_field() {
        let base = CompilationConfig::default();
        let variants = [
            CompilationConfig {
                libraries: LibrarySet::LEEKWARS,
                ..Default::default()
            },
            CompilationConfig {
                fold: FoldSet::LEEKWARS,
                ..Default::default()
            },
            CompilationConfig {
                seed_library: true,
                ..Default::default()
            },
            CompilationConfig {
                builtins: Arc::new(registry(false)),
                ..Default::default()
            },
        ];

        for (i, variant) in variants.iter().enumerate() {
            assert_ne!(variant, &base, "variant {i} must differ from the default");
            assert_ne!(
                variant.fingerprint(),
                base.fingerprint(),
                "variant {i} must digest differently from the default"
            );
            for other in &variants[i + 1..] {
                assert_ne!(variant.fingerprint(), other.fingerprint());
            }
        }
    }

    #[test]
    fn config_clone_is_equal_and_digests_equal() {
        let config = CompilationConfig {
            libraries: both_libraries(),
            fold: FoldSet::LEEKWARS,
            seed_library: true,
            builtins: Arc::new(registry(false)),
        };
        let clone = config.clone();
        assert_eq!(config, clone);
        assert_eq!(config.fingerprint(), clone.fingerprint());

        // An independently built registry with the same content is the same
        // configuration, `Arc` identity notwithstanding.
        let rebuilt = CompilationConfig {
            builtins: Arc::new(registry(true)),
            ..config.clone()
        };
        assert_eq!(config, rebuilt);
        assert_eq!(config.fingerprint(), rebuilt.fingerprint());
    }
}
