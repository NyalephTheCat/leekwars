//! Name resolution for Leekscript.
//!
//! Walks a parsed [`SourceFile`] AST, builds a stack of lexical
//! scopes, and emits diagnostics for unresolved names, duplicate
//! declarations, privacy violations, etc.
//!
//! Scope:
//! - **File** — top-level globals, functions, classes (forward-declared).
//! - **Function** — params + locals.
//! - **Block** — nested locals.
//! - **Foreach/For** — loop bindings.
//!
//! Builtins: see [`builtins`] — a curated list of upstream stdlib free
//! functions (`length`, `push`, `sin`, …) plus game-side runtime
//! functions.
//!
//! Module layout:
//! - [`scope`] — scope stack, [`SymbolKind`](scope::SymbolKind),
//!   span/error helpers
//! - [`classes`] — class declaration sweep + inheritance walk
//! - [`statements`] — statement-level resolution + function bodies
//! - [`expressions`] — expression-level resolution + calls + names
//! - [`checks`] — assignment / l-value / privacy / final-field checks
//! - [`util`] — pure AST helpers and constants

use std::collections::HashMap;

use leek_diagnostics::Diagnostic;
use leek_parser::ast::{AstNode, ClassDecl, FnDecl, SourceFile, Stmt};
use leek_span::SourceId;
use leek_syntax::Version;

pub mod builtins;
mod checks;
mod classes;
mod expressions;
pub mod folder;
pub mod include_graph;
pub mod pipeline;
mod scope;
mod statements;
mod util;

use scope::FnMeta;
pub use scope::SymbolKind;

pub mod index;
pub use index::{ResolveTable, ResolvedRef, Symbol, SymbolId};

/// Diagnostic codes produced by the resolver. Re-exported from the
/// central catalog in [`leek_diagnostics::codes`] so existing
/// `codes::FOO` references keep working — call sites don't change
/// when codes move into the shared module.
pub use leek_diagnostics::codes;

// ---- Public entry points ----

/// Walk the source file at the latest version and produce diagnostics.
pub fn resolve(file: &SourceFile, source: SourceId) -> Vec<Diagnostic> {
    resolve_with_version(file, source, Version::LATEST)
}

/// Same as [`resolve`] but lets callers pin the language version, so
/// version-gated checks (e.g. case-sensitive keyword detection at v3+)
/// fire correctly.
pub fn resolve_with_version(
    file: &SourceFile,
    source: SourceId,
    version: Version,
) -> Vec<Diagnostic> {
    resolve_with_options(file, source, version, Options::default())
}

/// Options influencing what diagnostics are emitted.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Strict mode (`// @strict`) — enables stricter member-existence
    /// checks (`this.unknown`, `instance.unknown`).
    pub strict: bool,
    /// Enables `import ...` statements for builtin libraries.
    pub experimental_imports: bool,
    /// **Experimental.** Allow a function name to be declared more than
    /// once (overloads / repeated signatures) without
    /// `REDECLARED_SYMBOL`. Intended for signature files; arities of
    /// the repeated declarations are unioned. Off by default.
    pub experimental_overloads: bool,
}

pub fn resolve_with_options(
    file: &SourceFile,
    source: SourceId,
    version: Version,
    opts: Options,
) -> Vec<Diagnostic> {
    resolve_collecting(file, source, version, opts).diagnostics
}

/// Resolver outcome carrying both diagnostics and the LSP-facing
/// [`ResolveTable`]. Prefer this entry point over
/// [`resolve_with_options`] when downstream consumers (the LSP,
/// `miku watch`) need symbol/reference data.
#[derive(Debug, Clone, Default)]
pub struct ResolveResult {
    pub diagnostics: Vec<Diagnostic>,
    pub table: ResolveTable,
}

pub fn resolve_collecting(
    file: &SourceFile,
    source: SourceId,
    version: Version,
    opts: Options,
) -> ResolveResult {
    let mut r = Resolver::new(source, version, opts);
    r.resolve_file(file);
    // References are pushed in walk order; sort by offset so the
    // LSP's cursor-position lookup can binary-search.
    r.references.sort_by_key(|r| r.name_offset);
    ResolveResult {
        diagnostics: r.diagnostics,
        table: ResolveTable {
            symbols: r.symbols,
            references: r.references,
        },
    }
}

