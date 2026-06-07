//! Big-O extraction.
//!
//! Walks a [`CostExpr`] and reduces it to a [`BigO`] complexity
//! class. The approach:
//!
//! 1. Distribute over `Sum` and `Max` — both pick the dominant
//!    term, with `Max` picking the term that's largest in every
//!    size dimension.
//! 2. Collapse a `Product` to a monomial in size variables, with a
//!    separate log-factor count.
//! 3. The result is a [`Term`] = (log_factors, {size_var: degree}).
//!    The biggest term across all summands wins. Constants count
//!    as degree 0.
//!
//! Comparison rule: higher polynomial degree wins; ties broken by
//! more log factors. So `n²` > `n·log(n)` > `n` > `log(n)` > `1`,
//! `n·m` is incomparable to `n²` (we surface both as
//! [`BigO::Polynomial`]).

use std::collections::BTreeMap;

use crate::cost_expr::{CostExpr, SizeVar};

/// A canonical complexity class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BigO {
    /// O(1).
    Constant,
    /// O(log v) — one log factor over `v`.
    Log(SizeVar),
    /// O(v) — linear in a single size variable.
    Linear(SizeVar),
    /// O(v · log(v)).
    NLogN(SizeVar),
    /// O(v²).
    Quadratic(SizeVar),
    /// Anything else expressible as a polynomial in size vars,
    /// optionally with log factors. Map values are exponents per
    /// size var; `log_factors` is the number of `log(·)` wrappers
    /// (treated multiplicatively, all over the dominant variable).
    Polynomial {
        degrees: BTreeMap<SizeVar, u32>,
        log_factors: u32,
    },
    /// Couldn't compute — recursion or data-dependent loop.
    Unknown,
}

impl BigO {
    /// Render as `O(...)`.
    pub fn render(&self) -> String {
        match self {
            BigO::Constant => "O(1)".into(),
            BigO::Log(v) => format!("O(log {v})"),
            BigO::Linear(v) => format!("O({v})"),
            BigO::NLogN(v) => format!("O({v} · log {v})"),
            BigO::Quadratic(v) => format!("O({v}²)"),
            BigO::Polynomial {
                degrees,
                log_factors,
            } => {
                let mut parts: Vec<String> = degrees
                    .iter()
                    .filter(|&(_, d)| *d > 0)
                    .map(|(v, &d)| match d {
                        1 => v.name.clone(),
                        2 => format!("{}²", v.name),
                        3 => format!("{}³", v.name),
                        _ => format!("{}^{d}", v.name),
                    })
                    .collect();
                for _ in 0..*log_factors {
                    parts.push("log".into());
                }
                if parts.is_empty() {
                    return "O(1)".into();
                }
                format!("O({})", parts.join(" · "))
            }
            BigO::Unknown => "O(?)".into(),
        }
    }
}

impl std::fmt::Display for BigO {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// A single canonical term in our pseudo-polynomial — a product
/// of size variables (each at some exponent) with an optional log
/// factor count. `Const(k)` collapses to `Term::constant()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Term {
    /// Exponent per size variable.
    degrees: BTreeMap<SizeVar, u32>,
    /// Multiplicative log factors. We don't track *which* variable
    /// the log is over because in practice it always sits next to
    /// a matching size factor (`n · log n` from a halving loop).
    log_factors: u32,
    /// True if the expression involves an `Unknown`. Wins over
    /// every other classification.
    unknown: bool,
}

impl Term {
    fn constant() -> Self {
        Self::default()
    }

    /// Multiply two terms together (loop-bound × body).
    fn mul(mut a: Term, b: Term) -> Term {
        if a.unknown || b.unknown {
            return Term {
                unknown: true,
                ..Default::default()
            };
        }
        for (v, d) in b.degrees {
            *a.degrees.entry(v).or_insert(0) += d;
        }
        a.log_factors += b.log_factors;
        a
    }

