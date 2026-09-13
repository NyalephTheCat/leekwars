//! `switch` lowering, ported from upstream `SwitchBlock.writeJavaCode`.
//!
//! Every switch lowers to one braced block with numbered temporaries:
//!
//! ```java
//! {
//! Object __sw_0 = <discriminant>;
//! int __si_0 = -1;
//! if (ops(eq(__sw_0, 1l) || eq(__sw_0, 2l), 2)) __si_0 = 0;
//! else if (ops(eq(__sw_0, 3l), 1)) __si_0 = 1;
//! switch (__si_0) {
//! case 0: {
//! ops(1);<body>
//! }
//! default: {
//! ops(1);<body>
//! }
//! }
//! }
//! ```
//!
//! The outer braces and the `<n>` suffix keep sequential and nested switches
//! from redeclaring a Java local, and the braced arms give each arm's locals
//! their own scope while Java fall-through between arms is preserved.
//!
//! Clean mode (`native_switch`) replaces the `eq` chain with an O(1) guarded
//! dispatch when every label is a distinct `int`-range integer constant (see
//! [`int_dispatch`]). The chain stays as the fallback for any subject that is
//! not a `Long`, since only `eq` implements loose equality across types.

use std::collections::HashSet;

use leek_hir::{Expr, ExprKind, Literal, Stmt, SwitchStmt, UnaryOp};
use leek_types::Type;

/// One upstream `SwitchCase`: consecutive labels with no statement between
/// them share the body that follows (`case 1: case 2: …`).
struct SwitchCase<'h> {
    labels: Vec<&'h Expr>,
    /// A `default:` anywhere in the label run makes the whole case the
    /// default; its other labels are then never tested (as upstream).
    is_default: bool,
    body: &'h [Stmt],
}

/// Regroup HIR arms into upstream cases. The parser gives every label its own
/// arm, so an empty-bodied arm is a label that shares the next arm's body. The
/// grouping matters for op parity: upstream charges one `ops(…, n)` per case
/// test and one `ops(1)` per case body entered.
fn group_cases(sw: &SwitchStmt) -> Vec<SwitchCase<'_>> {
    let mut cases = Vec::new();
    let mut labels = Vec::new();
    let mut is_default = false;
    let last = sw.arms.len().saturating_sub(1);
    for (i, arm) in sw.arms.iter().enumerate() {
        match &arm.case {
            Some(label) => labels.push(label),
            None => is_default = true,
        }
        if !arm.body.is_empty() || i == last {
            cases.push(SwitchCase {
                labels: std::mem::take(&mut labels),
                is_default,
                body: &arm.body,
            });
            is_default = false;
        }
    }
    cases
}

/// Value of an integer-constant label (`1`, `-1`) that fits a Java `int`.
fn int_label(e: &Expr) -> Option<i32> {
    match &e.kind {
        ExprKind::Literal(Literal::Int(n)) => i32::try_from(*n).ok(),
        ExprKind::Unary(UnaryOp::Neg, inner) => match &inner.kind {
            ExprKind::Literal(Literal::Int(n)) => {
                n.checked_neg().and_then(|n| i32::try_from(n).ok())
            }
            _ => None,
        },
        _ => None,
    }
}

