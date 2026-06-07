//! Symbolic cost expressions.
//!
//! [`CostExpr`] generalises [`leek_charge`]'s scalar `u64` to a
//! tree that can carry parameter-derived size variables. The
//! analyser builds one of these per user function; [`big_o`]
//! reduces it to a complexity class for display.
//!
//! [`leek_charge`]: ../../leek-charge/index.html
//! [`big_o`]: super::big_o

use std::fmt;

/// A size variable derived from a parameter — e.g. `count(arr0)`
/// for the first parameter when it's typed as an array. Two
/// `SizeVar`s are equal iff their `param_index` matches; `name`
/// is purely cosmetic (for display).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SizeVar {
    /// 0-based parameter position the size refers to.
    pub param_index: u32,
    /// Display name. Conventionally the parameter's identifier or
    /// `n`, `m`, ... when no name is known.
    pub name: String,
}

impl SizeVar {
    pub fn new(param_index: u32, name: impl Into<String>) -> Self {
        Self {
            param_index,
            name: name.into(),
        }
    }
}

impl fmt::Display for SizeVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// Symbolic ops-cost expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CostExpr {
    /// A literal ops count. The straight-line floor of `leek-charge`
    /// flows through here unchanged.
    Const(u64),
    /// `count(param_i)` or `length(param_i)` — a parameter-derived
    /// size variable.
    Size(SizeVar),
    /// `log2(inner)` — emitted for binary-search-style loops where
    /// the counter multiplies by a constant each iteration.
    Log(Box<CostExpr>),
    /// Sum of terms. Always >=2 elements after simplification.
    Sum(Vec<CostExpr>),
    /// Product of (loop bound, body cost) or any other pairing.
    /// After simplification this is canonical: factors in a fixed
    /// order with all constants collected into the left factor.
    Product(Vec<CostExpr>),
    /// Maximum of branch costs — `if`/`switch` worst case.
    Max(Vec<CostExpr>),
    /// Something we couldn't symbolically determine (data-dependent
    /// loop, recursion, unresolved call). Carries a static reason
    /// string for human reporting. Treated as conservatively
    /// "non-constant" by big-O extraction.
    Unknown(&'static str),
}

impl CostExpr {
    /// 0 ops.
    pub fn zero() -> Self {
        CostExpr::Const(0)
    }

    /// Convenience: a constant.
    pub fn k(n: u64) -> Self {
        CostExpr::Const(n)
    }

    /// Sum constructor that pre-flattens children. Use this from
    /// the analyser rather than constructing `Sum` directly so the
    /// IR never sees nested-sum shapes.
    pub fn sum(parts: Vec<CostExpr>) -> Self {
        let mut flat: Vec<CostExpr> = Vec::with_capacity(parts.len());
        for p in parts {
            match p {
                CostExpr::Sum(inner) => flat.extend(inner),
                CostExpr::Const(0) => {}
                other => flat.push(other),
            }
        }
        match flat.len() {
            0 => CostExpr::Const(0),
            1 => flat.into_iter().next().unwrap(),
            _ => CostExpr::Sum(flat),
        }
        .simplify()
    }

    /// Product constructor — flattens nested products and absorbs
    /// `Const(1)` factors. A zero factor short-circuits to zero.
    pub fn product(parts: Vec<CostExpr>) -> Self {
        let mut flat: Vec<CostExpr> = Vec::with_capacity(parts.len());
        for p in parts {
            match p {
                CostExpr::Product(inner) => flat.extend(inner),
                CostExpr::Const(1) => {}
                CostExpr::Const(0) => return CostExpr::Const(0),
                other => flat.push(other),
            }
        }
        match flat.len() {
            0 => CostExpr::Const(1),
            1 => flat.into_iter().next().unwrap(),
            _ => CostExpr::Product(flat),
        }
        .simplify()
    }

