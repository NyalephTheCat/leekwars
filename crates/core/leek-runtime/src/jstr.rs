//! Java `String` measurement, in Rust.
//!
//! Upstream LeekScript is Java, and Java strings are sequences of **UTF-16
//! code units**. Every length upstream takes — the value `length` returns,
//! the sizes it charges operations against, the positions `charAt` and
//! `codePointAt` index by — is `String.length()`, which counts those units.
//! A character outside the Basic Multilingual Plane (an emoji, say) is *two*
//! of them.
//!
//! Rust's `str` is UTF-8, and its three obvious "length" answers all
//! disagree with Java on such a string: `s.len()` counts bytes (`"😀"` → 4),
//! `s.chars().count()` counts Unicode scalar values (`"😀"` → 1), and only
//! `s.encode_utf16().count()` matches Java (`"😀"` → 2). This workspace used
//! all three in different places, so the same string measured differently
//! depending on which builtin looked at it (#268 RT-03, #336 RT-M3).
//!
//! This module is the single answer. Every function here names the Java
//! method it mirrors, and each has an ASCII fast path: for an ASCII string
//! one byte is one code unit is one character, so the byte view is already
//! the UTF-16 view and no re-encoding is needed. Non-ASCII strings pay a
//! scan, which is the price of being right.
//!
//! Indices in and out of this module are **UTF-16 positions**, never byte
//! offsets and never `char` positions.