    /// Picks the dominant of two terms; if neither dominates, returns
    /// a fresh term whose degrees are the per-variable maximum (the
    /// "worst case among incomparable monomials" — what we'd report
    /// from a `Max(n², m²)` for instance).
    fn dominant(a: Term, b: Term) -> Term {
        if a.unknown || b.unknown {
            return Term {
                unknown: true,
                ..Default::default()
            };
        }
        // Direct comparison: if a ≥ b in every dimension (and log
        // factors), keep a.
        let a_dom_b = dominates_or_equal(&a, &b);
        let b_dom_a = dominates_or_equal(&b, &a);
        match (a_dom_b, b_dom_a) {
            (true, _) => a,
            (false, true) => b,
            (false, false) => {
                // Incomparable — take elementwise max of degrees.
                let mut degrees = a.degrees;
                for (v, d) in b.degrees {
                    let e = degrees.entry(v).or_insert(0);
                    if d > *e {
                        *e = d;
                    }
                }
                Term {
                    degrees,
                    log_factors: a.log_factors.max(b.log_factors),
                    unknown: false,
                }
            }
        }
    }
}

/// True iff every degree in `a` is ≥ the corresponding degree in
/// `b`, and `a.log_factors >= b.log_factors`. A missing key counts
/// as degree 0.
fn dominates_or_equal(a: &Term, b: &Term) -> bool {
    if a.log_factors < b.log_factors {
        // We tolerate fewer log factors only if a has a strictly
        // higher polynomial degree on every variable.
        let mut strictly_more = false;
        for (v, &bd) in &b.degrees {
            let ad = a.degrees.get(v).copied().unwrap_or(0);
            if ad < bd {
                return false;
            }
            if ad > bd {
                strictly_more = true;
            }
        }
        for (v, &ad) in &a.degrees {
            let bd = b.degrees.get(v).copied().unwrap_or(0);
            if ad > bd {
                strictly_more = true;
            }
            if ad < bd {
                return false;
            }
        }
        return strictly_more;
    }
    for (v, &bd) in &b.degrees {
        let ad = a.degrees.get(v).copied().unwrap_or(0);
        if ad < bd {
            return false;
        }
    }
    true
}

/// Reduce a [`CostExpr`] to its dominant term.
fn dominant_term(expr: &CostExpr) -> Term {
    match expr {
        CostExpr::Const(_) => Term::constant(),
        CostExpr::Size(v) => Term {
            degrees: {
                let mut m = BTreeMap::new();
                m.insert(v.clone(), 1);
                m
            },
            ..Default::default()
        },
        CostExpr::Log(inner) => {
            // log of a constant is 0 (already simplified out); log
            // of a size var contributes a log factor at degree 0 on
            // that var. We treat `log(anything else)` conservatively
            // as a single log factor.
            let inner_term = dominant_term(inner);
            if inner_term.unknown {
                return inner_term;
            }
            // The "size" of the inner doesn't carry — we strip its
            // degrees and just record a log factor.
            Term {
                log_factors: inner_term.log_factors + 1,
                ..Default::default()
            }
        }
        CostExpr::Sum(parts) => parts
            .iter()
            .map(dominant_term)
            .reduce(Term::dominant)
            .unwrap_or_default(),
        CostExpr::Max(parts) => parts
            .iter()
            .map(dominant_term)
            .reduce(Term::dominant)
            .unwrap_or_default(),
        CostExpr::Product(parts) => {
            let mut acc = Term::constant();
            for p in parts {
                acc = Term::mul(acc, dominant_term(p));
            }
            acc
        }
        CostExpr::Unknown(_) => Term {
            unknown: true,
            ..Default::default()
        },
    }
}