    /// Max constructor that drops duplicates and constant-zeros.
    pub fn max(parts: Vec<CostExpr>) -> Self {
        let mut deduped: Vec<CostExpr> = Vec::new();
        for p in parts {
            if matches!(p, CostExpr::Const(0)) {
                continue;
            }
            if !deduped.iter().any(|q| q == &p) {
                deduped.push(p);
            }
        }
        match deduped.len() {
            0 => CostExpr::Const(0),
            1 => deduped.into_iter().next().unwrap(),
            _ => CostExpr::Max(deduped),
        }
    }

    /// One-pass local simplification. Idempotent: calling twice
    /// returns the same shape. The analyser already prefers
    /// [`sum`](Self::sum) / [`product`](Self::product) constructors;
    /// `simplify` exists for the cases that fall through (subtree
    /// transformations after construction).
    pub fn simplify(self) -> Self {
        match self {
            CostExpr::Sum(parts) => {
                let mut k: u64 = 0;
                let mut others: Vec<CostExpr> = Vec::with_capacity(parts.len());
                for p in parts {
                    match p.simplify() {
                        CostExpr::Const(c) => k = k.saturating_add(c),
                        CostExpr::Sum(inner) => {
                            // Already a flat sum after recursive simplify; flatten.
                            for q in inner {
                                if let CostExpr::Const(c) = q {
                                    k = k.saturating_add(c);
                                } else {
                                    others.push(q);
                                }
                            }
                        }
                        other => others.push(other),
                    }
                }
                if k != 0 {
                    others.push(CostExpr::Const(k));
                }
                match others.len() {
                    0 => CostExpr::Const(0),
                    1 => others.into_iter().next().unwrap(),
                    _ => CostExpr::Sum(others),
                }
            }
            CostExpr::Product(parts) => {
                let mut k: u64 = 1;
                let mut others: Vec<CostExpr> = Vec::with_capacity(parts.len());
                for p in parts {
                    match p.simplify() {
                        CostExpr::Const(0) => return CostExpr::Const(0),
                        CostExpr::Const(c) => k = k.saturating_mul(c),
                        CostExpr::Product(inner) => {
                            for q in inner {
                                if let CostExpr::Const(c) = q {
                                    k = k.saturating_mul(c);
                                } else {
                                    others.push(q);
                                }
                            }
                        }
                        other => others.push(other),
                    }
                }
                if k == 0 {
                    return CostExpr::Const(0);
                }
                if k != 1 {
                    others.insert(0, CostExpr::Const(k));
                }
                match others.len() {
                    0 => CostExpr::Const(1),
                    1 => others.into_iter().next().unwrap(),
                    _ => CostExpr::Product(others),
                }
            }
            CostExpr::Max(parts) => {
                let parts: Vec<CostExpr> = parts.into_iter().map(CostExpr::simplify).collect();
                CostExpr::max(parts)
            }
            CostExpr::Log(inner) => {
                let inner = inner.simplify();
                if let CostExpr::Const(c) = &inner {
                    // log2(1) = 0, log2(0) is undefined → 0.
                    if *c <= 1 {
                        return CostExpr::Const(0);
                    }
                }
                CostExpr::Log(Box::new(inner))
            }
            other => other,
        }
    }

