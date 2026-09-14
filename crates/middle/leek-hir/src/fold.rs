//! The active fold constants, parsed into literals once per generation.
//!
//! [`leek_prelude`] keeps the registered constants as `name → value-string`
//! pairs so that leaf crate stays free of any IR dependency; turning those
//! strings into [`Literal`]s is this crate's job. It used to be done inside
//! the lowering pass, which re-parsed every value string on every file
//! lowered (#191). The parsed map is built at most once per
//! [`leek_prelude::generation`] instead — the same key the merged library
//! header is memoized on, and the one that moves when a driver registers a
//! new constant or changes a value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ir::Literal;

/// The parsed map, tagged with the [`leek_prelude::generation`] it was built
/// at. `None` until the first call.
static FOLD_MAP: Mutex<Option<(u64, Arc<HashMap<String, Literal>>)>> = Mutex::new(None);

/// The active fold constants as `name → literal`, shared and rebuilt only
/// when [`leek_prelude::generation`] moves. Empty when constant folding
/// isn't active, which is the default path.
///
/// A value string containing `.` parses as a real, anything else as an
/// integer; a value that parses as neither is dropped — exactly what the
/// lowering pass did with it inline.
pub fn fold_map() -> Arc<HashMap<String, Literal>> {
    let generation = leek_prelude::generation();
    {
        let cache = FOLD_MAP.lock().expect("fold map cache lock");
        if let Some((built_at, map)) = &*cache
            && *built_at == generation
        {
            return Arc::clone(map);
        }
    }
    // The snapshot reports the generation it was taken at, and that is what
    // the map must be tagged with: sampling the counter separately could pair
    // an older snapshot with a newer generation and pin it there.
    let (pairs, generation) = leek_prelude::fold_constants_cached();
    let map: Arc<HashMap<String, Literal>> = Arc::new(
        pairs
            .iter()
            .filter_map(|(name, value)| Some((name.clone(), parse_literal(value)?)))
            .collect(),
    );
    // Two builders racing at the same generation still agree on one `Arc`,
    // and a build that lost to a *newer* generation does not clobber it.
    let mut cache = FOLD_MAP.lock().expect("fold map cache lock");
    match &*cache {
        Some((built_at, _)) if *built_at >= generation => {}
        _ => *cache = Some((generation, map)),
    }
    let (_, map) = cache.as_ref().expect("just filled");
    Arc::clone(map)
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