// ---- Resolver state ----
//
// All fields are `pub(crate)` so the methods spread across the
// `scope`, `classes`, `statements`, `expressions`, and `checks`
// modules can mutate the same `Resolver` struct via `impl Resolver`
// blocks. The struct itself stays private to the crate.

pub(crate) struct Resolver {
    pub(crate) source: SourceId,
    pub(crate) version: Version,
    pub(crate) opts: Options,
    pub(crate) diagnostics: Vec<Diagnostic>,

    /// Stack of lexical scopes — `scopes[0]` is the builtin scope,
    /// `scopes[1]` is the file scope, and inner scopes push on top.
    /// Each name resolves to a [`SymbolId`] pointing into
    /// [`symbols`](Self::symbols).
    pub(crate) scopes: Vec<HashMap<String, SymbolId>>,
    /// Parallel to [`scopes`](Self::scopes): `true` for scopes that
    /// open a new function/method/lambda body (a closure boundary).
    /// The shadowing check stops at the nearest function boundary —
    /// crossing one and shadowing an outer binding is allowed.
    pub(crate) scope_boundaries: Vec<bool>,

    /// All declared symbols, allocated as the resolver visits each
    /// declaration. Indexed by [`SymbolId.0`](SymbolId) — slot
    /// position is the canonical id.
    pub(crate) symbols: Vec<Symbol>,
    /// Every successful name resolution (the LSP's go-to-def
    /// source). Pushed in source order; sorted by `name_offset`
    /// before being exposed via [`ResolveTable`].
    pub(crate) references: Vec<ResolvedRef>,
    /// Stack of "what symbol am I lexically inside of right now" —
    /// used to attribute each newly declared [`Symbol::container`].
    /// Top-level declarations see an empty stack.
    pub(crate) containers: Vec<SymbolId>,

    /// Arity + version metadata for functions/lambdas we've seen.
    /// Keyed by name. Lambdas bound to `var name = (…) -> …` get an
    /// entry too. Names not in the map are treated as variadic and
    /// version-agnostic — that's the default for runtime helpers we
    /// haven't classified yet.
    pub(crate) fn_meta: HashMap<String, FnMeta>,
    /// Names that have appeared on the left of an assignment (`cos
    /// = function(…) {…}`, `cos = f`). For these we skip the
    /// `BUILTIN_FN_META` fallback at call sites — the user has
    /// shadowed the builtin with a value of unknown arity, and
    /// upstream Leekscript dispatches to the user's binding rather
    /// than the original builtin.
    pub(crate) reassigned_names: std::collections::HashSet<String>,
    /// Symbols exposed via `import <builtin-library>` statements.
    pub(crate) imported_library_symbols: std::collections::HashSet<String>,

    // ---- Class metadata ----
    /// Class-name → set of field names that were declared `final`.
    pub(crate) class_final_fields: HashMap<String, std::collections::HashSet<String>>,
    /// Class-name → set of `static final` field names.
    pub(crate) class_static_final_fields: HashMap<String, std::collections::HashSet<String>>,
    /// Class-name → set of all declared field names. Used to detect
    /// `this.unknown_field` references.
    pub(crate) class_fields_all: HashMap<String, std::collections::HashSet<String>>,
    /// Class-name → set of `static` field/method names. Used for
    /// `ClassName.foo` member existence checks.
    pub(crate) class_static_members: HashMap<String, std::collections::HashSet<String>>,
    /// Classes that extend a parent we haven't analyzed (or any
    /// parent at all) — strict member checks are skipped to avoid
    /// false positives from inherited members.
    pub(crate) class_has_unknown_parent: std::collections::HashSet<String>,
    /// Subclass-name → parent-class-name (immediate parent only).
    pub(crate) class_parent: HashMap<String, String>,
    /// Classes whose constructor was declared `private` / `protected`.
    pub(crate) class_private_constructor: std::collections::HashSet<String>,
    pub(crate) class_protected_constructor: std::collections::HashSet<String>,
    pub(crate) class_private_fields: HashMap<String, std::collections::HashSet<String>>,
    pub(crate) class_protected_fields: HashMap<String, std::collections::HashSet<String>>,
    pub(crate) class_private_methods: HashMap<String, std::collections::HashSet<String>>,
    pub(crate) class_protected_methods: HashMap<String, std::collections::HashSet<String>>,
    pub(crate) class_private_static_methods: HashMap<String, std::collections::HashSet<String>>,
    pub(crate) class_protected_static_methods: HashMap<String, std::collections::HashSet<String>>,
    /// Class-name → (method-name → arity range).
    pub(crate) class_method_arities: HashMap<String, HashMap<String, (u8, u8)>>,

