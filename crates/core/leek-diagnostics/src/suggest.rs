//! "Did you mean…?" helpers.
//!
//! Producers call [`best_match`] when they have an unknown name and a
//! set of known candidates. It returns the closest match within a
//! reasonable edit-distance budget — empty string if nothing is close
//! enough, so callers can `unwrap_or_default()` and skip the
//! suggestion.

/// Levenshtein edit distance between two byte strings. Stops early
/// when distance exceeds `max`. O(a.len * b.len) worst case.
fn levenshtein(a: &str, b: &str, max: usize) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let m = a.len();
    let n = b.len();
    // Length-difference lower bound: if it already exceeds `max`,
    // skip the computation entirely.
    if m.abs_diff(n) > max {
        return max + 1;
    }
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    // Two-row DP. Indices over `b` along columns.
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=n {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(curr[j]);
        }
        if row_min > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Return the candidate closest to `needle` by edit distance, if any
/// is within an acceptable threshold (≤ ~1/3 of the longer string).
pub fn best_match<'a, I, S>(needle: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a S>,
    S: AsRef<str> + ?Sized + 'a,
{
    if needle.is_empty() {
        return None;
    }
    let budget = (needle.len().max(1) / 3).max(1);
    let mut best: Option<(usize, &'a str)> = None;
    for c in candidates {
        let cand = c.as_ref();
        if cand == needle {
            continue;
        }
        let d = levenshtein(needle, cand, budget);
        if d <= budget && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, cand));
        }
    }
    best.map(|(_, s)| s)
}

/// Convenience: return a `"did you mean `X`?"` string, or `None` if
/// no candidate is close enough.
pub fn suggest_similar<'a, I, S>(needle: &str, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = &'a S>,
    S: AsRef<str> + ?Sized + 'a,
{
    best_match(needle, candidates).map(|m| format!("did you mean `{m}`?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_typo() {
        let cands: &[&str] = &["damage", "search", "count", "size"];
        assert_eq!(best_match("damge", cands), Some("damage"));
        assert_eq!(best_match("daemag", cands), Some("damage"));
    }

    #[test]
    fn rejects_unrelated() {
        let cands: &[&str] = &["damage", "search"];
        assert_eq!(best_match("xyz", cands), None);
    }

    #[test]
    fn ignores_exact_match() {
        let cands: &[&str] = &["damage", "damge"];
        // `damge` exists; we never suggest an exact-match candidate
        // because callers only invoke this on *unknown* names.
        assert_eq!(best_match("damge", cands), Some("damage"));
    }

    #[test]
    fn formats_message() {
        let cands: &[&str] = &["count", "search"];
        assert_eq!(
            suggest_similar("cont", cands).as_deref(),
            Some("did you mean `count`?")
        );
    }
}
