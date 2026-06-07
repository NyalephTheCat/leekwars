//! Scope stack, symbol kinds, and the small primitives that
//! operate on them: `push_scope`, `pop_scope`, `declare`, `lookup`,
//! plus the `name_in_outer_scope` shadowing check and span/error
//! helpers.
//!
//! The builtin "scope" is **not** stored as a dynamic HashMap any
//! more — it lives behind [`BUILTIN_SCOPE`] as a `LazyLock<HashSet>`
//! shared across every [`Resolver`] instance. Lookups walk the
//! dynamic scope stack first; misses fall through to the static
//! set. Construction used to do ~330 `String::from` allocations per
//! `Resolver::new`; this skips them entirely.

use std::collections::HashMap;
use std::sync::LazyLock;

use leek_diagnostics::{Code, Diagnostic, Severity};
use leek_span::Span;
use leek_syntax::{SyntaxNode, SyntaxToken, Version};

use crate::Resolver;
use crate::builtins;
use crate::codes;
use crate::index::{ResolvedRef, Symbol, SymbolId};

/// `FnMeta` for every builtin in [`builtins::BUILTIN_FNS`]. Same
/// `LazyLock` trick — built once on first access.
pub(crate) static BUILTIN_FN_META: LazyLock<HashMap<&'static str, FnMeta>> = LazyLock::new(|| {
    let mut m = HashMap::with_capacity(builtins::BUILTIN_FNS.len());
    for b in builtins::BUILTIN_FNS {
        m.insert(
            b.name,
            FnMeta {
                min_args: b.min_args,
                max_args: b.max_args,
                min_version: b.min_version,
            },
        );
    }
    m
});

/// Where in the lexical-binding taxonomy a name fits. Tracked so
/// later checks can discriminate (`f += 1` on a `Function` is a
/// `CANNOT_REDEFINE_FUNCTION`, on a `Local` it's fine).
#[cfg_attr(feature = "salsa", derive(salsa::Update))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)] // some variants unused until later slices
pub enum SymbolKind {
    Global,
    Function,
    Class,
    Param,
    Local,
    /// Class field, accessible via `this.x`.
    Field,
    /// Stdlib / game-side runtime function.
    Builtin,
}

/// Static metadata for a callable name — what arities it accepts
/// and at what minimum language version.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FnMeta {
    pub(crate) min_args: u8,
    pub(crate) max_args: u8,
    /// Minimum language version a call to this name is allowed at.
    pub(crate) min_version: u8,
}