    // ---- Var class-type tracking ----
    /// Local variable name → class name (any `... x = new C(...)`), as a
    /// **stack of scopes** parallel to [`scopes`](Self::scopes): one map per
    /// lexical scope, pushed/popped in lockstep. A flat map would leak a
    /// binding's class type out of the block it was declared in, so a later
    /// same-named variable in a sibling/outer scope would be wrongly treated
    /// as that class (bogus final-field / privacy diagnostics). Used by
    /// final-field assignment checks, regardless of whether the binding had
    /// an explicit type annotation. Access via [`var_class_of`](Self::var_class_of)
    /// / [`set_var_class`](Self::set_var_class).
    pub(crate) var_class_types: Vec<HashMap<String, String>>,
    /// Per-scope subset of [`var_class_types`](Self::var_class_types) limited
    /// to bindings that had an explicit class annotation (`A x = new A()`).
    /// Privacy checks fire here — `var x = new A()` doesn't restrict access to
    /// private members because the static type is dynamic.
    pub(crate) var_class_types_typed: Vec<HashMap<String, String>>,

    // ---- Control-flow context ----
    /// Depth of enclosing loops — controls `continue`. Resets at
    /// function/lambda boundaries.
    pub(crate) loop_depth: u32,
    /// Depth of enclosing breakable constructs (loops + `switch`) —
    /// controls `break`. Resets at function boundaries too.
    pub(crate) breakable_depth: u32,

    // ---- Class-body context ----
    /// True while we're inside a class body whose fields/methods we
    /// fully know about. Stays `false` when the class extends a
    /// parent we haven't analyzed.
    pub(crate) in_class: bool,
    /// Name of the class we're currently inside (when
    /// [`in_class`](Self::in_class) is `true`).
    pub(crate) current_class: Option<String>,
    /// True inside a class constructor body — constructors are
    /// permitted to assign to `final` fields via `this`.
    pub(crate) in_constructor: bool,
}

impl Resolver {
    pub(crate) fn new(source: SourceId, version: Version, opts: Options) -> Self {
        // The builtin set lives in a static `LazyLock` (see
        // `scope::BUILTIN_SCOPE` / `scope::BUILTIN_FN_META`) — every
        // resolver shares it without paying for the ~330 inserts on
        // construction.
        Self {
            source,
            version,
            opts,
            diagnostics: Vec::new(),
            scopes: Vec::new(),
            scope_boundaries: Vec::new(),
            symbols: Vec::new(),
            references: Vec::new(),
            containers: Vec::new(),
            fn_meta: HashMap::new(),
            reassigned_names: std::collections::HashSet::new(),
            imported_library_symbols: std::collections::HashSet::new(),
            class_final_fields: HashMap::new(),
            class_static_final_fields: HashMap::new(),
            class_fields_all: HashMap::new(),
            class_static_members: HashMap::new(),
            class_has_unknown_parent: std::collections::HashSet::new(),
            class_parent: HashMap::new(),
            class_private_constructor: std::collections::HashSet::new(),
            class_protected_constructor: std::collections::HashSet::new(),
            class_private_fields: HashMap::new(),
            class_protected_fields: HashMap::new(),
            class_private_methods: HashMap::new(),
            class_protected_methods: HashMap::new(),
            class_private_static_methods: HashMap::new(),
            class_protected_static_methods: HashMap::new(),
            class_method_arities: HashMap::new(),
            var_class_types: Vec::new(),
            var_class_types_typed: Vec::new(),
            loop_depth: 0,
            breakable_depth: 0,
            in_class: false,
            current_class: None,
            in_constructor: false,
        }
    }

