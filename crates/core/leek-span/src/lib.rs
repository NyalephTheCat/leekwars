//! Source positions and spans.
//!
//! [`Span`] is a half-open byte range `[start, end)` inside a [`SourceId`].
//! Spans are cheap to copy (12 bytes). A [`LineTable`] lazily computed from
//! the source text converts byte offsets to (line, column) on demand.

use std::borrow::Cow;
use std::num::NonZeroU32;
use std::ops::Range;

pub mod paths;
pub mod pragma;

/// Define a `u32`-backed index newtype with the workspace's standard
/// derives (`Copy`/`Eq`/`Hash`/`Ord`, plus salsa's `Update` when the
/// invoking crate's `salsa` feature is on) and, optionally, a `Display`
/// format. Centralizes the boilerplate the HIR/MIR/resolver index types
/// (`DefId`, `LocalId`, `BlockId`, `SymbolId`) all repeated.
///
/// ```ignore
/// newtype_index! {
///     /// Index into `MirFunction::blocks`.
///     pub struct BlockId;
///     display = "bb{}";
/// }
/// ```
///
/// The salsa `derive` is emitted as `#[cfg_attr(feature = "salsa", …)]`,
/// evaluated against the crate that *invokes* the macro — so each such
/// crate must keep its optional `salsa` dependency + feature (they all
/// already do).
#[macro_export]
macro_rules! newtype_index {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident;
        $(display = $fmt:literal;)?
    ) => {
        $(#[$meta])*
        #[cfg_attr(feature = "salsa", derive(salsa::Update))]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis struct $name(pub u32);

        $(
            impl ::core::fmt::Display for $name {
                fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    write!(f, $fmt, self.0)
                }
            }
        )?
    };
}

/// Stable identifier for a source file, opaque to consumers.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(NonZeroU32);

impl SourceId {
    pub const fn new(raw: u32) -> Option<Self> {
        match NonZeroU32::new(raw) {
            Some(n) => Some(Self(n)),
            None => None,
        }
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// Half-open byte range inside a source file.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub source: SourceId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(source: SourceId, start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "span start > end");
        Self { source, start, end }
    }

    /// A zero-width placeholder span for compiler-synthesized nodes that carry
    /// no real source location (e.g. `Stmt::Charge`, inserted defaults). Uses a
    /// reserved sentinel `SourceId` (`u32::MAX`) that real files never get
    /// (`ProjectIndex` hands out ids from 1 upward), so a synthetic span is
    /// distinguishable from — and can't be mistaken for a real location in —
    /// the first loaded file. Only meaningful as "no real span".
    #[must_use]
    pub fn synthetic() -> Self {
        Span::new(Self::SYNTHETIC_SOURCE, 0, 0)
    }

    /// The reserved `SourceId` used by [`Span::synthetic`].
    pub const SYNTHETIC_SOURCE: SourceId = match SourceId::new(u32::MAX) {
        Some(id) => id,
        None => unreachable!(),
    };

