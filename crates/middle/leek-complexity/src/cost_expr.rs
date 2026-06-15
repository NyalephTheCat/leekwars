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

/// The root a size variable hangs off — the stable input whose value
/// (or one of whose fields) carries the element count.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SizeRoot {
    /// 0-based parameter position.
    Param(u32),
    /// The enclosing instance (`this`) inside a method body.
    This,
    /// A top-level global, identified by its declaration index
    /// (`DefId`'s raw value). Like `This`, it's shared state the
    /// caller can't supply, so it passes through substitution.
    Global(u32),
}

/// Where a size variable comes from: a [`root`](SizeSource::root) plus a
/// field-access [`path`](SizeSource::path) off it. The empty path means
/// the root value itself (`count(arr)`); a non-empty path is a field
/// chain (`this.data`, `obj.cells`, `this.grid.rows`).
///
/// Drives identity (two `SizeVar`s are equal iff their `source` matches)
/// and substitution: a parameter root is rewritten at a call site (its
/// field path composed onto the argument's), while a `this` root is
/// *instance state* the caller can't supply, so it passes through and
/// surfaces in the method's own big-O.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SizeSource {
    pub root: SizeRoot,
    pub path: Vec<String>,
}

/// A size variable — the element count of a sized value (array / map /
/// set / string), identified by its access path from a stable root
/// (`count(arr)`, `count(this.data)`, `count(obj.cells)`). Two
/// `SizeVar`s are equal iff their [`source`](SizeVar::source) matches;
/// `name` is purely cosmetic (for display).
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone)]
pub struct SizeVar {
    /// Identity of the size variable.
    pub source: SizeSource,
    /// Display name. Conventionally the parameter / field access path,
    /// or `n`, `m`, ... when no name is known.
    pub name: String,
}

impl SizeVar {
    /// A parameter-derived size variable at 0-based position `index`
    /// (the parameter value itself, no field path).
    pub fn new(index: u32, name: impl Into<String>) -> Self {
        Self {
            source: SizeSource {
                root: SizeRoot::Param(index),
                path: Vec::new(),
            },
            name: name.into(),
        }
    }

    /// A field-derived size variable for `this.<name>`.
    pub fn field(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            source: SizeSource {
                root: SizeRoot::This,
                path: vec![name.clone()],
            },
            name,
        }
    }

    /// A global-derived size variable (the global value itself, no
    /// field path). `index` is the global's `DefId` raw value.
    pub fn global(index: u32, name: impl Into<String>) -> Self {
        Self {
            source: SizeSource {
                root: SizeRoot::Global(index),
                path: Vec::new(),
            },
            name: name.into(),
        }
    }

    /// The `this` object root (`{root: This, path: []}`). Not a sized
    /// value on its own — only a field path off it is — but the base a
    /// resolver extends with [`with_field`](Self::with_field).
    pub(crate) fn this_object() -> Self {
        Self {
            source: SizeSource {
                root: SizeRoot::This,
                path: Vec::new(),
            },
            name: String::new(),
        }
    }

    /// Extend this location with one more field access (`base.field`),
    /// updating both the path and the display name.
    pub(crate) fn with_field(mut self, field: &str) -> Self {
        self.source.path.push(field.to_string());
        self.name = if self.name.is_empty() {
            field.to_string()
        } else {
            format!("{}.{field}", self.name)
        };
        self
    }

    /// Append a whole field chain — used to compose a callee's
    /// `param.field…` path onto the argument's location during
    /// substitution.
    pub(crate) fn extend(mut self, more: &[String]) -> Self {
        for f in more {
            self = self.with_field(f);
        }
        self
    }

    /// The parameter position this size substitutes for at a call site:
    /// `Some(i)` only for a bare parameter (root `Param(i)`, empty path).
    /// Field paths and `this` roots return `None`.
    pub fn param_index(&self) -> Option<u32> {
        match (&self.source.root, self.source.path.is_empty()) {
            (SizeRoot::Param(i), true) => Some(*i),
            _ => None,
        }
    }
}

