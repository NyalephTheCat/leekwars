//! L0041 `MapAsSet` (nursery, LeekScript 4+) — flag a map whose every
//! stored value is `true`: it's a set wearing a map costume.
//!
//! ```leekscript
//! var seen = [:]
//! for (var cell in cells) {
//!     seen[cell] = true        // value never matters
//! }
//! if (target in seen) { … }
//!
//! var seen = <>                // a set says it directly
//! setPut(seen, cell)
//! if (target in seen) { … }
//! ```
//!
//! ## Detection
//!
//! Per body: a local declared with an empty map literal (or a map
//! literal whose values are all `true`) where **every** use is
//! set-shaped — `m[k] = true` writes, `m[k]` / `k in m` /
//! `mapContainsKey` reads, `mapRemove`, size queries. One use that
//! doesn't fit (stored elsewhere, passed to another call, a non-`true`
//! value written) disqualifies the whole candidate.
//!
//! Gated on [`crate::LintOptions::version`] ≥ 4 — older scripts don't
//! have sets.

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{BinaryOp, Callee, DefId, Expr, ExprKind, Literal, NameRef, Stmt};

use super::{for_each_expr_deep_in_stmts, for_each_stmt};
use crate::LintGroup;
use crate::pass::{Body, LintCx, LintMeta, LintPass};

pub struct MapAsSet {
    /// Target language version; the lint is silent below 4.
    pub version: u8,
}

static META: LintMeta = LintMeta {
    name: "map-as-set",
    code: codes::MAP_AS_SET,
    group: LintGroup::Nursery,
    description: "map whose values are all `true` — a set stores the keys without the dummy values",
};

/// Builtins that read a map the way a set would be read.
const SET_SHAPED_CALLS: &[&str] = &[
    "mapContainsKey",
    "mapRemove",
    "mapSize",
    "mapClear",
    "mapIsEmpty",
    "count",
];

impl LintPass for MapAsSet {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_body(&mut self, cx: &mut LintCx<'_, '_>, body: &Body<'_>) {
        if self.version < 4 {
            return;
        }
        let mut findings = Vec::new();
        for_each_stmt(body.stmts, &mut |s| {
            let Stmt::VarDecl(v) = s else { return };
            if v.is_global {
                return; // other bodies may use it map-style
            }
            let Some((all_true, nonempty)) = map_literal_init(v.init.as_ref()) else {
                return;
            };
            if !all_true {
                return;
            }
            if let Some(d) = judge(body, v.def, &v.name, v.span, nonempty) {
                findings.push(d);
            }
        });
        for d in findings {
            cx.emit(d);
        }
    }
}

/// `Some((values_all_true, nonempty))` when the declaration's
/// initializer is a map literal.
fn map_literal_init(init: Option<&Expr>) -> Option<(bool, bool)> {
    match init.map(|e| &e.kind) {
        Some(ExprKind::Map(pairs)) => {
            Some((pairs.iter().all(|(_, v)| is_true(v)), !pairs.is_empty()))
        }
        _ => None,
    }
}

fn is_true(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Literal(Literal::Bool(true)))
}

fn is_name(e: &Expr, def: DefId) -> bool {
    matches!(&e.kind, ExprKind::Name(NameRef::Local(d)) if *d == def)
}

