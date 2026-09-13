//! Property tests for `EditSet` (P2 #26): randomized overlap / ordering /
//! boundary behavior beyond the curated unit tests. Deterministic (fixed-seed
//! xorshift) so any failure reproduces.

use leek_rewrite::{EditError, EditSet};

/// Deterministic xorshift64 PRNG — no external `rand` dependency, and no
/// `Math.random`-style nondeterminism (failures reproduce verbatim).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    /// [`Self::below`] as `u32` (every bound in these tests is tiny).
    fn below32(&mut self, n: u64) -> u32 {
        u32::try_from(self.below(n)).unwrap()
    }
}

/// `SOURCE.len()` as `u32` (the source is a short fixed string).
fn source_len() -> u32 {
    u32::try_from(SOURCE.len()).unwrap()
}

const SOURCE: &str = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP";

/// A naive reference apply: splice non-overlapping edits into the source in
/// `EditSet`'s documented order — by start offset, insertions before the
/// replacement that starts at the same offset, push order among the rest
/// (the sort is stable). Panics if edits overlap (caller guarantees they
/// don't).
fn reference_apply(source: &str, mut edits: Vec<(u32, u32, String)>) -> String {
    edits.sort_by_key(|e| (e.0, e.0 != e.1));
    let mut out = String::new();
    let mut cursor = 0usize;
    for (s, e, repl) in edits {
        let (s, e) = (s as usize, e as usize);
        assert!(s >= cursor, "reference given overlapping edits");
        out.push_str(&source[cursor..s]);
        out.push_str(&repl);
        cursor = e;
    }
    out.push_str(&source[cursor..]);
    out
}

/// Generate a random set of *non-overlapping* edits over SOURCE by walking
/// left-to-right and leaving gaps, so they're valid by construction.
fn gen_disjoint(rng: &mut Rng) -> Vec<(u32, u32, String)> {
    let len = source_len();
    let mut edits = Vec::new();
    let mut pos = 0u32;
    while pos < len {
        let gap = rng.below32(4); // 0..3 untouched bytes (0 → adjacent)
        pos += gap;
        if pos >= len {
            break;
        }
        let span = rng.below32(4); // 0..3 → includes zero-length inserts
        let end = (pos + span).min(len);
        let repl_len = rng.below(4);
        let repl: String = (0..repl_len)
            .map(|_| (b'!' + u8::try_from(rng.below(10)).unwrap()) as char)
            .collect();
        edits.push((pos, end, repl));
        pos = end.max(pos + 1); // ensure progress even for zero-length spans
    }
    edits
}

/// Like [`gen_disjoint`], but sprinkles zero-length insertions at the
/// boundaries of the generated edits — the tie cases `gen_disjoint`
/// steps over. Insertions own no bytes, so the set stays conflict-free
/// whatever order it is pushed in.
fn gen_with_boundary_inserts(rng: &mut Rng) -> Vec<(u32, u32, String)> {
    let mut edits = gen_disjoint(rng);
    // Offsets that already hold an insertion. Two insertions at one offset
    // apply in *push* order by design, so a shuffled set would legitimately
    // differ; keep at most one per offset here and let
    // `push_order_is_fifo_for_inserts_at_one_offset` cover that case.
    let mut taken: Vec<u32> = edits.iter().filter(|e| e.0 == e.1).map(|e| e.0).collect();
    let mut extra = Vec::new();
    for (i, (s, e, _)) in edits.iter().enumerate() {
        // Deterministic tag so a misordered insertion shows up as a diff,
        // not just a length mismatch.
        for (off, text) in [(*s, format!("<{i}")), (*e, format!("{i}>"))] {
            if rng.below(2) == 0 && !taken.contains(&off) {
                taken.push(off);
                extra.push((off, off, text));
            }
        }
    }
    edits.extend(extra);
    edits
}

/// Push two edits into a fresh set in the given order, then apply.
fn push_two(first: (u32, u32, &str), second: (u32, u32, &str)) -> Result<String, EditError> {
    let mut set = EditSet::new(SOURCE.len());
    set.push(first.0, first.1, first.2.to_string())?;
    set.push(second.0, second.1, second.2.to_string())?;
    set.apply(SOURCE)
}

#[test]
fn disjoint_edits_are_order_independent_and_match_reference() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..2000 {
        let edits = gen_disjoint(&mut rng);
        let expected = reference_apply(SOURCE, edits.clone());

        // Push in several shuffled orders; every order must accept all edits
        // (they're disjoint) and produce the identical, reference-correct text.
        for _ in 0..3 {
            let mut shuffled = edits.clone();
            // Fisher–Yates with the same PRNG.
            for i in (1..shuffled.len()).rev() {
                let j = usize::try_from(rng.below((i + 1) as u64)).unwrap();
                shuffled.swap(i, j);
            }
            let mut set = EditSet::new(SOURCE.len());
            for (s, e, repl) in &shuffled {
                set.push(*s, *e, repl.clone())
                    .expect("disjoint edits must never conflict");
            }
            assert_eq!(
                set.apply(SOURCE).unwrap(),
                expected,
                "push order changed the result"
            );
        }
    }
}

