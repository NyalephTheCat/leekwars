//! The fold constants a compilation asks for, parsed into literals.
//!
//! [`leek_environment`] keeps the leek-wars fight constants as
//! `name → value-string` pairs baked into the binary; turning those strings
//! into [`Literal`]s is this crate's job. It used to be done inside the
//! lowering pass, which re-parsed every value string on every file lowered
//! (#191).
//!
//! Which catalogs to fold now arrives as a [`FoldSet`] argument instead of
//! being read back out of `leek_prelude`'s process-global registrations
//! (#98, #226). That makes the map a pure function of a one-bit input, which
//! is what lets the salsa-tracked [`lower_hir_query`] be keyed on it — a
//! tracked query cannot see a generation counter move, so it could never be
//! keyed on the registry.
//!
//! [`lower_hir_query`]: crate::pipeline::lower_hir_query

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use leek_config::FoldSet;

use crate::ir::Literal;

/// Memoized [`fold_map`] answers, one slot per [`FoldSet`], indexed by
/// [`FoldSet::bits`].
///
/// A plain array of [`OnceLock`]s rather than the generation-keyed
/// [`Mutex`](std::sync::Mutex) this used to be: with the catalogs named by
/// the argument there is nothing left to invalidate, so a slot's answer is a
/// function of the slot's own index and stays correct once computed. Same
/// table shape, and the same reasoning, as `leek_prelude`'s
/// `MERGED_HEADER_FOR`.
static FOLD_MAP: [OnceLock<Arc<HashMap<String, Literal>>>; 2] = [const { OnceLock::new() }; 2];

/// The table above has a slot for every set [`FoldSet`] can build. A second
/// fold bit in `leek-config` would index past the end, so it has to widen
/// this table in the same change.
const _: () = assert!(
    FoldSet::from_bits(u8::MAX).bits() < 2,
    "leek-config gained a fold bit: widen FOLD_MAP accordingly"
);

/// The constants the catalogs in `fold` define, as `name → literal`, parsed
/// at most once per set per process and shared by [`Arc`].
///
/// Empty — and still `Arc`-stable — for [`FoldSet::NONE`], the default
/// compile path: an empty map makes
/// [`fold_constants`](crate::transform::fold_constants) a no-op.
///
/// A value string containing `.` parses as a real, anything else as an
/// integer; a value that parses as neither is dropped — exactly what the
/// lowering pass did with it inline.
pub fn fold_map(fold: FoldSet) -> Arc<HashMap<String, Literal>> {
    let slot = usize::from(fold.bits());
    Arc::clone(FOLD_MAP[slot].get_or_init(|| Arc::new(parse_constants(fold))))
}

/// Parse every catalog named in `fold` into one map.
fn parse_constants(fold: FoldSet) -> HashMap<String, Literal> {
    if !fold.contains(FoldSet::LEEKWARS) {
        return HashMap::new();
    }
    leek_environment::leekwars_constant_values()
        .into_iter()
        .filter_map(|(name, value)| Some((name.to_string(), parse_literal(value)?)))
        .collect()
}

/// Parse one registered value string: a `.`-bearing value is a real, any
/// other one an integer. `None` when it is neither.
fn parse_literal(value: &str) -> Option<Literal> {
    if value.contains('.') {
        value.parse::<f64>().ok().map(Literal::Real)
    } else {
        value.parse::<i64>().ok().map(Literal::Int)
    }
}