/// Java [`String.length()`] — the number of UTF-16 code units.
///
/// `len16("abc") == 3`, `len16("héllo") == 5`, `len16("a😀b") == 4`.
///
/// [`String.length()`]: https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/lang/String.html#length()
#[inline]
pub fn len16(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

/// Java `String.charAt(int)` — the code unit at `index`, or `None` when
/// `index` is at or past [`len16`] (Java throws `StringIndexOutOfBounds`
/// there; callers decide what the language does with the miss).
///
/// On a string with an astral character this can return one half of a
/// surrogate pair, exactly as Java does — that is what makes the pair
/// addressable at all.
#[inline]
pub fn unit_at(s: &str, index: usize) -> Option<u16> {
    if s.is_ascii() {
        s.as_bytes().get(index).map(|&b| u16::from(b))
    } else {
        s.encode_utf16().nth(index)
    }
}

/// Java `String.substring(int, int)` — the code units in `[start, end)`.
///
/// `None` for the ranges Java rejects with `StringIndexOutOfBounds`:
/// `start > end`, or `end` past [`len16`].
///
/// One thing Rust cannot mirror: a range that cuts a surrogate pair in half
/// is a well-formed `String` in Java and is *not* one here, so the orphaned
/// half comes back as the replacement character `U+FFFD` rather than a lone
/// surrogate. Every other range round-trips exactly.
pub fn substring16(s: &str, start: usize, end: usize) -> Option<String> {
    if start > end {
        return None;
    }
    if s.is_ascii() {
        return s.get(start..end).map(str::to_owned);
    }
    let units: Vec<u16> = s.encode_utf16().collect();
    if end > units.len() {
        return None;
    }
    Some(String::from_utf16_lossy(&units[start..end]))
}

/// Java `String.codePointAt(int)` — the full Unicode code point that *starts*
/// at UTF-16 position `index`.
///
/// A high surrogate followed by a low surrogate is recombined into the
/// character they encode, so `code_point_at("a😀b", 1) == Some(0x1_F600)`.
/// A surrogate that is not part of a well-formed pair — including the low
/// half read on its own, `code_point_at("a😀b", 2) == Some(0xDE00)` — comes
/// back as that bare unit, again as Java does. `None` past the end.
pub fn code_point_at(s: &str, index: usize) -> Option<u32> {
    if s.is_ascii() {
        return s.as_bytes().get(index).map(|&b| u32::from(b));
    }
    let mut units = s.encode_utf16().skip(index);
    let hi = units.next()?;
    match (hi, units.next()) {
        (0xD800..=0xDBFF, Some(lo @ 0xDC00..=0xDFFF)) => {
            Some(0x1_0000 + ((u32::from(hi) - 0xD800) << 10) + (u32::from(lo) - 0xDC00))
        }
        _ => Some(u32::from(hi)),
    }
}

/// Java `String.indexOf(String, int)` — the UTF-16 position of the first
/// occurrence of `needle` at or after `from`, or `None` for Java's `-1`.
///
/// An empty needle matches at `from` (clamped to the end of the haystack),
/// which is what Java answers. A `from` past the end finds nothing.
pub fn index_of16(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let hay_len = len16(hay);
    if needle.is_empty() {
        return Some(from.min(hay_len));
    }
    if from >= hay_len {
        return None;
    }
    if hay.is_ascii() {
        // ASCII: byte offsets and UTF-16 positions are the same number.
        return hay[from..].find(needle).map(|i| i + from);
    }
    // A needle is a valid `str`, so it can never begin with a lone
    // surrogate; a match therefore always starts on a character boundary,
    // and searching bytes from the first boundary at or after `from` finds
    // exactly what Java's scan would.
    let byte_from = byte_offset_at_or_after(hay, from);
    let found = byte_from + hay[byte_from..].find(needle)?;
    Some(len16(&hay[..found]))
}

/// Byte offset of the first character whose UTF-16 position is `>= units`.
/// (`units` may land inside a surrogate pair; rounding up to the next
/// character is what a byte-wise search needs, and matches Java, since no
/// valid needle can match starting at a low surrogate.)
fn byte_offset_at_or_after(s: &str, units: usize) -> usize {
    let mut seen = 0usize;
    for (byte, c) in s.char_indices() {
        if seen >= units {
            return byte;
        }
        seen += c.len_utf16();
    }
    s.len()
}

#[cfg(test)]
mod tests {
    use super::{code_point_at, index_of16, len16, substring16, unit_at};

    /// The ASCII fast path and the general path must never disagree. Running
    /// both over the same ASCII inputs is what keeps the shortcut honest.
    #[test]
    fn the_ascii_fast_path_agrees_with_the_general_path() {
        for s in ["", "a", "abc", "hello world", "0123456789"] {
            assert_eq!(len16(s), s.encode_utf16().count(), "len16({s:?})");
            for i in 0..=s.len() + 1 {
                assert_eq!(
                    unit_at(s, i),
                    s.encode_utf16().nth(i),
                    "unit_at({s:?}, {i})"
                );
                assert_eq!(
                    code_point_at(s, i),
                    s.encode_utf16().nth(i).map(u32::from),
                    "code_point_at({s:?}, {i})",
                );
            }
        }
    }

    #[test]
    fn len16_counts_code_units_not_bytes_or_scalars() {
        assert_eq!(len16("abc"), 3);
        assert_eq!(len16("héllo"), 5, "é is 2 bytes but 1 unit");
        assert_eq!(len16("日本"), 2);
        assert_eq!(len16("a😀b"), 4, "the emoji is a surrogate pair");
        assert_eq!(len16(""), 0);
    }

    #[test]
    fn unit_at_exposes_both_halves_of_a_surrogate_pair() {
        assert_eq!(unit_at("a😀b", 0), Some(0x61));
        assert_eq!(unit_at("a😀b", 1), Some(0xD83D), "high surrogate");
        assert_eq!(unit_at("a😀b", 2), Some(0xDE00), "low surrogate");
        assert_eq!(unit_at("a😀b", 3), Some(0x62));
        assert_eq!(unit_at("a😀b", 4), None);
    }

    #[test]
    fn substring16_slices_by_code_unit_and_rejects_javas_error_ranges() {
        assert_eq!(substring16("héllo", 1, 3).as_deref(), Some("él"));
        assert_eq!(substring16("a😀b", 1, 3).as_deref(), Some("😀"));
        assert_eq!(substring16("abc", 0, 3).as_deref(), Some("abc"));
        assert_eq!(substring16("abc", 2, 2).as_deref(), Some(""));
        assert_eq!(substring16("abc", 2, 1), None, "start > end");
        assert_eq!(substring16("abc", 0, 4), None, "end past the string");
        assert_eq!(substring16("a😀b", 0, 5), None);
        // Half a pair is not representable: it renders as U+FFFD.
        assert_eq!(substring16("a😀b", 1, 2).as_deref(), Some("\u{FFFD}"));
    }

    #[test]
    fn code_point_at_recombines_a_pair_but_not_a_lone_half() {
        assert_eq!(code_point_at("a😀b", 0), Some(0x61));
        assert_eq!(code_point_at("a😀b", 1), Some(0x1_F600));
        assert_eq!(code_point_at("a😀b", 2), Some(0xDE00));
        assert_eq!(code_point_at("a😀b", 3), Some(0x62));
        assert_eq!(code_point_at("a😀b", 4), None);
        assert_eq!(code_point_at("日本", 1), Some(0x672C));
    }

    #[test]
    fn index_of16_answers_in_code_unit_positions() {
        assert_eq!(index_of16("hello", "ll", 0), Some(2));
        assert_eq!(index_of16("hello", "l", 3), Some(3));
        assert_eq!(index_of16("hello", "z", 0), None);
        // After the emoji the positions shift by two, not one.
        assert_eq!(index_of16("a😀b", "b", 0), Some(3));
        assert_eq!(index_of16("héllo là", "là", 0), Some(6));
        assert_eq!(index_of16("a😀b", "😀", 0), Some(1));
        assert_eq!(index_of16("a😀b", "a", 1), None, "searching past the match");
        // Java's empty-needle and out-of-range answers.
        assert_eq!(index_of16("abc", "", 1), Some(1));
        assert_eq!(index_of16("abc", "", 9), Some(3));
        assert_eq!(index_of16("abc", "a", 9), None);
    }
}
