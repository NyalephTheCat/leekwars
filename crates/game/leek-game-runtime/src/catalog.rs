//! The upstream item data, vendored verbatim and read on first use.
//!
//! [`crates/game/leek-game-runtime/data/`](../data/) holds byte-for-byte
//! copies of the reference generator's own `data/{weapons,chips,summons}.json`
//! — the same files `Generator.loadWeapons` / `loadChips` / `loadSummons`
//! read at startup. `tools/game-item-extract.sh --write` refreshes them from
//! the `official-generator` submodule and `--check` proves they still match,
//! so a generator bump is a data diff a reviewer can read rather than a
//! rewrite of generated Rust.
//!
//! The copies are vendored rather than read out of the submodule so the crate
//! still builds in a tree without it, and they are parsed here rather than
//! transcribed into `static` tables so that *every* field upstream ships
//! survives the trip: a transcriber only carries the fields it was taught,
//! and each one it was not (`passive_effects`, a summon's `zone`) went
//! missing without anybody noticing.
//!
//! Each catalog is parsed once, behind its consumer's `OnceLock`, so a fight
//! that never summons never touches `summons.json`.

use serde_json::Value;

/// `data/weapons.json` — keyed by template id, with the public item id in
/// each entry's `item` field.
pub(crate) const WEAPONS_JSON: &str = include_str!("../data/weapons.json");

/// `data/chips.json` — keyed by the public item id.
pub(crate) const CHIPS_JSON: &str = include_str!("../data/chips.json");

/// `data/summons.json` — keyed by the bulb/plant template id.
pub(crate) const SUMMONS_JSON: &str = include_str!("../data/summons.json");

/// Every entry of one catalog file, ordered by the JSON key read as an
/// integer.
///
/// Panics on malformed JSON: these files are vendored from upstream and held
/// against it by `tools/game-item-extract.sh --check`, so a parse failure is
/// a corrupt checkout rather than an input to handle.
pub(crate) fn rows(json: &str, what: &str) -> Vec<Value> {
    let parsed: Value =
        serde_json::from_str(json).unwrap_or_else(|e| panic!("{what}.json is malformed: {e}"));
    let Value::Object(map) = parsed else {
        panic!("{what}.json is not a JSON object")
    };
    let mut rows: Vec<(i64, Value)> = map
        .into_iter()
        .map(|(k, v)| {
            let key = k
                .parse()
                .unwrap_or_else(|_| panic!("{what}.json key {k:?} is not an integer"));
            (key, v)
        })
        .collect();
    rows.sort_by_key(|&(key, _)| key);
    rows.into_iter().map(|(_, v)| v).collect()
}

/// A field read as an integer, or `default` when the entry omits it.
///
/// Values arrive as JSON numbers with no integer/float distinction, so this
/// falls back to truncating a float — which is what upstream's own `getInt`
/// does with the same data.
pub(crate) fn int(entry: &Value, field: &str, default: i64) -> i64 {
    match entry.get(field) {
        None | Some(Value::Null) => default,
        // `as i64` is the `(int)` truncation upstream performs; no value in
        // this data is anywhere near the i64 boundary, so nothing saturates.
        #[allow(clippy::cast_possible_truncation)]
        Some(v) => v
            .as_i64()
            .unwrap_or_else(|| v.as_f64().unwrap_or(0.0) as i64),
    }
}

/// [`int`], narrowed to `i32` — the width the official spec types use.
pub(crate) fn int32(entry: &Value, field: &str, default: i64) -> i32 {
    i32::try_from(int(entry, field, default)).unwrap_or(0)
}

/// A field read as a float, or `0.0` when the entry omits it. The effect
/// `value1` / `value2` columns are the only fractional ones upstream ships.
pub(crate) fn num(entry: &Value, field: &str) -> f64 {
    entry.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

/// A field read as a boolean, or `default` when the entry omits it.
pub(crate) fn flag(entry: &Value, field: &str, default: bool) -> bool {
    // A boolean, or the integer one older snapshots wrote it as. Upstream
    // reads it the same way (`isBoolean() ? booleanValue() : intValue() != 0`)
    // since the 3.00 data reformat; taking only the boolean would silently
    // fall back to the default on an old entry, and a `"los": 0` weapon read
    // as `true` shoots through walls.
    entry.get(field).map_or(default, |v| {
        v.as_bool()
            .unwrap_or_else(|| v.as_i64().is_some_and(|n| n != 0))
    })
}

/// A field read as an owned string, empty when the entry omits it.
pub(crate) fn text(entry: &Value, field: &str) -> String {
    entry
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A field read as an array of integers (a chip list, a state list), empty
/// when the entry omits it.
pub(crate) fn int_list(entry: &Value, field: &str) -> Vec<i32> {
    entry
        .get(field)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_i64)
                .filter_map(|n| i32::try_from(n).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// A field read as an array of entries (`effects`, `passive_effects`), empty
/// when the entry omits it.
pub(crate) fn entries<'a>(entry: &'a Value, field: &str) -> &'a [Value] {
    entry
        .get(field)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// A summon's `characteristics.<name>` `[min, max]` pair, `(0, 0)` when the
/// entry omits it.
pub(crate) fn range(entry: &Value, stat: &str) -> (i32, i32) {
    let pair = entry
        .get("characteristics")
        .and_then(|c| c.get(stat))
        .and_then(Value::as_array);
    let at = |p: &[Value], i: usize| {
        p.get(i)
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(0)
    };
    match pair {
        Some(p) if p.len() >= 2 => (at(p, 0), at(p, 1)),
        _ => (0, 0),
    }
}
