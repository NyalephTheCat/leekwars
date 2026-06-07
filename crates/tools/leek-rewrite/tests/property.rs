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
}

const SOURCE: &str = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP";

/// A naive reference apply: splice non-overlapping edits (sorted by start)
/// into the source. Panics if edits overlap (caller guarantees they don't).
fn reference_apply(source: &str, mut edits: Vec<(u32, u32, String)>) -> String {
    edits.sort_by_key(|e| e.0);
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
    let len = SOURCE.len() as u32;
    let mut edits = Vec::new();
    let mut pos = 0u32;
    while pos < len {
        let gap = rng.below(4) as u32; // 0..3 untouched bytes (0 → adjacent)
        pos += gap;
        if pos >= len {
            break;
        }
        let span = rng.below(4) as u32; // 0..3 → includes zero-length inserts
        let end = (pos + span).min(len);
        let repl_len = rng.below(4);
        let repl: String = (0..repl_len).map(|_| (b'!' + (rng.below(10) as u8)) as char).collect();
        edits.push((pos, end, repl));
        pos = end.max(pos + 1); // ensure progress even for zero-length spans
    }
    edits
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
                let j = rng.below((i + 1) as u64) as usize;
                shuffled.swap(i, j);
            }
            let mut set = EditSet::new(SOURCE.len());
            for (s, e, repl) in &shuffled {
                set.push(*s, *e, repl.clone())
                    .expect("disjoint edits must never conflict");
            }
            assert_eq!(set.apply(SOURCE), expected, "push order changed the result");
        }
    }
}

#[test]
fn overlapping_edits_are_always_rejected_never_silently_wrong() {
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let len = SOURCE.len() as u32;
    for _ in 0..5000 {
        // Two random spans; push both. If they overlap (share an interior
        // byte), the second push MUST be rejected. If they only touch or are
        // disjoint, both succeed and the result matches the reference.
        let a0 = rng.below(len as u64) as u32;
        let a1 = (a0 + rng.below(5) as u32).min(len);
        let b0 = rng.below(len as u64) as u32;
        let b1 = (b0 + rng.below(5) as u32).min(len);

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
            assert!(res.is_ok(), "strictly disjoint edits must be accepted, got {res:?}");
        }
        // In every case (including ambiguous zero-length-at-boundary edits) the
        // core safety invariant holds: the push either cleanly succeeds or is a
        // clean `Overlap` error — never silent corruption — and a successful set
        // always applies to exactly the reference splice of its stored edits.
        if res.is_ok() {
            let expected = reference_apply(
                SOURCE,
                set.iter().map(|e| (e.start, e.end, e.replacement.clone())).collect(),
            );
            assert_eq!(set.apply(SOURCE), expected, "applied result diverged from reference");
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
    assert_eq!(set.apply(SOURCE), expected);
}