    /// The reserved `SourceId` for spans inside the project manifest
    /// (`Miku.toml`). The manifest is not a `.leek` file and never enters the
    /// `ProjectIndex`, but its diagnostics still need a source to point at, so
    /// they get their own sentinel rather than borrowing the entry file's id —
    /// which is what made a manifest error render a caret at byte 0 of
    /// `main.leek`. Sits next to [`SYNTHETIC_SOURCE`](Self::SYNTHETIC_SOURCE)
    /// at the top of the range, out of reach of the ids `ProjectIndex` hands
    /// out from 1 upward.
    pub const MANIFEST_SOURCE: SourceId = match SourceId::new(u32::MAX - 1) {
        Some(id) => id,
        None => unreachable!(),
    };

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Combine two spans into the smallest span covering both.
    /// Panics if they belong to different sources.
    pub fn union(self, other: Span) -> Span {
        assert_eq!(
            self.source, other.source,
            "cannot union spans from different sources"
        );
        Span {
            source: self.source,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// Narrow a byte offset or length to the `u32` that [`Span`] stores.
///
/// `Span` uses `u32` offsets throughout, so sources larger than 4 GiB are
/// unsupported by design; this is the single place that invariant is enforced.
#[must_use]
#[inline]
pub fn offset(n: usize) -> u32 {
    u32::try_from(n).expect("source larger than 4 GiB")
}

/// One-based line/column, where `col` is a **byte** column: the number of
/// UTF-8 bytes from the start of the line, plus one.
///
/// A byte column is the right unit for slicing source text, and the wrong one
/// for showing a human or an editor. Two conversions live on [`LineTable`] and
/// are the only supported ways to leave byte space:
///
/// - [`LineTable::display_col`] — the column a terminal draws at, counting
///   characters and expanding tabs. The diagnostic renderer pads carets with
///   it, and prints it in the `--> file:line:col` header.
/// - [`LineTable::utf16_col`] — UTF-16 code units, the unit LSP `Position`
///   speaks.
///
/// The three agree on pure-ASCII, tab-free lines and diverge everywhere else,
/// so a value's unit must travel with it rather than be assumed.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

/// Maps byte offsets in a source string to (line, column) positions.
///
/// Stores the byte offset of each line start. Construction is O(n);
/// lookup is O(log lines) via binary search.
#[derive(Debug, Clone)]
pub struct LineTable {
    line_starts: Vec<u32>,
}

impl LineTable {
    // Byte offsets and line indices fit in `u32`: `Span` uses `u32` offsets
    // throughout, so sources larger than 4 GiB are unsupported by design.
    pub fn new(text: &str) -> Self {
        let mut line_starts = Vec::with_capacity(text.len() / 32 + 1);
        line_starts.push(0);
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(offset(i + 1));
            }
        }
        Self { line_starts }
    }

    pub fn line_col(&self, offset: u32) -> LineCol {
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        LineCol {
            line: crate::offset(line_idx + 1),
            col: offset - self.line_starts[line_idx] + 1,
        }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Byte offset of the start of `line_idx` (0-based). `None` if
    /// the index is out of range.
    pub fn line_start(&self, line_idx: usize) -> Option<u32> {
        self.line_starts.get(line_idx).copied()
    }

    /// One-based **display** column of `offset`: the column a terminal draws
    /// that character at, counting characters (not bytes) and expanding a tab
    /// to the next multiple of `tab_width`.
    ///
    /// This is the column the diagnostic renderer pads carets to. An `offset`
    /// past the end of its line clamps to just after the line's last
    /// character; a `tab_width` of 0 is treated as 1.
    pub fn display_col(&self, source: &str, offset: u32, tab_width: u32) -> u32 {
        let tab_width = tab_width.max(1);
        let mut col = 0u32;
        self.walk_line_to(source, offset, |ch| {
            col = if ch == '\t' {
                // Advance to the next tab stop, never by less than one cell.
                (col / tab_width + 1) * tab_width
            } else {
                col + 1
            };
        });
        col + 1
    }

    /// One-based **UTF-16** column of `offset` — the unit an LSP `Position`
    /// counts in. An `offset` past the end of its line clamps to the line end.
    ///
    /// This differs from [`display_col`](Self::display_col) for any character
    /// outside the BMP (`𝄞` is two UTF-16 units but one display cell) and for
    /// tabs (one UTF-16 unit, `tab_width` cells).
    pub fn utf16_col(&self, source: &str, offset: u32) -> u32 {
        let mut col = 0u32;
        self.walk_line_to(source, offset, |ch| col += crate::offset(ch.len_utf16()));
        col + 1
    }

    /// Call `step` once per character of `offset`'s line that lies strictly
    /// before `offset` — the shared walk behind the column conversions.
    fn walk_line_to(&self, source: &str, offset: u32, mut step: impl FnMut(char)) {
        let lc = self.line_col(offset);
        let byte_col = (lc.col - 1) as usize;
        let line = self.line_text(source, (lc.line - 1) as usize).unwrap_or("");
        let mut byte = 0usize;
        for ch in line.chars() {
            if byte >= byte_col {
                break;
            }
            byte += ch.len_utf8();
            step(ch);
        }
    }

    /// [`line_text`](Self::line_text) with tabs expanded to spaces at
    /// multiples of `tab_width`, so a rendered source line lines up with a
    /// caret padded by [`display_col`](Self::display_col). `None` for an
    /// out-of-range index; borrows the original when the line holds no tab.
    pub fn expand_tabs<'a>(
        &self,
        source: &'a str,
        line_idx: usize,
        tab_width: u32,
    ) -> Option<Cow<'a, str>> {
        Some(expand_tabs_in(self.line_text(source, line_idx)?, tab_width))
    }

