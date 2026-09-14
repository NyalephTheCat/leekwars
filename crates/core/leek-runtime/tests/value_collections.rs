//! `MapData` and `SetData` carry a side index next to their authoritative
//! ordered storage — `entries` + `index` for the map, `items` + `keys` for
//! the set. Every write has to keep the two in step: `insert` appends and
//! records the slot, `remove` shifts every later slot down by one, `reindex`
//! rebuilds after a permutation. Nothing tested that (#188), and a desynced
//! index is silent: lookups return the wrong entry rather than failing.
//!
//! These drive long deterministic insert / overwrite / remove sequences and
//! check the whole invariant after every single operation, so a break is
//! reported at the op that caused it. The sequence comes from a 20-line
//! xorshift rather than a proptest dependency — it is reproducible by seed,
//! which is what matters for a regression.

use leek_runtime::{MapData, MapKey, SetData, Value};

/// xorshift64*. Deterministic, seeded, and short enough to read.
struct Rand(u64);

impl Rand {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Every `MapData` invariant, checked as a whole.
///
/// `entries` is authoritative, so: the index has exactly one slot per entry,
/// each entry's key canonicalises to a slot pointing back at it, and no two
/// entries share a canonical key.
fn check_map(m: &MapData, after: &str) {
    assert_eq!(
        m.entries.len(),
        m.index.len(),
        "after {after}: {} entries but {} index slots",
        m.entries.len(),
        m.index.len()
    );
    for (i, (k, _)) in m.entries.iter().enumerate() {
        let slot = m.index.get(&MapKey::of(k));
        assert_eq!(
            slot,
            Some(&i),
            "after {after}: entry {i} (key {k:?}) is indexed at {slot:?}"
        );
        assert!(
            m.get(k).is_some(),
            "after {after}: entry {i} (key {k:?}) is not reachable by lookup"
        );
    }
}

/// The set's mirror: one key per item, every item present, no duplicates.
fn check_set(s: &SetData, after: &str) {
    assert_eq!(
        s.items.len(),
        s.keys.len(),
        "after {after}: {} items but {} keys",
        s.items.len(),
        s.keys.len()
    );
    for v in &s.items {
        assert!(
            s.contains(v),
            "after {after}: item {v:?} is not in the key set"
        );
    }
}

/// A small key space, so the sequence hits collisions, overwrites and
/// removals of absent keys rather than only ever appending.
fn key(i: u64) -> Value {
    let n = i64::try_from(i / 3).expect("small");
    match i % 3 {
        0 => Value::Int(n),
        1 => Value::String(std::rc::Rc::new(format!("k{n}"))),
        #[expect(clippy::cast_precision_loss, reason = "n is under 30")]
        _ => Value::Real(n as f64 + 0.5),
    }
}

#[test]
fn map_index_tracks_entries_through_inserts_and_removals() {
    let mut r = Rand(0x1234_5678_9ABC_DEF0);
    let mut m = MapData::new();
    let mut mirror: Vec<Value> = Vec::new();
    for step in 0..600 {
        let k = key(r.below(30));
        if r.below(3) == 0 && !mirror.is_empty() {
            // Remove an existing key about a third of the time, and
            // occasionally one that was never there.
            let victim = if r.below(4) == 0 {
                key(r.below(30))
            } else {
                let pick = r.below(u64::try_from(mirror.len()).expect("small"));
                mirror[usize::try_from(pick).expect("small")].clone()
            };
            let had = m.remove(&victim).is_some();
            if had {
                let c = MapKey::of(&victim);
                mirror.retain(|x| MapKey::of(x) != c);
            }
            check_map(&m, &format!("step {step}: remove({victim:?}) -> {had}"));
        } else {
            let fresh = !m.contains_key(&k);
            m.insert(k.clone(), Value::Int(step));
            if fresh {
                mirror.push(k.clone());
            }
            check_map(&m, &format!("step {step}: insert({k:?})"));
        }
        assert_eq!(m.len(), mirror.len(), "step {step}: length drifted");
    }
    assert!(!m.is_empty(), "the sequence emptied the map — weaken it");
}

#[test]
fn map_insert_overwrites_in_place_and_keeps_order() {
    let mut m = MapData::new();
    for i in 0..10 {
        m.insert(Value::Int(i), Value::Int(i * 10));
    }
    m.insert(Value::Int(4), Value::Int(999));
    check_map(&m, "overwrite");
    assert_eq!(m.len(), 10, "an overwrite must not append");
    assert!(
        matches!(m.entries[4], (Value::Int(4), Value::Int(999))),
        "the overwritten value must stay in its original slot"
    );
}

#[test]
fn map_removal_shifts_only_the_later_slots() {
    let mut m = MapData::new();
    for i in 0..10 {
        m.insert(Value::Int(i), Value::Int(i));
    }
    assert!(m.remove(&Value::Int(3)).is_some());
    check_map(&m, "remove(3)");
    assert!(
        matches!(m.entries[3], (Value::Int(4), _)),
        "later entries must close the gap, keeping their order"
    );
    assert!(!m.contains_key(&Value::Int(3)));
}

#[test]
fn map_reindex_is_idempotent_and_repairs_a_permutation() {
    let mut m = MapData::new();
    for i in 0..20 {
        m.insert(key(i), Value::Int(i64::try_from(i).expect("small")));
    }
    // A sort is exactly the case `reindex` exists for: `entries` is permuted
    // behind the index's back.
    m.entries.reverse();
    m.reindex();
    check_map(&m, "reverse + reindex");
    let before = m.index.clone();
    m.reindex();
    assert_eq!(before, m.index, "reindex must be idempotent");
    check_map(&m, "reindex twice");
}

#[test]
fn set_keys_track_items_through_inserts_and_removals() {
    let mut r = Rand(0x0FED_CBA9_8765_4321);
    let mut s = SetData::new();
    for step in 0..600u64 {
        let v = key(r.below(30));
        let present = s.contains(&v);
        if r.below(3) == 0 {
            let removed = s.remove(&v);
            assert_eq!(
                removed,
                present,
                "step {step}: remove({v:?}) reported {removed} for a {} element",
                if present { "present" } else { "missing" }
            );
            check_set(&s, &format!("step {step}: remove({v:?})"));
        } else {
            let added = s.insert(v.clone());
            assert_eq!(
                added,
                !present,
                "step {step}: insert({v:?}) reported {added} for a {} element",
                if present { "present" } else { "missing" }
            );
            check_set(&s, &format!("step {step}: insert({v:?})"));
        }
    }
}

#[test]
fn set_insert_reports_novelty_and_keeps_first_occurrence_order() {
    let mut s = SetData::new();
    assert!(s.insert(Value::Int(1)));
    assert!(s.insert(Value::Int(2)));
    assert!(
        !s.insert(Value::Int(1)),
        "a repeat must report no insertion"
    );
    check_set(&s, "repeat insert");
    assert_eq!(s.len(), 2);
    assert!(
        matches!(s.items[0], Value::Int(1)),
        "first occurrence wins, like upstream's LinkedHashSet"
    );
    assert!(s.remove(&Value::Int(1)));
    assert!(
        !s.remove(&Value::Int(1)),
        "removing twice must report false"
    );
    check_set(&s, "double remove");
}
