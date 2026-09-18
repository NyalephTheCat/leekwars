//! Scope stack primitives and diagnostic helpers.

use leek_diagnostics::{Code, Diagnostic, Severity};
use leek_span::Span;
use leek_syntax::SyntaxToken;

use super::{Checker, Scope, ScopeKind};
use crate::ty::Type;

impl Checker {
    /// Push a transparent scope: blocks, loop headers, narrowing arms.
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Scope::new(ScopeKind::Block));
    }

    /// Push an opaque scope: a named function, method or constructor
    /// body. Not for lambdas — see [`Self::push_lambda`].
    pub(crate) fn push_function(&mut self) {
        self.scopes.push(Scope::new(ScopeKind::Function));
    }

    /// Push a lambda body's scope. Transparent, so the body's reads of
    /// enclosing locals resolve as captures.
    pub(crate) fn push_lambda(&mut self) {
        self.scopes.push(Scope::new(ScopeKind::Lambda));
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(crate) fn declare(&mut self, name: &str, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_string(), ty);
        }
    }

    /// Declare a `global`. Globals are program-wide whatever block or
    /// function body they textually sit in, so the binding is recorded
    /// on the file scope and flagged there as the one kind of name a
    /// function body may reach across its boundary.
    pub(crate) fn declare_global(&mut self, name: &str, ty: Type) {
        if let Some(file) = self.scopes.first_mut() {
            file.locals.insert(name.to_string(), ty);
            file.globals.insert(name.to_string());
        }
    }

    /// Record a declaration's type, routing a `global` to the file
    /// scope and anything else to the innermost scope.
    pub(crate) fn bind(&mut self, is_global: bool, name: &str, ty: Type) {
        if is_global {
            self.declare_global(name, ty);
        } else {
            self.declare(name, ty);
        }
    }

    /// Mark `name` as a `global` on the file scope without committing a
    /// type for it. Used for the plain `global x` / `global x = expr`
    /// forms, whose type the checker deliberately leaves dynamic: the
    /// name must still be reachable from inside a function body, even
    /// though no entry lands in the file scope's `locals`.
    pub(crate) fn note_global(&mut self, name: &str) {
        if let Some(file) = self.scopes.first_mut() {
            file.globals.insert(name.to_string());
        }
    }

    /// Resolve `name` through the scope stack, innermost first.
    ///
    /// Blocks and lambda bodies are walked straight through, so a
    /// closure captures the enclosing function's typed locals (#192).
    /// A named function, method or constructor body is a hard boundary:
    /// it cannot see another function's locals *or* the main block's,
    /// which LeekScript functions genuinely cannot access — only
    /// `global`s cross, including ones an include supplied.
    pub(crate) fn lookup(&self, name: &str) -> Option<&Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.locals.get(name) {
                return Some(ty);
            }
            match scope.kind {
                ScopeKind::Block | ScopeKind::Lambda => {}
                ScopeKind::Function => return self.lookup_global(name),
                // Outermost already; the loop would end here anyway.
                ScopeKind::File => return None,
            }
        }
        None
    }

    /// The recorded type of a file-scope binding declared `global`.
    fn lookup_global(&self, name: &str) -> Option<&Type> {
        let file = self.scopes.first()?;
        if file.globals.contains(name) {
            file.locals.get(name)
        } else {
            None
        }
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