    /// Slice of `source` covering line `line_idx` (0-based), without
    /// the trailing `\n`. `None` if the index is out of range.
    pub fn line_text<'a>(&self, source: &'a str, line_idx: usize) -> Option<&'a str> {
        let start = *self.line_starts.get(line_idx)? as usize;
        let end = self
            .line_starts
            .get(line_idx + 1)
            .map_or(source.len(), |&e| e as usize);
        // Drop the trailing newline if present.
        let end = if end > start && source.as_bytes()[end - 1] == b'\n' {
            // Also drop a preceding CR for CRLF endings.
            if end > start + 1 && source.as_bytes()[end - 2] == b'\r' {
                end - 2
            } else {
                end - 1
            }
        } else {
            end
        };
        source.get(start..end)
    }
}

/// Expand every tab in `line` to spaces, each advancing to the next multiple
/// of `tab_width`. Borrows when there is nothing to expand.
#[must_use]
pub fn expand_tabs_in(line: &str, tab_width: u32) -> Cow<'_, str> {
    if !line.contains('\t') {
        return Cow::Borrowed(line);
    }
    let tab_width = (tab_width.max(1)) as usize;
    let mut out = String::with_capacity(line.len() + tab_width);
    let mut col = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let next = (col / tab_width + 1) * tab_width;
            out.extend(std::iter::repeat_n(' ', next - col));
            col = next;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    Cow::Owned(out)
}

/// Opt-in experimental language features, threaded explicitly through the
/// pipeline instead of read from process-global environment variables. Lives in
/// `leek-span` so every pass (lexer→types) and the salsa `Input` can carry it
/// without a dependency cycle.
///
/// Historically each pass read its own `LEEK_EXPERIMENTAL_*` env var — global,
/// untestable, and (inside salsa-tracked queries) impure: salsa wouldn't re-run
/// when a flag changed. Carrying the flags as data fixes all three; the env
/// vars are read exactly once, at a boundary, via [`FeatureFlags::from_env`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FeatureFlags {
    /// `LEEK_EXPERIMENTAL_FN_SIGNATURES`: bodiless `function f() -> T;`
    /// signatures and `@backend:` directives.
    pub function_signatures: bool,
    /// `LEEK_EXPERIMENTAL_GENERIC_SYNTAX`: parse generic type syntax.
    pub generic_syntax: bool,
    /// `LEEK_EXPERIMENTAL_GENERICS`: generic builtin type inference.
    pub generics: bool,
    /// `LEEK_EXPERIMENTAL_FN_OVERLOADS`: function overloading.
    pub overloads: bool,
    /// `LEEK_EXPERIMENTAL_PRELUDE`: implicit standard-library prelude.
    pub prelude: bool,
    /// `LEEK_EXPERIMENTAL_TYPES`: `type Name = T` alias declarations
    /// and tuple-shaped array types (`Array[integer, boolean]`).
    pub types: bool,
    /// `LEEK_EXPERIMENTAL_INTERFACES`: `interface Name { … }`
    /// declarations and the `implements` clause on classes.
    pub interfaces: bool,
    /// `LEEK_EXPERIMENTAL_ENUMS`: `enum Name { A, B = 10 }`
    /// declarations — integer-backed variants lowered to a class with
    /// static final integer fields.
    pub enums: bool,
}