/// Java `int` labels for each case (empty for the default), or `None` when a
/// native Java `switch` cannot reproduce `eq` semantics:
///
/// - a label is not an integer constant, or is outside `int` range (a Java
///   `case` label must be an `int` constant, and truncating would match the
///   wrong arm);
/// - a label repeats (the chain keeps the first match; javac rejects it);
/// - there are two defaults (two `default:` labels do not compile);
/// - the subject is statically a string, real, boolean or null, where loose
///   `eq` (`eq(1.0, 1)` is true) is required and a `Long` guard is dead.
///
/// Mirrors upstream `SwitchBlock.buildConstantDispatch` for integer labels.
fn int_dispatch(sw: &SwitchStmt, cases: &[SwitchCase<'_>]) -> Option<Vec<Vec<i32>>> {
    if cases.iter().filter(|c| c.is_default).count() > 1
        || !cases.iter().any(|c| !c.is_default && !c.labels.is_empty())
        || matches!(
            sw.discriminant.ty,
            Type::String | Type::Real | Type::Boolean | Type::Null
        )
    {
        return None;
    }
    let mut seen = HashSet::new();
    cases
        .iter()
        .map(|case| {
            if case.is_default {
                return Some(Vec::new());
            }
            case.labels
                .iter()
                .map(|label| int_label(label).filter(|v| seen.insert(*v)))
                .collect()
        })
        .collect()
}

impl super::Emitter<'_> {
    pub(crate) fn emit_switch(&mut self, sw: &SwitchStmt) {
        let id = self.switch_counter.get();
        self.switch_counter.set(id + 1);
        let sw_var = format!("__sw_{id}");
        let si_var = format!("__si_{id}");
        let cases = group_cases(sw);

        self.open_switch_block("{");
        let disc = self.expr_to_string(&sw.discriminant);
        let line = self.line_of(sw.span);
        self.writer
            .add_line_at(&format!("Object {sw_var} = {disc};"), line);
        if self.opts.emit_ops {
            let cost = self.emit_cost(&sw.discriminant);
            if cost > 0 {
                self.writer.add_code(&format!("ops({cost});"));
            }
        }
        self.writer.add_line(&format!("int {si_var} = -1;"));
        let dispatch = if self.opts.native_switch {
            int_dispatch(sw, &cases)
        } else {
            None
        };
        match dispatch {
            Some(labels) => self.write_int_dispatch(&cases, &labels, id, &sw_var, &si_var),
            None => self.write_index_chain(&cases, &sw_var, &si_var),
        }
        self.write_switch_bodies(&cases, &si_var);
        self.close_switch_block("}");
    }

    /// Upstream `writeIndexChain`: an `if` / `else if` chain storing the index
    /// of the first matching case. Each test costs 1 op per label plus the
    /// labels' own expression cost.
    fn write_index_chain(&mut self, cases: &[SwitchCase<'_>], sw_var: &str, si_var: &str) {
        let mut keyword = "if";
        for (index, case) in cases.iter().enumerate() {
            if case.is_default {
                continue;
            }
            let test = case
                .labels
                .iter()
                .map(|label| format!("eq({sw_var}, {})", self.expr_to_string(label)))
                .collect::<Vec<_>>()
                .join(" || ");
            let test = if self.opts.emit_ops {
                let ops: u32 = case
                    .labels
                    .iter()
                    .map(|label| 1 + self.emit_cost(label))
                    .sum();
                format!("ops({test}, {ops})")
            } else {
                test
            };
            self.writer
                .add_line(&format!("{keyword} ({test}) {si_var} = {index};"));
            keyword = "else if";
        }
    }

    /// Upstream `writeGuardedDispatch`, integer labels only: a `Long` subject
    /// that fits an `int` picks its case index through a native Java `switch`;
    /// a `Long` outside `int` range matches no label (index stays -1), and any
    /// other subject falls back to the loose-equality chain.
    fn write_int_dispatch(
        &mut self,
        cases: &[SwitchCase<'_>],
        labels: &[Vec<i32>],
        id: u32,
        sw_var: &str,
        si_var: &str,
    ) {
        let sv_var = format!("__swv_{id}");
        let sk_var = format!("__swk_{id}");
        self.open_switch_block(&format!("if ({sw_var} instanceof Long {sv_var}) {{"));
        if self.opts.emit_ops {
            self.writer.add_code("ops(1);");
        }
        self.writer
            .add_line(&format!("int {sk_var} = (int) (long) {sv_var};"));
        self.writer
            .add_line(&format!("if ({sk_var} == (long) {sv_var})"));
        self.open_switch_block(&format!("switch ({sk_var}) {{"));
        for (index, case_labels) in labels.iter().enumerate() {
            if case_labels.is_empty() {
                continue;
            }
            for label in case_labels {
                self.writer.add_line(&format!("case {label}:"));
            }
            self.writer.add_line(&format!("{si_var} = {index}; break;"));
        }
        self.close_switch_block("}");
        self.close_switch_block("} else {");
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        self.write_index_chain(cases, sw_var, si_var);
        self.close_switch_block("}");
    }

    /// Upstream `writeBodies`: a Java `switch` on the computed index with one
    /// braced block per case, so fall-through and the default's position are
    /// preserved. The Leek `default` arm stays a Java `default:` label: when
    /// every arm returns, javac then sees the switch as never completing
    /// normally, so a function ending in it needs no trailing return.
    fn write_switch_bodies(&mut self, cases: &[SwitchCase<'_>], si_var: &str) {
        self.open_switch_block(&format!("switch ({si_var}) {{"));
        for (index, case) in cases.iter().enumerate() {
            if case.is_default {
                self.open_switch_block("default: {");
            } else {
                self.open_switch_block(&format!("case {index}: {{"));
            }
            if self.opts.emit_ops {
                self.writer.add_code("ops(1);");
            }
            self.emit_stmts(case.body);
            self.close_switch_block("}");
        }
        self.close_switch_block("}");
    }

    /// Emit an opening line; clean mode indents what follows (exact mode stays
    /// flush-left like the reference).
    fn open_switch_block(&mut self, line: &str) {
        self.writer.add_line(line);
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
    }

    fn close_switch_block(&mut self, line: &str) {
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        self.writer.add_line(line);
    }
}