impl Resolver {
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
        self.scope_boundaries.push(false);
        self.var_class_types.push(HashMap::new());
        self.var_class_types_typed.push(HashMap::new());
    }
    pub(crate) fn push_function_scope(&mut self) {
        self.scopes.push(HashMap::new());
        self.scope_boundaries.push(true);
        self.var_class_types.push(HashMap::new());
        self.var_class_types_typed.push(HashMap::new());
    }
    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
        self.scope_boundaries.pop();
        self.var_class_types.pop();
        self.var_class_types_typed.pop();
    }

    /// Record that local `name` holds an instance of class `cls` (from
    /// `... name = new cls(...)`), in the innermost scope. `typed` marks an
    /// explicit class annotation (`cls name = …`), which additionally enables
    /// privacy checks. The binding is dropped when its scope is popped, so it
    /// can't leak into a sibling/outer scope.
    pub(crate) fn set_var_class(&mut self, name: String, cls: String, typed: bool) {
        if let Some(scope) = self.var_class_types.last_mut() {
            scope.insert(name.clone(), cls.clone());
        }
        if typed && let Some(scope) = self.var_class_types_typed.last_mut() {
            scope.insert(name, cls);
        }
    }

    /// The class an in-scope local was constructed with, if tracked — walks
    /// scopes outward so an inner binding shadows an outer one.
    pub(crate) fn var_class_of(&self, name: &str) -> Option<String> {
        self.var_class_types
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    /// As [`var_class_of`](Self::var_class_of) but only for bindings with an
    /// explicit class annotation (privacy checks).
    pub(crate) fn var_class_typed_of(&self, name: &str) -> Option<String> {
        self.var_class_types_typed
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    /// Declare `name` in the innermost scope, tied to the identifier
    /// token at the declaration site. Returns `(id, redeclared)` —
    /// `redeclared` is `true` when `name` was already declared in
    /// the same scope as a non-builtin (callers emit
    /// `REDECLARED_SYMBOL`). The returned `id` is always fresh.
    pub(crate) fn declare(&mut self, ident: &SyntaxToken, kind: SymbolKind) -> (SymbolId, bool) {
        let name = ident.text();
        let def_span = self.span_of(ident);
        self.declare_with_span(name, kind, def_span, def_span)
    }

    /// Variant of [`declare`] for sites where the declaration's
    /// identifier and its full extent differ (e.g. function whose
    /// `def_span` is the name token but whose `full_span` covers the
    /// whole declaration).
    pub(crate) fn declare_with_span(
        &mut self,
        name: &str,
        kind: SymbolKind,
        def_span: Span,
        full_span: Span,
    ) -> (SymbolId, bool) {
        let id = SymbolId(u32::try_from(self.symbols.len()).expect("more than u32::MAX symbols"));
        self.symbols.push(Symbol {
            id,
            kind,
            name: name.to_string(),
            def_span,
            full_span,
            container: self.containers.last().copied(),
        });
        // The resolver always pushes a file scope before declaring, so this
        // should never be empty — but degrade gracefully (return the symbol
        // without binding it into a scope) rather than crash the LSP if some
        // error-recovery path ever declares before `push_scope`.
        let Some(scope) = self.scopes.last_mut() else {
            debug_assert!(false, "resolver should always have at least one scope");
            return (id, false);
        };
        let prior = scope.insert(name.to_string(), id);
        let redeclared = match prior {
            Some(prev_id) => !matches!(self.symbols[prev_id.0 as usize].kind, SymbolKind::Builtin),
            None => false,
        };
        (id, redeclared)
    }

    /// Declare a function parameter. Two parameters in the same list
    /// with the same name is a `DUPLICATED_ARGUMENT` error.
    pub(crate) fn declare_param(&mut self, ident: &SyntaxToken) {
        let (_, redecl) = self.declare(ident, SymbolKind::Param);
        if redecl {
            self.err(
                codes::DUPLICATED_ARGUMENT,
                self.span_of(ident),
                format!("duplicate parameter `{}`", ident.text()),
            );
        }
    }

    /// Look up a name walking outward through all dynamic scopes.
    /// Misses fall back to the static [`BUILTIN_SCOPE`] so user
    /// declarations always win over builtins (the `var search = …`
    /// pattern that combat AIs love).
    pub(crate) fn lookup(&self, name: &str) -> Option<SymbolKind> {
        self.lookup_id(name)
            .map(|id| self.symbols[id.0 as usize].kind)
            .or_else(|| {
                if crate::builtins::is_builtin_name(name)
                    || self.imported_library_symbols.contains(name)
                {
                    Some(SymbolKind::Builtin)
                } else {
                    None
                }
            })
    }

    /// Look up a name returning the resolved [`SymbolId`]. Builtins
    /// resolve to `None` since they don't have an in-table entry —
    /// callers that need to distinguish builtins should fall through
    /// to [`lookup`](Self::lookup) too.
    pub(crate) fn lookup_id(&self, name: &str) -> Option<SymbolId> {
        for scope in self.scopes.iter().rev() {
            if let Some(&id) = scope.get(name) {
                return Some(id);
            }
        }
        None
    }

    /// Record that the reference token at `ident` (offset / length
    /// derived from its text-range) resolved to a known symbol.
    /// Silently no-ops when the name isn't bound — the LSP just
    /// treats those positions as un-navigable.
    pub(crate) fn record_ref(&mut self, ident: &SyntaxToken) {
        let Some(target) = self.lookup_id(ident.text()) else {
            return;
        };
        let range = ident.text_range();
        self.references.push(ResolvedRef {
            name_offset: u32::from(range.start()),
            name_len: u32::from(range.end()) - u32::from(range.start()),
            target,
        });
    }

    /// Record a reference from `tok` to the symbol named `name` *only*
    /// when that name resolves to a user-declared class. Used for
    /// class names that appear outside ordinary expression position —
    /// type annotations (`Cat c`), `new Cat(...)`, and the `this`
    /// keyword — so hover / go-to-def reach the class declaration.
    pub(crate) fn record_class_ref(&mut self, tok: &SyntaxToken, name: &str) {
        let Some(id) = self.lookup_id(name) else {
            return;
        };
        if self.symbols[id.0 as usize].kind != SymbolKind::Class {
            return;
        }
        let range = tok.text_range();
        self.references.push(ResolvedRef {
            name_offset: u32::from(range.start()),
            name_len: u32::from(range.end()) - u32::from(range.start()),
            target: id,
        });
    }

    /// True if `name` is bound by an outer dynamic scope up to and
    /// not crossing the nearest function boundary. Builtins never
    /// count as shadowing (the static set is checked separately).
    pub(crate) fn name_in_outer_scope(&self, name: &str) -> bool {
        let n = self.scopes.len();
        if n < 2 {
            return false;
        }
        let mut idx = n - 2;
        loop {
            if self.scopes[idx].contains_key(name) {
                return true;
            }
            if self.scope_boundaries[idx] {
                return false;
            }
            if idx == 0 {
                return false;
            }
            idx -= 1;
        }
    }

    /// Check whether `name` is the case-different form of a v3+
    /// keyword (`True`, `FALSE`, `Not`, …). Used to flag obvious
    /// typos that case-insensitivity hid at v1/v2.
    pub(crate) fn looks_like_case_typo(&self, name: &str) -> bool {
        if self.version < Version::V3 {
            return false;
        }
        // Words that Leekscript still rejects on capitalization
        // mismatch at v3+.
        const CASE_SENSITIVE_KEYWORDS: &[&str] = &[
            "true",
            "false",
            "null",
            "not",
            "and",
            "or",
            "if",
            "else",
            "while",
            "for",
            "do",
            "in",
            "break",
            "continue",
            "return",
            "var",
            "global",
            "include",
            "extends",
            "new",
            "this",
            "super",
            "static",
            "private",
            "public",
            "protected",
            "constructor",
        ];
        // Edge case: upstream accepts the TitleCase `Null` as an
        // alias for the `null` literal even at v3+. So we flag
        // `NULL`/`nULL`/etc but not `Null` itself.
        if name == "Null" {
            return false;
        }
        let lower = name.to_ascii_lowercase();
        lower != name && CASE_SENSITIVE_KEYWORDS.contains(&lower.as_str())
    }

    // ---- span / error helpers ----

    pub(crate) fn span_of(&self, tok: &SyntaxToken) -> Span {
        leek_syntax::token_span(tok, self.source)
    }

    pub(crate) fn node_span(&self, node: &SyntaxNode) -> Span {
        let range = node.text_range();
        Span::new(
            self.source,
            u32::from(range.start()),
            u32::from(range.end()),
        )
    }

    pub(crate) fn err(&mut self, code: Code, span: Span, msg: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::new(code, Severity::Error, span, msg));
    }
}
