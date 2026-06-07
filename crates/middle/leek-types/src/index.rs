//! Public type table built by the checker as it walks. The LSP
//! reads this to answer hover requests; consumers who only need
//! diagnostics ignore it.

use std::collections::HashMap;

use leek_span::Span;

use crate::ty::Type;

/// Declared/inferred signature data the checker accumulates, surfaced so
/// the LSP can render a declaration's return/field type even when the
/// source omits the annotation. Keyed by name (and, for members, by the
/// owning class). `Type::Any` entries mean "no better information".
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InferredSignatures {
    /// Top-level function name → inferred/declared return type.
    pub fn_returns: HashMap<String, Type>,
    /// Top-level function name → declared parameter types (in order), so
    /// hover can render a function's value type `Function<P0, … => R>`.
    pub fn_params: HashMap<String, Vec<Type>>,
    /// Class name → field name → declared/inferred field type.
    pub field_types: HashMap<String, HashMap<String, Type>>,
    /// Class name → method name → declared/inferred return type.
    pub method_returns: HashMap<String, HashMap<String, Type>>,
}

/// One expression with its inferred [`Type`].
///
/// Hover does a "smallest span that covers cursor" binary search
/// over [`TypeTable::exprs`] — the smallest containing entry is the
/// most-nested expression and yields the most specific type.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedExpr {
    pub span: Span,
    pub ty: Type,
}

#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeTable {
    /// Sorted by `span.start` so callers can binary-search by cursor
    /// position.
    pub exprs: Vec<TypedExpr>,
}

impl TypeTable {
    /// Return the innermost typed expression covering the cursor
    /// offset (smallest containing span), if any.
    pub fn smallest_at(&self, cursor_offset: u32) -> Option<&TypedExpr> {
        // The vector is sorted by span.start. Walk candidates whose
        // start <= cursor and pick the one with the smallest length
        // (== innermost) among those whose end > cursor.
        let cutoff = match self
            .exprs
            .binary_search_by_key(&cursor_offset, |t| t.span.start)
        {
            Ok(i) => i + 1, // include exact-match
            Err(i) => i,
        };
        self.exprs[..cutoff]
            .iter()
            .rev()
            .filter(|t| t.span.start <= cursor_offset && cursor_offset < t.span.end)
            .min_by_key(|t| t.span.end - t.span.start)
    }
}