#[test]
fn overlapping_edits_are_always_rejected_never_silently_wrong() {
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let len = source_len();
    for _ in 0..5000 {
        // Two random spans; push both. If they overlap (share an interior
        // byte), the second push MUST be rejected. If they only touch or are
        // disjoint, both succeed and the result matches the reference.
        let a0 = rng.below32(u64::from(len));
        let a1 = (a0 + rng.below32(5)).min(len);
        let b0 = rng.below32(u64::from(len));
        let b1 = (b0 + rng.below32(5)).min(len);

        let mut set = EditSet::new(SOURCE.len());
        set.push(a0, a1, "X".into()).unwrap();
        let res = set.push(b0, b1, "Y".into());

        // Two spans that share an *interior* byte definitely overlap and must
        // be rejected (`max(start) < min(end)` over half-open intervals).
        let shares_interior = a0.max(b0) < a1.min(b1);
        // Two non-empty spans separated by a gap definitely don't conflict.
        let strictly_disjoint = a1 < b0 || b1 < a0;

        if shares_interior {
            assert!(
                matches!(res, Err(EditError::Overlap { .. })),
                "overlapping edits ({a0}..{a1}) & ({b0}..{b1}) must be rejected, got {res:?}",
            );
        } else if strictly_disjoint {
            assert!(
                res.is_ok(),
                "strictly disjoint edits must be accepted, got {res:?}"
            );
        }
        // In every case (including ambiguous zero-length-at-boundary edits) the
        // core safety invariant holds: the push either cleanly succeeds or is a
        // clean `Overlap` error — never silent corruption — and a successful set
        // always applies to exactly the reference splice of its stored edits.
        if res.is_ok() {
            let expected = reference_apply(
                SOURCE,
                set.iter()
                    .map(|e| (e.start, e.end, e.replacement.clone()))
                    .collect(),
            );
            assert_eq!(
                set.apply(SOURCE).unwrap(),
                expected,
                "applied result diverged from reference"
            );
        } else {
            assert!(
                matches!(res, Err(EditError::Overlap { .. })),
                "a rejected edit must be a clean Overlap error, got {res:?}",
            );
        }
    }
}

#[test]
fn touching_edits_at_a_boundary_are_accepted() {
    // Two edits that share only an endpoint (`a.end == b.start`) do NOT
    // overlap and must both apply. Also covers a zero-length insertion sitting
    // exactly at another edit's boundary.
    let mut set = EditSet::new(SOURCE.len());
    set.push(2, 5, "X".into()).unwrap();
    set.push(5, 8, "Y".into()).unwrap();
    // Zero-length insert exactly at the shared boundary (5).
    set.push(5, 5, "|".into()).unwrap();
    let expected = reference_apply(
        SOURCE,
        vec![(2, 5, "X".into()), (5, 5, "|".into()), (5, 8, "Y".into())],
    );
    assert_eq!(set.apply(SOURCE).unwrap(), expected);
}

#[test]
fn boundary_inserts_are_order_independent_and_match_reference() {
    let mut rng = Rng(0x5EED_0F1E_2D3C_4B5A);
    for _ in 0..2000 {
        let edits = gen_with_boundary_inserts(&mut rng);
        let expected = reference_apply(SOURCE, edits.clone());

        for _ in 0..3 {
            let mut shuffled = edits.clone();
            for i in (1..shuffled.len()).rev() {
                let j = usize::try_from(rng.below((i + 1) as u64)).unwrap();
                shuffled.swap(i, j);
            }
            let mut set = EditSet::new(SOURCE.len());
            for (s, e, repl) in &shuffled {
                set.push(*s, *e, repl.clone())
                    .expect("insertions at an edit's boundary must never conflict");
            }
            assert_eq!(
                set.apply(SOURCE).unwrap(),
                expected,
                "push order changed the result for {shuffled:?}"
            );
        }
    }
}

/// Exhaustive over a small grid: for *every* pair of spans — replacement
/// vs replacement, replacement vs zero-length insertion, insertion vs
/// insertion — the two push orders must agree on whether the pair is legal
/// and, when it is, on the text it produces.
#[test]
fn push_order_changes_neither_the_verdict_nor_the_text() {
    const GRID: u32 = 8;
    for a0 in 0..GRID {
        for a1 in a0..=GRID {
            for b0 in 0..GRID {
                for b1 in b0..=GRID {
                    let a = (a0, a1, "A");
                    let b = (b0, b1, "B");
                    let fwd = push_two(a, b);
                    let rev = push_two(b, a);
                    assert_eq!(
                        fwd.is_ok(),
                        rev.is_ok(),
                        "({a0}..{a1}) & ({b0}..{b1}) accepted in one order only: \
                         {fwd:?} vs {rev:?}",
                    );
                    let (Ok(fwd), Ok(rev)) = (fwd, rev) else {
                        continue;
                    };
                    if a0 == a1 && b0 == b1 && a0 == b0 {
                        // Two insertions at one offset: FIFO by design, so the
                        // orders differ — and each must be exactly its push order.
                        assert!(fwd.contains("AB"), "insert FIFO broken at {a0}: {fwd}");
                        assert!(rev.contains("BA"), "insert FIFO broken at {a0}: {rev}");
                    } else {
                        assert_eq!(
                            fwd, rev,
                            "({a0}..{a1}) & ({b0}..{b1}) applied differently per push order",
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn push_order_is_fifo_for_inserts_at_one_offset() {
    // Insertions at a single offset keep the order they were pushed in,
    // however many there are and whatever else shares the offset.
    for &(start, end) in &[(5u32, 5u32), (5, 9), (2, 5)] {
        for replace_first in [true, false] {
            let mut set = EditSet::new(SOURCE.len());
            let push_replacement = |set: &mut EditSet| {
                if (start, end) != (5, 5) {
                    set.push(start, end, "R".into()).unwrap();
                }
            };
            if replace_first {
                push_replacement(&mut set);
            }
            for tag in ["a", "b", "c", "d"] {
                set.push(5, 5, tag.into()).unwrap();
            }
            if !replace_first {
                push_replacement(&mut set);
            }
            let out = set.apply(SOURCE).unwrap();
            assert!(
                out.contains("abcd"),
                "inserts at 5 lost push order next to {start}..{end} \
                 (replace_first = {replace_first}): {out}",
            );
        }
    }
}
