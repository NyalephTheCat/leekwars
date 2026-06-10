//! Scope stack primitives and diagnostic helpers.

use leek_diagnostics::{Code, Diagnostic, Severity};
use leek_span::Span;
use leek_syntax::SyntaxToken;

use super::{Checker, Scope};
use crate::ty::Type;

impl Checker {
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Scope::empty());
    }

    pub(crate) fn push_function(&mut self) {
        self.scopes.push(Scope::function());
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn declare(&mut self, name: &str, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_string(), ty);
        }
    }

    pub(crate) fn lookup(&self, name: &str) -> Option<&Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.locals.get(name) {
                return Some(ty);
            }
            if scope.is_function_boundary {
                return None;
            }
        }
        None
    }

    pub(crate) fn span_of(&self, tok: &SyntaxToken) -> Span {
        leek_syntax::token_span(tok, self.source)
    }

    pub(crate) fn err(&mut self, code: Code, span: Span, msg: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::new(code, Severity::Error, span, msg));
    }

    pub(crate) fn warn(&mut self, code: Code, span: Span, msg: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::new(code, Severity::Warning, span, msg));
    }
}