impl FeatureFlags {
    /// All features off — the default, non-experimental behavior.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Read the flags from the `LEEK_EXPERIMENTAL_*` environment variables. The
    /// single sanctioned place env is consulted; call it once at an entry
    /// boundary and thread the result, rather than reading env deep in a pass.
    #[must_use]
    pub fn from_env() -> Self {
        let on = |k: &str| std::env::var_os(k).is_some();
        Self {
            function_signatures: on("LEEK_EXPERIMENTAL_FN_SIGNATURES"),
            generic_syntax: on("LEEK_EXPERIMENTAL_GENERIC_SYNTAX"),
            generics: on("LEEK_EXPERIMENTAL_GENERICS"),
            overloads: on("LEEK_EXPERIMENTAL_FN_OVERLOADS"),
            prelude: on("LEEK_EXPERIMENTAL_PRELUDE"),
            types: on("LEEK_EXPERIMENTAL_TYPES"),
            interfaces: on("LEEK_EXPERIMENTAL_INTERFACES"),
            enums: on("LEEK_EXPERIMENTAL_ENUMS"),
        }
    }

    /// Pack into a `u8` bitmask. Lets the salsa pipeline inputs carry the flags
    /// as a primitive (avoiding a `salsa::Update` dependency on this type, the
    /// same reason `version_byte` is stored as a `u8`).
    #[must_use]
    pub fn to_bits(self) -> u8 {
        u8::from(self.function_signatures)
            | (u8::from(self.generic_syntax) << 1)
            | (u8::from(self.generics) << 2)
            | (u8::from(self.overloads) << 3)
            | (u8::from(self.prelude) << 4)
            | (u8::from(self.types) << 5)
            | (u8::from(self.interfaces) << 6)
            | (u8::from(self.enums) << 7)
    }

