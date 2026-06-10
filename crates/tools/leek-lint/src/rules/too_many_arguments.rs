//! L0025 `TooManyArguments` (pedantic) — flag a function / method /
//! constructor with more than [`MAX_PARAMS`] parameters.
//!
//! Long parameter lists are hard to call correctly (which `7` was the
//! damage again?). Grouping related values in a map, object, or class
//! usually reads better. Inspired by clippy's `too_many_arguments`.

use leek_diagnostics::{Diagnostic, codes};

use crate::LintGroup;
use crate::pass::{Body, BodyKind, LintCx, LintMeta, LintPass};

pub struct TooManyArguments;

/// Threshold above which the lint fires (clippy uses 7; Leekscript
/// AIs pass smaller bundles around, so be a little stricter).
const MAX_PARAMS: usize = 5;

static META: LintMeta = LintMeta {
    name: "too-many-arguments",
    code: codes::TOO_MANY_ARGUMENTS,
    group: LintGroup::Pedantic,
    description: "function with a very long parameter list — group related values instead",
};

impl LintPass for TooManyArguments {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    fn check_body(&mut self, cx: &mut LintCx<'_, '_>, body: &Body<'_>) {
        // A lambda's arity is dictated by its caller; only named
        // callables are worth flagging.
        if body.kind == BodyKind::Lambda || body.params.len() <= MAX_PARAMS {
            return;
        }
        let name = body.name.unwrap_or("<anonymous>");
        let n = body.params.len();
        cx.emit(
            Diagnostic::new(
                codes::TOO_MANY_ARGUMENTS,
                leek_diagnostics::Severity::Hint,
                body.span,
                format!("`{name}` takes {n} parameters — more than {MAX_PARAMS} is hard to call correctly"),
            )
            .with_note(
                "group related parameters into a map, array, or class so call sites stay readable",
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(TooManyArguments, src)
    }

    #[test]
    fn flags_six_params() {
        let d = run("function f(a, b, c, d, e, g) { return a }\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("6 parameters"), "{d:?}");
    }

    #[test]
    fn ignores_five_params() {
        let d = run("function f(a, b, c, d, e) { return a }\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn ignores_wide_lambda() {
        let d = run("var g = (a, b, c, d, e, f) -> a\nreturn g(1, 2, 3, 4, 5, 6)\n");
        assert!(d.is_empty(), "got {d:?}");
    }
}