/// Scan every use of `def` in the body; a diagnostic if all of them
/// are set-shaped (and there is evidence of set use).
fn judge(
    body: &Body<'_>,
    def: DefId,
    name: &str,
    span: leek_span::Span,
    init_nonempty: bool,
) -> Option<Diagnostic> {
    let mut total = 0usize; // every mention of the map
    let mut allowed = 0usize; // mentions in set-shaped positions
    let mut true_writes = 0usize;
    let mut bad = false;
    for_each_expr_deep_in_stmts(body.stmts, &mut |e| {
        if is_name(e, def) {
            total += 1;
        }
        match &e.kind {
            // `m[k]` — as a truthiness read this is the map-as-set
            // membership idiom; as an assignment target the parent
            // `Binary` below decides whether the write is `= true`.
            ExprKind::Index(base, _) if is_name(base, def) => allowed += 1,
            // `k in m` / `k not in m`.
            ExprKind::Binary(BinaryOp::In | BinaryOp::NotIn, _, hay) if is_name(hay, def) => {
                allowed += 1;
            }
            // Writes through `m[k]`: only literal `true` keeps the
            // set reading; anything else is a real map.
            ExprKind::Binary(op, lhs, rhs) if op.is_assignment() => {
                if let ExprKind::Index(base, _) = &lhs.kind
                    && is_name(base, def)
                {
                    if *op == BinaryOp::Assign && is_true(rhs) {
                        true_writes += 1;
                    } else {
                        bad = true;
                    }
                }
            }
            // `mapContainsKey(m, k)`, `mapRemove(m, k)`, sizes.
            ExprKind::Call(call) => {
                if let Callee::Function(NameRef::Builtin(n)) = &call.callee
                    && SET_SHAPED_CALLS.contains(&n.as_str())
                    && call.args.first().is_some_and(|a| is_name(a, def))
                {
                    allowed += 1;
                }
            }
            _ => {}
        }
    });
    // Any mention outside the allowed shapes (returned, reassigned,
    // passed to another call, iterated…) means we can't be sure.
    if bad || total != allowed || !(true_writes > 0 || init_nonempty) {
        return None;
    }
    Some(
        Diagnostic::new(
            codes::MAP_AS_SET,
            leek_diagnostics::Severity::Hint,
            span,
            format!("`{name}` only ever stores `true` — it is a set of keys"),
        )
        .with_note(format!(
            "declare it as a set: `var {name} = <>`, then `setPut({name}, key)` to add, `key in {name}` to test, `setRemove({name}, key)` to drop — same operations without the placeholder values"
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{lint_one, lint_one_v};
    use leek_syntax::Version;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(MapAsSet { version: 4 }, src)
    }

    #[test]
    fn flags_true_only_accumulator() {
        let d = run(
            "function f(cells) {\n  var seen = [:]\n  for (var c in cells) {\n    seen[c] = true\n  }\n  return mapSize(seen)\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].notes[0].contains("setPut(seen"), "{d:?}");
    }

    #[test]
    fn flags_all_true_literal_with_membership_reads() {
        let d = run(
            "function f(x) {\n  var melee = [1: true, 5: true]\n  if (x in melee) { return 1 }\n  return 0\n}\n",
        );
        assert_eq!(d.len(), 1, "got {d:?}");
    }

    #[test]
    fn ignores_map_storing_real_values() {
        let d = run(
            "function f(cells) {\n  var dist = [:]\n  for (var c in cells) {\n    dist[c] = c * 2\n  }\n  return mapSize(dist)\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_map_that_escapes() {
        // Passed to a non-map call — could be used map-style there.
        let d = run(
            "function g(m) { return m }\nfunction f(cells) {\n  var seen = [:]\n  for (var c in cells) {\n    seen[c] = true\n  }\n  return g(seen)\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_mixed_value_literal() {
        let d = run(
            "function f(x) {\n  var flags = [1: true, 5: false]\n  if (x in flags) { return 1 }\n  return 0\n}\n",
        );
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_empty_map_never_written() {
        // No `= true` writes and no seeded keys — nothing says "set".
        let d = run("function f(x) {\n  var m = [:]\n  return x in m\n}\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn silent_below_v4() {
        let d = lint_one_v(
            MapAsSet { version: 3 },
            "function f(cells) {\n  var seen = [:]\n  for (var c in cells) {\n    seen[c] = true\n  }\n  return mapSize(seen)\n}\n",
            Version::V3,
        );
        assert!(d.is_empty(), "got {d:?}");
    }
}