    fn resolve_file(&mut self, file: &SourceFile) {
        // Push a file scope on top of the builtin scope so top-level
        // declarations live separately and shadowing checks can see
        // outer scopes without the noise of every builtin.
        self.push_scope();
        // Two-pass for forward references: first collect
        // declarations, then walk bodies. Matching on the node kind before
        // casting consumes each child with a single `cast` rather than cloning
        // it for every failed cast attempt.
        for child in file.syntax().children() {
            match child.kind() {
                leek_syntax::SyntaxKind::FnDecl => {
                    if let Some(fn_decl) = FnDecl::cast(child) {
                        self.declare_fn(&fn_decl);
                    }
                }
                leek_syntax::SyntaxKind::ClassDecl => {
                    if let Some(cls) = ClassDecl::cast(child) {
                        self.declare_class(&cls);
                    }
                }
                _ => {
                    if let Some(stmt) = Stmt::cast(child) {
                        self.declare_top_stmt(&stmt);
                    }
                }
            }
        }
        let mut terminated_at: Option<leek_span::Span> = None;
        for child in file.syntax().children() {
            match child.kind() {
                leek_syntax::SyntaxKind::FnDecl => {
                    if let Some(fn_decl) = FnDecl::cast(child) {
                        self.resolve_fn_body(&fn_decl);
                    }
                }
                leek_syntax::SyntaxKind::ClassDecl => {
                    if let Some(cls) = ClassDecl::cast(child) {
                        self.resolve_class(&cls);
                    }
                }
                _ => {
                    if let Some(stmt) = Stmt::cast(child) {
                        self.resolve_stmt(&stmt);
                        if terminated_at.is_none()
                            && crate::statements::is_block_terminator(&stmt)
                        {
                            terminated_at = Some(self.node_span(stmt.syntax()));
                        } else if terminated_at.is_some() {
                            self.err(
                                codes::CANT_ADD_INSTRUCTION_AFTER_BREAK,
                                self.node_span(stmt.syntax()),
                                "cannot add instruction after a terminator (return/break/continue)"
                                    .to_string(),
                            );
                            terminated_at = None;
                        }
                    }
                }
            }
        }
        // Final pass: anchor class references in type-annotation
        // positions. Runs while the file scope (holding every class
        // symbol) is still on the stack so name lookups succeed.
        self.record_type_ref_classes(file.syntax());
        self.pop_scope();
    }
}

#[cfg(test)]
mod index_tests {
    use leek_lexer::lex;
    use leek_parser::ast::{AstNode, SourceFile as AstSourceFile};
    use leek_parser::parse_tokens;
    use leek_span::SourceId;
    use leek_syntax::SyntaxNode;

    use super::*;

    fn run(text: &str) -> ResolveResult {
        run_with_options(text, Options::default())
    }

    fn run_with_options(text: &str, opts: Options) -> ResolveResult {
        let src = SourceId::new(1).unwrap();
        let lex_out = lex(text, src, Version::LATEST);
        let parse = parse_tokens(text, src, &lex_out.tokens, Version::LATEST);
        let ast = AstSourceFile::cast(SyntaxNode::new_root(parse.green))
            .expect("parse produced a SourceFile");
        resolve_collecting(&ast, src, Version::LATEST, opts)
    }

