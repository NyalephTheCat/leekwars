//! Public symbol / reference table built up by the resolver as it
//! walks. The LSP reads this to answer go-to-definition and other
//! navigation requests; consumers who only need diagnostics ignore
//! it.

use leek_span::Span;

use crate::scope::SymbolKind;

leek_span::newtype_index! {
    /// Stable index into [`ResolveTable::symbols`]. Monotonically
    /// allocated as the resolver visits each declaration.
    pub struct SymbolId;
}

/// One declared name and the span where it lives.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub name: String,
    /// Span of the identifier token itself — what go-to-def jumps to.
    pub def_span: Span,
    /// Span of the whole declaration node (function, class, var
    /// statement). Equal to `def_span` for symbols that don't have
    /// a meaningful enclosing node.
    pub full_span: Span,
    /// Enclosing class / function symbol, if any. `None` for
    /// top-level declarations.
    pub container: Option<SymbolId>,
}

/// A successful name resolution from a reference token to a
/// declaration. `name_offset` / `name_len` give the source-byte
/// range of the *reference* — the LSP locates a click via offset
/// binary-search over [`ResolveTable::references`].
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedRef {
    pub name_offset: u32,
    pub name_len: u32,
    pub target: SymbolId,
}

/// Everything the resolver collected for LSP-style navigation.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveTable {
    pub symbols: Vec<Symbol>,
    /// Sorted by `name_offset` so callers can binary-search by
    /// cursor position.
    pub references: Vec<ResolvedRef>,
}

impl ResolveTable {
    /// Binary search the references for the one covering
    /// `cursor_offset` (i.e. `name_offset <= cursor < name_offset + name_len`).
    pub fn reference_at(&self, cursor_offset: u32) -> Option<&ResolvedRef> {
        // Search for the largest name_offset <= cursor.
        let idx = match self
            .references
            .binary_search_by_key(&cursor_offset, |r| r.name_offset)
        {
            Ok(i) => i,
            Err(i) if i > 0 => i - 1,
            Err(_) => return None,
        };
        let r = &self.references[idx];
        if cursor_offset < r.name_offset + r.name_len {
            Some(r)
        } else {
            None
        }
    }

    pub fn symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.get(id.0 as usize)
    }
}
