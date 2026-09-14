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
//!
//! The second half pins what the index means for `==` (#331): map and set
//! structural equality is *one probe per element* through that same index,
//! so it is linear and its key matching is the index's canonical notion
//! rather than the loose `==` used on values.

use std::cell::RefCell;
use std::rc::Rc;

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

fn str_value(s: &str) -> Value {
    Value::String(Rc::new(s.to_owned()))
}

/// A `Value::Map` over the given entries, in the given order.
fn map_of(pairs: &[(Value, Value)]) -> Value {
    Value::Map(Rc::new(RefCell::new(MapData::from_pairs(pairs.to_vec()))))
}

/// A `Value::Set` over the given elements, in the given order.
fn set_of(items: &[Value]) -> Value {
    Value::Set(Rc::new(RefCell::new(
        items.iter().cloned().collect::<SetData>(),
    )))
}

#[test]
fn map_equality_matches_keys_canonically_and_values_loosely() {
    // `MapLeekValue.eq` looks each left key up with `map.get(key)`, i.e. the
    // `LinkedHashMap`'s own `hashCode`/`equals`, where a `Long` key never
    // matches a `Double` one. So the keys `1` and `1.0` are different
    // entries even though `1 == 1.0` is true as a *value* comparison.
    let int_key = map_of(&[(Value::Int(1), str_value("a"))]);
    let real_key = map_of(&[(Value::Real(1.0), str_value("a"))]);
    assert!(
        !int_key.loose_eq(&real_key),
        "`[1 : 'a'] == [1.0 : 'a']` must be false: distinct canonical keys"
    );
    // The values, by contrast, do go through the loose `ai.eq`.
    assert!(
        map_of(&[(Value::Int(1), Value::Int(2))])
            .loose_eq(&map_of(&[(Value::Int(1), Value::Real(2.0))])),
        "`[1 : 2] == [1 : 2.0]` must be true: values compare loosely"
    );
    // Same for sets, whose membership test is a `LinkedHashSet.contains`.
    assert!(
        !set_of(&[Value::Int(1)]).loose_eq(&set_of(&[Value::Real(1.0)])),
        "`<1> == <1.0>` must be false: distinct canonical elements"
    );
}

#[test]
fn map_equality_ignores_insertion_order() {
    // The index is keyed by the canonical key, not by position, so two maps
    // holding the same entries agree however they were built.
    let forward = map_of(&[
        (Value::Int(1), str_value("a")),
        (Value::Int(2), str_value("b")),
        (str_value("k"), Value::Int(3)),
    ]);
    let shuffled = map_of(&[
        (str_value("k"), Value::Int(3)),
        (Value::Int(2), str_value("b")),
        (Value::Int(1), str_value("a")),
    ]);
    assert!(forward.loose_eq(&shuffled));
    assert!(shuffled.loose_eq(&forward), "equality must be symmetric");
    // One value changed is still a mismatch, whatever the order.
    let changed = map_of(&[
        (str_value("k"), Value::Int(3)),
        (Value::Int(2), str_value("B")),
        (Value::Int(1), str_value("a")),
    ]);
    assert!(!forward.loose_eq(&changed));
}

#[test]
fn set_equality_ignores_insertion_order() {
    let forward = set_of(&[Value::Int(1), str_value("b"), Value::Real(2.5)]);
    let shuffled = set_of(&[Value::Real(2.5), Value::Int(1), str_value("b")]);
    assert!(forward.loose_eq(&shuffled));
    assert!(shuffled.loose_eq(&forward), "equality must be symmetric");
    assert!(
        !forward.loose_eq(&set_of(&[Value::Int(1), str_value("b"), Value::Real(2.6)])),
        "one differing element must break it"
    );
}

#[test]
fn a_null_valued_entry_is_not_a_missing_entry() {
    // Upstream has to spell this out — `map.get(key)` returning Java `null`
    // is ambiguous, so `MapLeekValue.eq` re-checks `containsKey`. Probing the
    // index answers it directly: the key is present or it is not.
    let with_null = map_of(&[(Value::Int(1), Value::Int(1)), (Value::Int(2), Value::Null)]);
    let without = map_of(&[(Value::Int(1), Value::Int(1)), (Value::Int(3), Value::Null)]);
    assert!(
        !with_null.loose_eq(&without),
        "key 2 holding null must not match a map that has no key 2"
    );
    assert!(
        with_null.loose_eq(&map_of(&[
            (Value::Int(2), Value::Null),
            (Value::Int(1), Value::Int(1)),
        ])),
        "two maps that both hold null under key 2 are equal"
    );
    assert!(
        !with_null.loose_eq(&map_of(&[
            (Value::Int(1), Value::Int(1)),
            (Value::Int(2), Value::Int(0)),
        ])),
        "null must not compare equal to 0 as an entry value"
    );
}

#[test]
fn identical_references_short_circuit_before_the_index() {
    // The pointer-identity fast path at the top of the comparison still
    // wins, ahead of any index probe: a map is equal to itself, and to a
    // second handle on the same storage, even when it holds itself.
    let m = map_of(&[(Value::Int(1), Value::Int(1))]);
    let Value::Map(inner) = &m else {
        unreachable!()
    };
    inner
        .borrow_mut()
        .insert(str_value("self"), Value::Map(Rc::clone(inner)));
    assert!(m.loose_eq(&m));
    assert!(m.loose_eq(&Value::Map(Rc::clone(inner))));
}

#[test]
fn large_map_equality_is_one_probe_per_entry() {
    // Shape test for the nested `all`/`any` this replaced, where matching
    // each left key meant scanning the whole right collection: on two equal
    // 2000-entry maps that is ~2 million recursive comparisons, and with
    // composite keys each one also inserts into the visited set. One index
    // probe per entry replaces the scan. The assertions are on the *result*
    // — a wall clock is not a fact about this machine — but a regression to
    // the old shape shows up as this test slowing to a crawl.
    const N: i64 = 2000;
    let base: Vec<(Value, Value)> = (0..N).map(|i| (Value::Int(i), Value::Int(i * 2))).collect();
    let mut other = base.clone();
    other[0].0 = Value::Int(-1);

    let a = map_of(&base);
    assert!(
        !a.loose_eq(&map_of(&other)),
        "the first key differs, so the maps are not equal"
    );
    assert!(
        a.loose_eq(&map_of(&base)),
        "two maps built from the same entries are equal"
    );
    // The same for sets, whose old arm had the same shape.
    let items: Vec<Value> = (0..N).map(Value::Int).collect();
    let mut missing = items.clone();
    missing[0] = Value::Int(-1);
    assert!(!set_of(&items).loose_eq(&set_of(&missing)));
    assert!(set_of(&items).loose_eq(&set_of(&items)));
}
