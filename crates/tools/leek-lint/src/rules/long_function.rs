//! L0026 `LongFunction` (pedantic) — flag a function / method body
//! containing more than [`MAX_STMTS`] statements (counted recursively,
//! so a deeply structured body weighs the same as a flat one).
//!
//! Long bodies hide structure: a fight loop that also scores cells,
//! picks weapons, and logs debug output is four functions in a trench
//! coat. Inspired by clippy's `too_many_lines`, but counted in
//! statements — the HIR carries no line table, and statements track
//! "things this function does" more honestly than blank-line-padded
//! line counts anyway.

use leek_diagnostics::{Diagnostic, codes};

use super::for_each_stmt;
use crate::LintGroup;
use crate::pass::{Body, BodyKind, LintCx, LintMeta, LintPass};

pub struct LongFunction;

/// Statement-count threshold. Roughly the 75-line mark for typical
/// Leekscript (one statement per line plus braces and blanks).
const MAX_STMTS: usize = 60;

static META: LintMeta = LintMeta {
    name: "long-function",
    code: codes::LONG_FUNCTION,
    group: LintGroup::Pedantic,
    description: "function body with very many statements — split it into helpers",
};

impl LintPass for LongFunction {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_body(&mut self, cx: &mut LintCx<'_, '_>, body: &Body<'_>) {
        // `main` is the script's entry point and often *is* the whole
        // program in small AIs; lambdas are anonymous one-shots.
        if matches!(body.kind, BodyKind::Main | BodyKind::Lambda) {
            return;
        }
        let mut count = 0usize;
        for_each_stmt(body.stmts, &mut |_| count += 1);
        if count <= MAX_STMTS {
            return;
        }
        let name = body.name.unwrap_or("<anonymous>");
        cx.emit(
            Diagnostic::new(
                codes::LONG_FUNCTION,
                leek_diagnostics::Severity::Hint,
                body.span,
                format!("`{name}` contains {count} statements (more than {MAX_STMTS})"),
            )
            .with_note(
                "long functions are hard to follow — extract self-contained steps into helper functions",
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(LongFunction, src)
    }

    fn function_with_stmts(n: usize) -> String {
        use std::fmt::Write;
        let mut src = String::from("function f() {\n");
        for i in 0..n {
            writeln!(src, "  var x{i} = {i}\n  debug(x{i})").unwrap();
        }
        src.push_str("}\n");
        src
    }

    #[test]
    fn flags_oversized_function() {
        // 35 pairs = 70 statements > 60.
        let d = run(&function_with_stmts(35));
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("70 statements"), "{d:?}");
    }

    #[test]
    fn ignores_reasonable_function() {
        let d = run(&function_with_stmts(10));
        assert!(d.is_empty(), "got {d:?}");
    }
}