// Identity / ordering / hashing are by `source` only; `name` is
// cosmetic, so two references to the same parameter or field compare
// equal even if rendered under different display names.
impl PartialEq for SizeVar {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}
impl Eq for SizeVar {}
impl std::hash::Hash for SizeVar {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.source.hash(state);
    }
}
impl PartialOrd for SizeVar {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SizeVar {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.source.cmp(&other.source)
    }
}

impl fmt::Display for SizeVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// Symbolic ops-cost expression.
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
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

    /// Substitute every parameter [`Size`] occurrence with a
    /// caller-provided `CostExpr`, keyed by parameter position.
    /// Unmapped *parameter* size variables become [`CostExpr::Unknown`]
    /// (conservative — we never silently drop a size dependency).
    ///
    /// Field size variables ([`SizeSource::Field`]) are **not**
    /// substituted: a callee's field size is instance state the caller
    /// can't supply, so it passes through unchanged and stays part of
    /// the callee's reported complexity.
    ///
    /// Used by call-graph substitution: when caller `f` calls
    /// callee `g`, we replace each `Size(p)` in g's formula with
    /// "the size of f's expression passed to g's parameter p".
    ///
    /// [`Size`]: CostExpr::Size
    /// [`SizeSource::Field`]: crate::cost_expr::SizeSource::Field
    pub fn substitute(&self, sub: &std::collections::HashMap<u32, CostExpr>) -> CostExpr {
        match self {
            CostExpr::Const(c) => CostExpr::Const(*c),
            CostExpr::Size(v) => substitute_size(v, sub),
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
    /// every parameter `Size(v)` to `sizes[idx]` and folds. Used
    /// by the empirical harness to predict ops at a concrete size.
    /// Field sizes (no parameter index) and unknowns short-circuit
    /// to `None`.
    pub fn evaluate_at(&self, sizes: &std::collections::HashMap<u32, u64>) -> Option<u64> {
        match self {
            CostExpr::Const(c) => Some(*c),
            CostExpr::Size(v) => sizes.get(&v.param_index()?).copied(),
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

/// Substitute one [`Size`](CostExpr::Size) at a call site.
///
/// - A `this`-rooted size is instance state the caller can't supply, so
///   it passes through unchanged.
/// - A bare parameter (`root: Param(i)`, empty path) becomes the
///   caller's argument size, or `Unknown` if the parameter wasn't mapped.
/// - A parameter *field path* (`obj.field…`) retargets onto the
///   argument's location: if the argument resolves to another size
///   location we compose the paths (`f(a)` where `f` reads `obj.field`
///   ⇒ `a.field`); otherwise we can't tie the field to an input and
///   yield `Unknown`.
fn substitute_size(v: &SizeVar, sub: &std::collections::HashMap<u32, CostExpr>) -> CostExpr {
    let SizeRoot::Param(idx) = v.source.root else {
        // `this` / global root — shared or instance state the caller
        // can't supply, so keep it as-is.
        return CostExpr::Size(v.clone());
    };
    let Some(arg) = sub.get(&idx) else {
        return CostExpr::Unknown("callee size variable not mapped at call site");
    };
    if v.source.path.is_empty() {
        return arg.clone();
    }
    // Field path off the parameter: compose onto the argument's location.
    match arg {
        CostExpr::Size(arg_var) => CostExpr::Size(arg_var.clone().extend(&v.source.path)),
        _ => CostExpr::Unknown("can't resolve field path through non-aggregate argument"),
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
    fn substitute_passes_field_sizes_through() {
        // A field size is instance state — it survives substitution
        // unchanged rather than becoming Unknown (the way an unmapped
        // parameter would).
        let e = CostExpr::Size(SizeVar::field("data"));
        let sub = std::collections::HashMap::<u32, CostExpr>::new();
        let out = e.substitute(&sub);
        assert_eq!(out, CostExpr::Size(SizeVar::field("data")));
    }

    #[test]
    fn field_and_param_sizes_have_distinct_identities() {
        // Field "data" and param 0 are different variables; two fields
        // with different names are different too.
        assert_ne!(SizeVar::field("data"), SizeVar::new(0, "data"));
        assert_ne!(SizeVar::field("a"), SizeVar::field("b"));
        assert_eq!(SizeVar::field("a"), SizeVar::field("a"));
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
