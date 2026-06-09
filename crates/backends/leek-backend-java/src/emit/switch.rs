use leek_hir::{ExprKind, Literal, SwitchStmt};

impl super::Emitter<'_> {
    pub(crate) fn emit_switch(&mut self, sw: &SwitchStmt) {
        // Native Java switch when every case is a compile-time
        // constant of a switchable type (int or string). Otherwise
        // lower to an if-else chain, which mirrors the reference's
        // exact-mode behavior.
        // A native Java `switch` only works when every case is an INTEGER
        // literal — the discriminant is narrowed to `int`, and Java case labels
        // must be `int` constants. A string / real / bool case (or a non-literal)
        // falls through to the `eq`-based if-else chain.
        if self.opts.native_switch
            && sw.arms.iter().all(|a| {
                a.case
                    .as_ref()
                    .is_none_or(|c| matches!(&c.kind, ExprKind::Literal(Literal::Int(_))))
            })
        {
            let disc = self.expr_to_string(&sw.discriminant);
            self.writer
                .add_line(&format!("switch ((int) ((Number) {disc}).longValue()) {{"));
            self.writer.push_indent();
            for arm in &sw.arms {
                match &arm.case {
                    Some(c) => {
                        // Bare `int` label (no `l` suffix): `case 1:` not `case 1l:`.
                        let lit = match &c.kind {
                            ExprKind::Literal(Literal::Int(n)) => n.to_string(),
                            _ => self.expr_to_string(c),
                        };
                        self.writer.add_line(&format!("case {lit}: {{"));
                    }
                    None => self.writer.add_line("default: {"),
                }
                self.writer.push_indent();
                self.emit_stmts(&arm.body);
                self.writer.pop_indent();
                self.writer.add_line("}");
            }
            self.writer.pop_indent();
            self.writer.add_line("}");
            return;
        }
        // Lowered if-else chain mirroring the Java reference.
        let disc = self.expr_to_string(&sw.discriminant);
        self.writer.add_line(&format!("Object __scrut = {disc};"));
        self.writer.add_line("int __idx = -1;");
        for (i, arm) in sw.arms.iter().enumerate() {
            if let Some(c) = &arm.case {
                let case = self.expr_to_string(c);
                // `eq` is the AI instance method (loose equality), matching
                // upstream's switch lowering — there is no `LeekOperations.eq`.
                self.writer.add_line(&format!(
                    "if (__idx == -1 && eq(__scrut, {case})) __idx = {i};"
                ));
            }
        }
        // Find default arm (if any) and assign its index as a fallback.
        if let Some((idx, _)) = sw.arms.iter().enumerate().find(|(_, a)| a.case.is_none()) {
            self.writer
                .add_line(&format!("if (__idx == -1) __idx = {idx};"));
        }
        self.writer.add_line("switch (__idx) {");
        self.writer.push_indent();
        for (i, arm) in sw.arms.iter().enumerate() {
            // Emit the Leek `default` arm as a Java `default:` label (not a
            // numbered `case`): when every arm returns, a `switch` *with a
            // default* is seen by javac as never completing normally, so a
            // function ending in such a switch needs no trailing return (and
            // emitting one would be unreachable). A numbered-only switch can
            // "fall through" in javac's eyes → "missing return".
            if arm.case.is_none() {
                self.writer.add_line("default:");
            } else {
                self.writer.add_line(&format!("case {i}:"));
            }
            self.writer.push_indent();
            self.emit_stmts(&arm.body);
            self.writer.pop_indent();
        }
        self.writer.pop_indent();
        self.writer.add_line("}");
    }
}