/// Top-level: classify a CostExpr.
pub fn big_o(expr: &CostExpr) -> BigO {
    let t = dominant_term(expr);
    if t.unknown {
        return BigO::Unknown;
    }
    let degrees: BTreeMap<SizeVar, u32> = t.degrees.into_iter().filter(|(_, d)| *d > 0).collect();
    match (degrees.len(), t.log_factors) {
        (0, 0) => BigO::Constant,
        (0, _) => {
            // log factors without a polynomial base — `log(n)`
            // alone collapses here. We can't tell which variable
            // the log is over from this term alone; check the
            // expression for the single Size descendant.
            if let Some(v) = single_size_var(expr) {
                BigO::Log(v)
            } else {
                BigO::Polynomial {
                    degrees,
                    log_factors: t.log_factors,
                }
            }
        }
        (1, 0) => {
            let (v, d) = degrees.into_iter().next().unwrap();
            match d {
                1 => BigO::Linear(v),
                2 => BigO::Quadratic(v),
                _ => {
                    let mut m = BTreeMap::new();
                    m.insert(v, d);
                    BigO::Polynomial {
                        degrees: m,
                        log_factors: 0,
                    }
                }
            }
        }
        (1, log) => {
            let (v, d) = degrees.into_iter().next().unwrap();
            if d == 1 && log == 1 {
                BigO::NLogN(v)
            } else {
                let mut m = BTreeMap::new();
                m.insert(v, d);
                BigO::Polynomial {
                    degrees: m,
                    log_factors: log,
                }
            }
        }
        _ => BigO::Polynomial {
            degrees,
            log_factors: t.log_factors,
        },
    }
}

/// Returns the single `SizeVar` mentioned in `expr` if there's
/// exactly one; otherwise `None`. Used to attribute a bare
/// log-factor to a specific size variable.
fn single_size_var(expr: &CostExpr) -> Option<SizeVar> {
    let mut seen: Option<SizeVar> = None;
    let mut stack = vec![expr];
    while let Some(node) = stack.pop() {
        match node {
            CostExpr::Size(v) => match &seen {
                Some(s) if s != v => return None,
                Some(_) => {}
                None => seen = Some(v.clone()),
            },
            CostExpr::Log(inner) => stack.push(inner),
            CostExpr::Sum(parts) | CostExpr::Product(parts) | CostExpr::Max(parts) => {
                stack.extend(parts.iter());
            }
            _ => {}
        }
    }
    seen
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
    fn constant_is_constant() {
        assert_eq!(big_o(&CostExpr::k(42)), BigO::Constant);
    }

    #[test]
    fn linear_in_n() {
        let e = CostExpr::sum(vec![CostExpr::Size(n()), CostExpr::k(7)]);
        assert_eq!(big_o(&e), BigO::Linear(n()));
    }

    #[test]
    fn quadratic_dominates_linear() {
        let e = CostExpr::sum(vec![
            CostExpr::product(vec![CostExpr::Size(n()), CostExpr::Size(n())]),
            CostExpr::product(vec![CostExpr::k(5), CostExpr::Size(n())]),
            CostExpr::k(100),
        ]);
        assert_eq!(big_o(&e), BigO::Quadratic(n()));
    }

    #[test]
    fn n_log_n_classifies_correctly() {
        // n · log(n)
        let e = CostExpr::product(vec![
            CostExpr::Size(n()),
            CostExpr::Log(Box::new(CostExpr::Size(n()))),
        ]);
        assert_eq!(big_o(&e), BigO::NLogN(n()));
    }

    #[test]
    fn product_of_two_distinct_vars_is_polynomial() {
        let e = CostExpr::product(vec![CostExpr::Size(n()), CostExpr::Size(m())]);
        let r = big_o(&e).render();
        assert!(r.contains('n'), "got {r}");
        assert!(r.contains('m'), "got {r}");
    }

    #[test]
    fn unknown_wins_over_everything() {
        let e = CostExpr::sum(vec![CostExpr::Size(n()), CostExpr::Unknown("recursive")]);
        assert_eq!(big_o(&e), BigO::Unknown);
    }

    #[test]
    fn max_picks_dominant_branch() {
        // if cond { O(n²) } else { O(n) } → O(n²).
        let e = CostExpr::max(vec![
            CostExpr::product(vec![CostExpr::Size(n()), CostExpr::Size(n())]),
            CostExpr::Size(n()),
        ]);
        assert_eq!(big_o(&e), BigO::Quadratic(n()));
    }

    #[test]
    fn lone_log_renders_with_var() {
        let e = CostExpr::Log(Box::new(CostExpr::Size(n())));
        assert_eq!(big_o(&e), BigO::Log(n()));
    }
}
