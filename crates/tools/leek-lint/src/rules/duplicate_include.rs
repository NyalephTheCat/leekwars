//! L0020 `DuplicateInclude` — flag a file that `include`s the same
//! script more than once. The second include is a no-op (the first
//! already pulled the file in), so it ships an autofix to remove it.

use std::collections::HashSet;

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, codes, diag};
use leek_hir::Stmt;

use crate::registry::declare_lint;
use crate::pass::{LintCx, LintMeta, LintPass};

#[derive(Default)]
pub struct DuplicateInclude;

declare_lint!(
    DuplicateInclude,
    "duplicate-include",
    codes::DUPLICATE_INCLUDE,
    Style,
    "second `include` of the same file — a no-op"
);

impl LintPass for DuplicateInclude {
    fn meta(&self) -> &'static LintMeta {
        &META
    }

    // Includes only count at the top level of `main`, so this drives
    // its own (one-level) walk instead of hooking `check_stmt`.
    fn check_file(&mut self, cx: &mut LintCx<'_, '_>) {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut dups = Vec::new();
        for stmt in &cx.file.main {
            if let Stmt::Include(inc) = stmt
                && !seen.insert(inc.path.as_str())
            {
                dups.push(diagnostic(&inc.path, inc.span));
            }
        }
        for d in dups {
            cx.emit(d);
        }
    }
}

fn diagnostic(path: &str, span: leek_span::Span) -> Diagnostic {
    diag!(
        codes::DUPLICATE_INCLUDE,
        span,
        "`{path}` is already included above"
    )
    .with_note("a second `include` of the same file does nothing — remove it")
    .with_suggestion(
        // Deleting the statement may leave a blank line behind.
        Suggestion::remove("remove the duplicate include", span)
            .with_applicability(Applicability::MaybeIncorrect),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::lint_one;

    fn run(src: &str) -> Vec<Diagnostic> {
        lint_one(DuplicateInclude, src)
    }

    #[test]
    fn flags_repeated_include() {
        let d = run("include(\"util\")\ninclude(\"util\")\nreturn 0\n");
        assert_eq!(d.len(), 1, "got {d:?}");
        assert!(d[0].message.contains("util"));
    }

    #[test]
    fn ignores_distinct_includes() {
        let d = run("include(\"util\")\ninclude(\"helpers\")\nreturn 0\n");
        assert!(d.is_empty(), "got {d:?}");
    }

    #[test]
    fn flags_only_the_extra_copies() {
        let d = run("include(\"a\")\ninclude(\"a\")\ninclude(\"a\")\nreturn 0\n");
        assert_eq!(d.len(), 2, "two duplicates of `a`: {d:?}");
    }

    /// The suggestion this rule emits must apply cleanly: the
    /// rewritten source still parses and the finding is gone.
    #[test]
    fn suggestions_are_applicable() {
        crate::testing::assert_suggestions_fix(
            || DuplicateInclude,
            "include(\"util\")\ninclude(\"util\")\nreturn 0\n",
        );
    }
}