    /// Unpack from the [`to_bits`](Self::to_bits) representation.
    #[must_use]
    pub fn from_bits(bits: u8) -> Self {
        Self {
            function_signatures: bits & 1 != 0,
            generic_syntax: bits & (1 << 1) != 0,
            generics: bits & (1 << 2) != 0,
            overloads: bits & (1 << 3) != 0,
            prelude: bits & (1 << 4) != 0,
            types: bits & (1 << 5) != 0,
            interfaces: bits & (1 << 6) != 0,
            enums: bits & (1 << 7) != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_source_differs_from_synthetic_and_from_any_real_id() {
        assert_ne!(Span::MANIFEST_SOURCE, Span::SYNTHETIC_SOURCE);
        // `ProjectIndex` hands out ids from 1 upward, so a real file would have
        // to open u32::MAX - 1 sources before it could collide.
        assert!(Span::MANIFEST_SOURCE.get() > 1);
        assert_eq!(Span::MANIFEST_SOURCE.get(), u32::MAX - 1);
    }

    #[test]
    fn feature_flags_bits_round_trip() {
        // Every distinct combination survives the salsa-wire bitmask encoding.
        for bits in 0u8..=u8::MAX {
            let f = FeatureFlags::from_bits(bits);
            assert_eq!(f.to_bits(), bits, "round-trip changed the bits");
        }
        // The fields map to the documented bit positions.
        let all = FeatureFlags {
            function_signatures: true,
            generic_syntax: true,
            generics: true,
            overloads: true,
            prelude: true,
            types: true,
            interfaces: true,
            enums: true,
        };
        assert_eq!(all.to_bits(), 0b1111_1111);
        assert_eq!(FeatureFlags::from_bits(0b1111_1111), all);
        assert_eq!(FeatureFlags::none(), FeatureFlags::from_bits(0));
        // A lone flag sets exactly its bit.
        let only_overloads = FeatureFlags {
            overloads: true,
            ..FeatureFlags::none()
        };
        assert_eq!(only_overloads.to_bits(), 0b0_1000);
    }

    #[test]
    fn span_union_grows() {
        let s = SourceId::new(1).unwrap();
        let a = Span::new(s, 5, 10);
        let b = Span::new(s, 8, 20);
        assert_eq!(a.union(b), Span::new(s, 5, 20));
    }

    #[test]
    fn synthetic_source_does_not_collide_with_first_real_file() {
        // Real files are numbered from 1 upward; the synthetic sentinel must
        // not equal the first real id, or every span from the first loaded
        // file would be indistinguishable from "no location".
        let first_real = SourceId::new(1).unwrap();
        assert_ne!(Span::synthetic().source, first_real);
        assert_eq!(Span::synthetic().source, Span::SYNTHETIC_SOURCE);
        assert_eq!(Span::SYNTHETIC_SOURCE.get(), u32::MAX);
    }

    #[test]
    fn display_col_counts_characters_not_bytes() {
        // `é` is two UTF-8 bytes, so the byte column and the display column
        // disagree from that character onward. Carets pad by display column.
        let text = "var café = 1\n";
        let table = LineTable::new(text);
        let at_one = offset(text.find('1').unwrap());
        assert_eq!(table.line_col(at_one).col, 13, "byte column");
        assert_eq!(table.display_col(text, at_one, 4), 12, "display column");
        assert_eq!(table.utf16_col(text, at_one), 12, "UTF-16 column");
    }

    #[test]
    fn display_and_utf16_columns_diverge_outside_the_bmp() {
        // `𝄞` is one display cell but two UTF-16 code units (a surrogate
        // pair), so the renderer's unit and the LSP's unit are not the same
        // quantity and must not be conflated.
        let text = "a𝄞b\n";
        let table = LineTable::new(text);
        let at_b = offset(text.find('b').unwrap());
        assert_eq!(table.line_col(at_b).col, 6, "byte column");
        assert_eq!(table.display_col(text, at_b, 4), 3);
        assert_eq!(table.utf16_col(text, at_b), 4);
    }

    #[test]
    fn display_col_expands_tabs_to_the_next_stop() {
        let text = "\treturn x\n";
        let table = LineTable::new(text);
        let at_return = 1;
        assert_eq!(table.line_col(at_return).col, 2, "byte column");
        assert_eq!(table.display_col(text, at_return, 4), 5);
        assert_eq!(table.display_col(text, at_return, 8), 9);
        // A tab mid-line advances to the stop, not by a full width.
        let text = "ab\tc\n";
        let table = LineTable::new(text);
        let at_c = offset(text.find('c').unwrap());
        assert_eq!(table.display_col(text, at_c, 4), 5);
    }

    #[test]
    fn expand_tabs_lines_up_with_display_col() {
        let text = "\tif (a)\n";
        let table = LineTable::new(text);
        let expanded = table.expand_tabs(text, 0, 4).unwrap();
        assert_eq!(expanded, "    if (a)");
        // The caret pad is `display_col - 1`; it must index the expanded line.
        let at_if = 1;
        let pad = (table.display_col(text, at_if, 4) - 1) as usize;
        assert_eq!(&expanded[pad..pad + 2], "if");
        // Nothing to expand: borrow rather than allocate.
        assert!(matches!(
            expand_tabs_in("plain", 4),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn columns_ignore_the_crlf_terminator() {
        let text = "var x = 1\r\nvar y = 2\r\n";
        let table = LineTable::new(text);
        let at_y = offset(text.rfind('y').unwrap());
        assert_eq!(table.line_col(at_y), LineCol { line: 2, col: 5 });
        assert_eq!(table.display_col(text, at_y, 4), 5);
        assert_eq!(table.utf16_col(text, at_y), 5);
    }

    #[test]
    fn an_offset_past_the_line_end_clamps_instead_of_running_on() {
        // The end offset of a span covering the last token sits on the
        // newline, and an EOF span sits past every line.
        let text = "abc\ndef\n";
        let table = LineTable::new(text);
        assert_eq!(table.display_col(text, 3, 4), 4, "on the newline");
        // Past EOF lands on the empty line after the final newline.
        assert_eq!(table.display_col(text, 99, 4), 1, "past EOF");
        assert_eq!(table.utf16_col(text, 99), 1);
    }

    #[test]
    fn line_table_basic() {
        let table = LineTable::new("a\nbb\nccc\n");
        assert_eq!(table.line_col(0), LineCol { line: 1, col: 1 });
        assert_eq!(table.line_col(2), LineCol { line: 2, col: 1 });
        assert_eq!(table.line_col(3), LineCol { line: 2, col: 2 });
        assert_eq!(table.line_col(5), LineCol { line: 3, col: 1 });
    }
}
