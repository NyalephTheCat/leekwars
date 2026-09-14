//! Proof that map/set key handling no longer allocates (RT-02, #239).
//!
//! The issue is a performance one, so asserting "it's faster now"
//! would be worthless. Instead this test installs a counting global
//! allocator and measures the allocations a workload performs.
//! Lookups, membership tests, overwriting inserts and removals on
//! primitive-keyed collections must come out at *exactly zero*.
//!
//! Every test also carries its own control that renders each key to a
//! `String` over the same workload — what the old string-keyed path did
//! per probe — so a broken harness (one that silently counts nothing)
//! fails loudly instead of passing vacuously.

// The counting allocator has to implement `GlobalAlloc`, which is an
// unsafe trait. Same carve-out as `src/builtin.rs`'s `no_mangle` shims.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::rc::Rc;

use leek_runtime::{MapData, MapKey, SetData, Value};

thread_local! {
    /// Allocations on *this* thread. Per-thread (rather than one
    /// global atomic) so the libtest harness and any sibling test
    /// thread can't perturb a measurement. `Cell<u64>` is `Copy` and
    /// has no destructor, so the const-initialised TLS slot never
    /// itself allocates or registers a teardown hook — safe to touch
    /// from inside the allocator.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every method forwards to `System` with the caller's layout
// unchanged; the counter is the only added work and it cannot
// re-enter the allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn bump() {
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f` and report how many allocations it made.
fn allocations(f: impl FnOnce()) -> u64 {
    let before = ALLOCS.with(Cell::get);
    f();
    ALLOCS.with(Cell::get) - before
}

const N: i64 = 10_000;
const PROBES: i64 = 100_000;

fn int_map() -> MapData {
    let mut m = MapData::new();
    for i in 0..N {
        m.insert(Value::Int(i), Value::Int(i * 2));
    }
    m
}

#[test]
fn int_key_lookup_allocates_nothing() {
    let m = int_map();
    let mut hits = 0u64;
    let n = allocations(|| {
        for i in 0..PROBES {
            if m.get(&Value::Int(i % N)).is_some() {
                hits += 1;
            }
        }
    });
    assert_eq!(hits, u64::try_from(PROBES).unwrap());
    assert_eq!(n, 0, "{PROBES} int-keyed lookups made {n} allocations");

    // Control: the pre-fix path built a `String` per lookup. If this
    // comes out at zero the harness isn't measuring anything and the
    // assertion above proves nothing.
    let control = allocations(|| {
        for i in 0..PROBES {
            std::hint::black_box(Value::Int(i % N).to_string());
        }
    });
    assert!(
        control >= u64::try_from(PROBES).unwrap(),
        "counting allocator is not working: {PROBES} key renderings counted {control} allocations"
    );
}

#[test]
fn string_key_lookup_allocates_nothing() {
    // A string key shares the `Value`'s `Rc`, so canonicalising it is
    // a refcount bump — the string body is never copied.
    let keys: Vec<Value> = (0..N)
        .map(|i| Value::String(Rc::new(format!("key-{i}-with-a-long-enough-body"))))
        .collect();
    let mut m = MapData::new();
    for (i, k) in keys.iter().enumerate() {
        m.insert(k.clone(), Value::Int(i64::try_from(i).unwrap()));
    }

    let mut hits = 0u64;
    let n = allocations(|| {
        for i in 0..PROBES {
            if m.get(&keys[usize::try_from(i % N).unwrap()]).is_some() {
                hits += 1;
            }
        }
    });
    assert_eq!(hits, u64::try_from(PROBES).unwrap());
    assert_eq!(n, 0, "{PROBES} string-keyed lookups made {n} allocations");

    let control = allocations(|| {
        for i in 0..PROBES {
            std::hint::black_box(keys[usize::try_from(i % N).unwrap()].to_string());
        }
    });
    assert!(
        control >= u64::try_from(PROBES).unwrap(),
        "control counted {control}"
    );
}

#[test]
fn overwriting_insert_allocates_nothing() {
    let mut m = int_map();
    let n = allocations(|| {
        for i in 0..PROBES {
            m.insert(Value::Int(i % N), Value::Int(i));
        }
    });
    assert_eq!(m.len(), usize::try_from(N).unwrap());
    assert_eq!(n, 0, "{PROBES} overwriting inserts made {n} allocations");
}

#[test]
fn set_contains_and_remove_allocate_nothing() {
    let mut set = SetData::new();
    for i in 0..N {
        set.insert(Value::Int(i));
    }
    let n = allocations(|| {
        for i in 0..PROBES {
            assert!(set.contains(&Value::Int(i % N)));
        }
    });
    assert_eq!(n, 0, "{PROBES} set membership tests made {n} allocations");

    // `SetData::remove` still walks `items` to find the element (the
    // set is order-preserving), but it used to build a `String` for
    // every element it walked past. Canonicalising is now free.
    let n = allocations(|| {
        for i in 0..N {
            assert!(set.remove(&Value::Int(i)));
        }
    });
    assert!(set.is_empty());
    assert_eq!(n, 0, "{N} set removals made {n} allocations");

    // The worst case for that scan: removing from the *back*, so the
    // walk covers the whole set each time. Still zero.
    let mut set = SetData::new();
    for i in 0..1_000 {
        set.insert(Value::Int(i));
    }
    let n = allocations(|| {
        for i in (0..1_000).rev() {
            assert!(set.remove(&Value::Int(i)));
        }
    });
    assert!(set.is_empty());
    assert_eq!(n, 0, "back-to-front set removals made {n} allocations");
}

#[test]
fn map_remove_allocates_nothing() {
    let mut m = int_map();
    let n = allocations(|| {
        for i in 0..N {
            assert!(m.remove(&Value::Int(i)).is_some());
        }
    });
    assert!(m.is_empty());
    assert_eq!(n, 0, "{N} map removals made {n} allocations");
}

#[test]
fn fresh_inserts_allocate_only_for_growth() {
    // Growing the map still allocates — the `Vec` and the `HashMap`
    // both reallocate as they double — but that is O(log n) events,
    // not one per insert. Pre-fix this workload also paid one
    // `String` per insert, i.e. at least `N` extra allocations.
    let mut m = MapData::new();
    let n = allocations(|| {
        for i in 0..N {
            m.insert(Value::Int(i), Value::Int(i));
        }
    });
    assert_eq!(m.len(), usize::try_from(N).unwrap());
    assert!(
        n < 100,
        "{N} fresh inserts made {n} allocations; expected only container growth"
    );

    // Same workload rendering a key per insert, the old way, for the
    // size of the win.
    let control = allocations(|| {
        for i in 0..N {
            std::hint::black_box(Value::Int(i).to_string());
        }
    });
    assert!(
        control > n * 10,
        "expected the string-keyed control ({control}) to dwarf the typed path ({n})"
    );
}

#[test]
fn building_a_key_allocates_only_for_composites() {
    let string = Value::String(Rc::new("hello world, a body worth copying".to_string()));
    let big = Value::BigInt(Rc::new(leek_runtime::big_from_decimal(
        "12345678901234567890",
    )));
    let n = allocations(|| {
        std::hint::black_box(MapKey::of(&Value::Null));
        std::hint::black_box(MapKey::of(&Value::Bool(true)));
        std::hint::black_box(MapKey::of(&Value::Int(-42)));
        std::hint::black_box(MapKey::of(&Value::Real(1.5)));
        std::hint::black_box(MapKey::of(&string));
        std::hint::black_box(MapKey::of(&big));
    });
    assert_eq!(n, 0, "primitive key canonicalisation made {n} allocations");
}