    /// Substitute every [`Size`] occurrence with a caller-provided
    /// `CostExpr`, keyed by `SizeVar::param_index`. Unmapped size
    /// variables become [`CostExpr::Unknown`] — the substitution is
    /// then conservative: we never silently drop a size dependency.
    ///
    /// Used by call-graph substitution: when caller `f` calls
    /// callee `g`, we replace each `Size(p)` in g's formula with
    /// "the size of f's expression passed to g's parameter p".
    ///
    /// [`Size`]: CostExpr::Size
    pub fn substitute(&self, sub: &std::collections::HashMap<u32, CostExpr>) -> CostExpr {
        match self {
            CostExpr::Const(c) => CostExpr::Const(*c),
            CostExpr::Size(v) => sub
                .get(&v.param_index)
                .cloned()
                .unwrap_or(CostExpr::Unknown(
                    "callee size variable not mapped at call site",
                )),
            CostExpr::Log(inner) => CostExpr::Log(Box::new(inner.substitute(sub))).simplify(),
            CostExpr::Sum(parts) => {
                CostExpr::sum(parts.iter().map(|p| p.substitute(sub)).collect())
            }
            CostExpr::Product(parts) => {
                CostExpr::product(parts.iter().map(|p| p.substitute(sub)).collect())
            }
            CostExpr::Max(parts) => {
                CostExpr::max(parts.iter().map(|p| p.substitute(sub)).collect())
            }
            CostExpr::Unknown(r) => CostExpr::Unknown(r),
        }
    }

    /// Walk the expression and return the smallest constant
    /// upper-bound substitution that turns it into a scalar. Sets
    /// every `Size(v)` to `sizes[&v.param_index]` and folds. Used
    /// by the empirical harness to predict ops at a concrete size.
    /// Unknowns short-circuit to `None`.
    pub fn evaluate_at(&self, sizes: &std::collections::HashMap<u32, u64>) -> Option<u64> {
        match self {
            CostExpr::Const(c) => Some(*c),
            CostExpr::Size(v) => sizes.get(&v.param_index).copied(),
            CostExpr::Log(inner) => {
                let v = inner.evaluate_at(sizes)?;
                if v <= 1 {
                    Some(0)
                } else {
                    Some(u64::from(64 - v.leading_zeros()))
                }
            }
            CostExpr::Sum(parts) => {
                let mut acc: u64 = 0;
                for p in parts {
                    acc = acc.saturating_add(p.evaluate_at(sizes)?);
                }
                Some(acc)
            }
            CostExpr::Product(parts) => {
                let mut acc: u64 = 1;
                for p in parts {
                    acc = acc.saturating_mul(p.evaluate_at(sizes)?);
                }
                Some(acc)
            }
            CostExpr::Max(parts) => parts
                .iter()
                .map(|p| p.evaluate_at(sizes))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .max(),
            CostExpr::Unknown(_) => None,
        }
    }