    #[test]
    fn table_records_var_decl_and_use() {
        let r = run("var x = 5;\nvar y = x + 1;\n");
        // Two declarations: `x` and `y`.
        assert_eq!(r.table.symbols.len(), 2);
        let x = r.table.symbols.iter().find(|s| s.name == "x").unwrap();
        let y = r.table.symbols.iter().find(|s| s.name == "y").unwrap();
        assert_eq!(x.kind, SymbolKind::Local);
        assert_eq!(y.kind, SymbolKind::Local);
        // One reference: `x` inside `var y = x + 1`.
        assert_eq!(r.table.references.len(), 1);
        assert_eq!(r.table.references[0].target, x.id);
    }

    #[test]
    fn reference_at_finds_cursor_inside_use() {
        let r = run("var apple = 5;\nvar n = apple;\n");
        // Find the byte offset of the use of `apple` on line 2.
        let src = "var apple = 5;\nvar n = apple;\n";
        // Skip the declaration occurrence; find the second `apple`.
        let first = src.find("apple").unwrap();
        let use_offset =
            (first + "apple".len() + src[first + "apple".len()..].find("apple").unwrap()) as u32;
        let found = r.table.reference_at(use_offset).expect("cursor on use");
        let target = r.table.symbol(found.target).unwrap();
        assert_eq!(target.name, "apple");
        assert_eq!(target.kind, SymbolKind::Local);
    }

    #[test]
    fn import_requires_experimental_flag() {
        let r = run("import fight.generator;\n");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == codes::AI_NOT_EXISTING)
        );
    }

    #[test]
    fn import_known_library_when_enabled() {
        let r = run_with_options(
            "import fight.generator;\nfightGenerate();\n",
            Options {
                strict: false,
                experimental_imports: true,
                ..Default::default()
            },
        );
        assert!(
            !r.diagnostics
                .iter()
                .any(|d| d.code == codes::AI_NOT_EXISTING)
        );
    }

    #[test]
    fn redeclared_function_errors_without_overloads() {
        let r = run("function a() { return 1 }\nfunction a(x) { return x }\n");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == codes::REDECLARED_SYMBOL),
            "expected REDECLARED_SYMBOL without the overloads flag"
        );
    }

    #[test]
    fn overloads_allowed_when_enabled() {
        let r = run_with_options(
            "function a() { return 1 }\nfunction a(x) { return x }\na()\na(5)\n",
            Options {
                experimental_overloads: true,
                ..Default::default()
            },
        );
        assert!(
            !r.diagnostics
                .iter()
                .any(|d| d.code == codes::REDECLARED_SYMBOL),
            "overloads should not raise REDECLARED_SYMBOL: {:?}",
            r.diagnostics
        );
        // The unioned arity (0..=1) means both `a()` and `a(5)` are
        // valid — no INVALID_PARAMETER_COUNT.
        assert!(
            !r.diagnostics
                .iter()
                .any(|d| d.code == codes::INVALID_PARAMETER_COUNT),
            "unioned arity should accept both call shapes: {:?}",
            r.diagnostics
        );
    }

    #[test]
    fn var_class_type_does_not_leak_across_scopes() {
        // A `new A()` binding in one function must not leak its class type to a
        // same-named variable in another function. Here `other`'s `obj` is an
        // array, so `obj.f = 5` must NOT raise CANNOT_ASSIGN_FINAL_FIELD — that
        // would be a false positive from a stale, un-popped class-type entry.
        let r = run(
            "class A { final f = 12 }\n\
             function uses() { var obj = new A() }\n\
             function other() { var obj = [1, 2, 3] obj.f = 5 return obj }\n",
        );
        assert!(
            !r.diagnostics
                .iter()
                .any(|d| d.code == codes::CANNOT_ASSIGN_FINAL_FIELD),
            "class type leaked across scopes → false final-field error: {:?}",
            r.diagnostics
        );
    }

    #[test]
    fn final_field_check_still_fires_in_scope() {
        // Positive control: within the same scope the tracked class type still
        // drives the final-field check, so a real violation is still caught.
        let r = run(
            "class A { final f = 12 }\n\
             var a = new A()\n\
             a.f = 5\n",
        );
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == codes::CANNOT_ASSIGN_FINAL_FIELD),
            "in-scope final-field assignment should still error: {:?}",
            r.diagnostics
        );
    }
}
