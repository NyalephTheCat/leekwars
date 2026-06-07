//! L0020 `DuplicateInclude` — flag a file that `include`s the same
//! script more than once. The second include is a no-op (the first
//! already pulled the file in), so it ships an autofix to remove it.

use std::collections::HashSet;

use leek_diagnostics::{Applicability, Diagnostic, Suggestion, TextEdit, codes};
use leek_hir::{HirFile, Stmt};

use crate::LintRule;

pub struct DuplicateInclude;

impl LintRule for DuplicateInclude {
    fn name(&self) -> &'static str {
        "duplicate-include"
    }

    fn code(&self) -> leek_diagnostics::Code {
        codes::DUPLICATE_INCLUDE
    }

    fn check(&self, file: &HirFile, out: &mut Vec<Diagnostic>) {
        let mut seen: HashSet<&str> = HashSet::new();
        for stmt in &file.main {
            if let Stmt::Include(inc) = stmt
                && !seen.insert(inc.path.as_str())
            {
                out.push(diagnostic(&inc.path, inc.span));
            }
        }
    }
}

fn diagnostic(path: &str, span: leek_span::Span) -> Diagnostic {
    Diagnostic::warning(
        codes::DUPLICATE_INCLUDE,
        span,
        format!("`{path}` is already included above"),
    )
    .with_note("a second `include` of the same file does nothing — remove it")
    .with_suggestion(Suggestion {
        message: "remove the duplicate include".to_string(),
        edits: vec![TextEdit {
            span,
            replacement: String::new(),
        }],
        // Deleting the statement may leave a blank line behind.
        applicability: Applicability::MaybeIncorrect,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use leek_parser::ast::{AstNode, SourceFile};
    use leek_parser::parse;
    use leek_span::SourceId;
    use leek_syntax::{SyntaxNode, Version};

    fn run(src: &str) -> Vec<Diagnostic> {
        let source = SourceId::new(1).unwrap();
        let parsed = parse(src, source, Version::V4);
        let ast = SourceFile::cast(SyntaxNode::new_root(parsed.green)).unwrap();
        let (hir, _) = leek_hir::lower_file(&ast, source);
        let mut out = Vec::new();
        DuplicateInclude.check(&hir, &mut out);
        out
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
}