    /// Pretty-print as a single-line ops formula.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out, Prec::Lowest);
        out
    }

    fn render_into(&self, out: &mut String, parent: Prec) {
        match self {
            CostExpr::Const(n) => out.push_str(&n.to_string()),
            CostExpr::Size(v) => out.push_str(&v.name),
            CostExpr::Log(inner) => {
                out.push_str("log(");
                inner.render_into(out, Prec::Lowest);
                out.push(')');
            }
            CostExpr::Sum(parts) => {
                let wrap = parent > Prec::Sum;
                if wrap {
                    out.push('(');
                }
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" + ");
                    }
                    p.render_into(out, Prec::Sum);
                }
                if wrap {
                    out.push(')');
                }
            }
            CostExpr::Product(parts) => {
                let wrap = parent > Prec::Product;
                if wrap {
                    out.push('(');
                }
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        out.push('·');
                    }
                    p.render_into(out, Prec::Product);
                }
                if wrap {
                    out.push(')');
                }
            }
            CostExpr::Max(parts) => {
                out.push_str("max(");
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    p.render_into(out, Prec::Lowest);
                }
                out.push(')');
            }
            CostExpr::Unknown(reason) => {
                out.push('?');
                out.push('(');
                out.push_str(reason);
                out.push(')');
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Prec {
    Lowest = 0,
    Sum = 1,
    Product = 2,
}

impl fmt::Display for CostExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n() -> SizeVar {
        SizeVar::new(0, "n")
    }
    fn m() -> SizeVar {
        SizeVar::new(1, "m")
    }

    #[test]
    fn sum_folds_constants() {
        let e = CostExpr::sum(vec![CostExpr::k(3), CostExpr::Size(n()), CostExpr::k(5)]);
        // After simplification we expect `n + 8` (constant
        // gathered to the right).
        let r = e.render();
        assert!(r.contains('n') && r.contains('8'), "got: {r}");
    }

    #[test]
    fn sum_drops_zeros_and_flattens() {
        let e = CostExpr::sum(vec![
            CostExpr::k(0),
            CostExpr::sum(vec![CostExpr::Size(n()), CostExpr::k(2)]),
            CostExpr::k(0),
        ]);
        assert_eq!(e.render(), "n + 2");
    }

    #[test]
    fn product_short_circuits_on_zero() {
        let e = CostExpr::product(vec![CostExpr::k(0), CostExpr::Size(n())]);
        assert_eq!(e.render(), "0");
    }

    #[test]
    fn product_drops_one_and_flattens() {
        let e = CostExpr::product(vec![
            CostExpr::k(1),
            CostExpr::Size(n()),
            CostExpr::product(vec![CostExpr::Size(m()), CostExpr::k(3)]),
        ]);
        // Expect 3·n·m (constants collected, then size vars).
        let r = e.render();
        assert!(r.contains('3'), "got: {r}");
        assert!(r.contains('n'), "got: {r}");
        assert!(r.contains('m'), "got: {r}");
    }

    #[test]
    fn max_dedups_identical_branches() {
        let e = CostExpr::max(vec![CostExpr::Size(n()), CostExpr::Size(n())]);
        // Both branches identical → just `n`, no max wrapper.
        assert_eq!(e.render(), "n");
    }

    #[test]
    fn log_of_constant_one_or_less_is_zero() {
        let e = CostExpr::Log(Box::new(CostExpr::k(1))).simplify();
        assert_eq!(e, CostExpr::Const(0));
    }

    #[test]
    fn substitute_replaces_sizes() {
        let e = CostExpr::sum(vec![
            CostExpr::product(vec![CostExpr::Size(n()), CostExpr::k(3)]),
            CostExpr::k(2),
        ]);
        let mut sub = std::collections::HashMap::new();
        sub.insert(0, CostExpr::Size(SizeVar::new(7, "arr")));
        let out = e.substitute(&sub);
        let r = out.render();
        assert!(r.contains("arr"), "got: {r}");
        assert!(
            !r.contains("n + ") && !r.starts_with('n'),
            "stale n in: {r}"
        );
    }

    #[test]
    fn substitute_unmapped_becomes_unknown() {
        let e = CostExpr::Size(n());
        let sub = std::collections::HashMap::<u32, CostExpr>::new();
        let out = e.substitute(&sub);
        assert!(matches!(out, CostExpr::Unknown(_)));
    }

    #[test]
    fn evaluate_at_folds_to_scalar() {
        // 6·n + 12 at n=10 → 72.
        let e = CostExpr::sum(vec![
            CostExpr::product(vec![CostExpr::k(6), CostExpr::Size(n())]),
            CostExpr::k(12),
        ]);
        let mut sizes = std::collections::HashMap::new();
        sizes.insert(0, 10);
        assert_eq!(e.evaluate_at(&sizes), Some(72));
    }

    #[test]
    fn evaluate_at_propagates_unknown_as_none() {
        let e = CostExpr::sum(vec![CostExpr::Size(n()), CostExpr::Unknown("recursion")]);
        let mut sizes = std::collections::HashMap::new();
        sizes.insert(0, 5);
        assert_eq!(e.evaluate_at(&sizes), None);
    }

    #[test]
    fn sum_renders_with_separators() {
        let e = CostExpr::sum(vec![
            CostExpr::product(vec![CostExpr::k(6), CostExpr::Size(n())]),
            CostExpr::k(12),
        ]);
        let r = e.render();
        assert!(r.contains(" + "), "got: {r}");
        assert!(r.contains("·") || r.contains('6'), "got: {r}");
    }
}
