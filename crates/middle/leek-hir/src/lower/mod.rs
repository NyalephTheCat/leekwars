//! Lower the parser AST into HIR.
//!
//! The lowering pass walks the [`SourceFile`] AST, building a
//! [`HirFile`] with `DefId`s assigned to every declaration and name
//! references resolved to those `DefId`s. Types come from the
//! type-checker output when available, otherwise default to
//! [`Type::Any`] — the interpreter does the work the checker
//! couldn't.
//!
//! This first slice is intentionally narrow:
//! - Functions, classes, globals, locals.
//! - All expression forms supported by the AST (we fall back to
//!   sensible defaults for shapes we don't fully model).
//! - No desugaring yet — compound assigns, postfix ops, and `for`
//!   loops keep their source shape so backends can preserve them.

use std::collections::HashMap;

use leek_diagnostics::Diagnostic;
use leek_parser::ast::{self, AstNode, Stmt as AstStmt};
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use leek_types::Type;

use crate::ir::{Block, Def, DefId, Expr, ExprKind, Global, HirFile, Literal, Local, Stmt};

mod defs;
mod expr;
mod stmt;
mod traits;
mod util;

pub use traits::{LowerExpr, LowerStmt};

/// Lower a source file into HIR.
///
/// The language version is taken from the file's `@version:N`
/// pragma if present, defaulting to v4. Use
/// [`lower_file_versioned`] when the caller already knows the
/// version (corpus runner, etc.) and the source text lacks a
/// pragma.
pub fn lower_file(file: &ast::SourceFile, source: SourceId) -> (HirFile, Vec<Diagnostic>) {
    let mut lo = Lowerer::new(source);
    lo.flags = leek_span::FeatureFlags::from_env();
    lo.lower_file(file);
    (lo.out, lo.diagnostics)
}

/// Like [`lower_file`] but overrides the language version. Used
/// by the corpus runner and other callers that know the target
/// version out-of-band (without a source pragma).
pub fn lower_file_versioned(
    file: &ast::SourceFile,
    source: SourceId,
    version: u8,
) -> (HirFile, Vec<Diagnostic>) {
    lower_file_versioned_with_flags(file, source, version, leek_span::FeatureFlags::from_env())
}

/// As [`lower_file_versioned`] but with explicit experimental [`FeatureFlags`]
/// (threaded by the pipeline from its `Input` instead of read from env).
pub fn lower_file_versioned_with_flags(
    file: &ast::SourceFile,
    source: SourceId,
    version: u8,
    flags: leek_span::FeatureFlags,
) -> (HirFile, Vec<Diagnostic>) {
    let mut lo = Lowerer::new(source);
    lo.flags = flags;
    lo.version_override = Some(version);
    lo.lower_file(file);
    (lo.out, lo.diagnostics)
}

/// Lower `file` at `version` with a signature-only `prelude` merged in
/// ahead of it. The prelude's declarations are pre-declared first so
/// the user file's calls resolve to them (and pick up their
/// `@<backend>-backend:` directives); the prelude contributes no
/// main-block statements. Used by the experimental implicit-prelude
/// path. `prelude_source` is a distinct [`SourceId`] so prelude spans
/// don't collide with the user file's.
pub fn lower_file_with_prelude(
    file: &ast::SourceFile,
    source: SourceId,
    version: u8,
    prelude: &ast::SourceFile,
    prelude_source: SourceId,
) -> (HirFile, Vec<Diagnostic>) {
    lower_file_with_prelude_with_flags(
        file,
        source,
        version,
        prelude,
        prelude_source,
        leek_span::FeatureFlags::from_env(),
    )
}

/// As [`lower_file_with_prelude`] but with explicit experimental
/// [`FeatureFlags`] (threaded by the pipeline instead of read from env).
pub fn lower_file_with_prelude_with_flags(
    file: &ast::SourceFile,
    source: SourceId,
    version: u8,
    prelude: &ast::SourceFile,
    prelude_source: SourceId,
    flags: leek_span::FeatureFlags,
) -> (HirFile, Vec<Diagnostic>) {
    let mut lo = Lowerer::new(source);
    lo.flags = flags;
    lo.version_override = Some(version);
    lo.version = version;

    // Pass 1: pre-declare the prelude's items first, then the user
    // file's, so user calls can resolve to prelude signatures.
    for (f, src) in [(prelude, prelude_source), (file, source)] {
        lo.source = src;
        lo.source_text = f.syntax().text().to_string();
        for child in f.syntax().children() {
            if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
                lo.predeclare_function(&fn_decl);
            } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
                lo.predeclare_class(&cls);
            } else if child.kind() == SyntaxKind::EnumDecl {
                lo.lower_enum_decl(&child);
            }
        }
    }

    // Pass 2: lower bodies. Prelude functions are bodiless, so this
    // only registers the user file's function/class bodies.
    for (f, src) in [(prelude, prelude_source), (file, source)] {
        lo.source = src;
        lo.source_text = f.syntax().text().to_string();
        for child in f.syntax().children() {
            if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
                lo.lower_function_body(&fn_decl);
            } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
                lo.lower_class_body(&cls);
            }
        }
    }

    // The user file's main block (the prelude has none).
    for child in file.syntax().children() {
        if ast::FnDecl::cast(child.clone()).is_some()
            || ast::ClassDecl::cast(child.clone()).is_some()
        {
            continue;
        }
        if let Some(stmt) = AstStmt::cast(child.clone()) {
            let mut buf = Vec::new();
            lo.lower_stmt_flat(&stmt, &mut buf);
            lo.out.main.extend(buf);
        }
    }
    (lo.out, lo.diagnostics)
}

/// Lower a multi-file Leekscript project into a single [`HirFile`].
///
/// Inputs:
/// - `includes` — every file the entry transitively includes, in
///   topological order (leaves first). Each carries its parsed AST,
///   `SourceId`, and canonical path.
/// - `entry` — the entry file's AST + source id + canonical path.
/// - `resolved_includes` — `(includer_canonical, include_name)` →
///   `included_canonical`, built by
///   `leek_resolver::include_graph::build_include_graph`.
///
/// Semantics (per `doc/pipeline.md` §5.1.3):
/// - Top-level declarations from every file are visible everywhere
///   (forward-declared in include-graph order).
/// - Each `Stmt::Include("name")` at run-time evaluates the included
///   file's main-block statements at that position. Two includes of
///   the same file are de-duplicated by the include graph itself, so
///   the splicer sees each main block at most once.
///
/// The single returned `HirFile` is what every downstream consumer
/// (resolver, type-checker, MIR, codegen) sees — no consumer needs
/// to know about includes.
pub fn lower_files(
    entry: (&ast::SourceFile, SourceId, &std::path::Path),
    includes: &[(ast::SourceFile, SourceId, std::path::PathBuf)],
    resolved_includes: &std::collections::BTreeMap<
        (std::path::PathBuf, String),
        std::path::PathBuf,
    >,
) -> (HirFile, Vec<Diagnostic>) {
    let mut lo = Lowerer::new(entry.1);
    lo.flags = leek_span::FeatureFlags::from_env();

    // Pass 1: pre-declare top-level items across every file
    // (included files first, entry last) so cross-file references
    // resolve via `file_decls`.
    for (file, src, _path) in includes {
        lo.source = *src;
        lo.source_text = file.syntax().text().to_string();
        for child in file.syntax().children() {
            if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
                lo.predeclare_function(&fn_decl);
            } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
                lo.predeclare_class(&cls);
            } else if child.kind() == SyntaxKind::EnumDecl {
                lo.lower_enum_decl(&child);
            }
        }
    }
    lo.source = entry.1;
    lo.source_text = entry.0.syntax().text().to_string();
    for child in entry.0.syntax().children() {
        if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
            lo.predeclare_function(&fn_decl);
        } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
            lo.predeclare_class(&cls);
        } else if child.kind() == SyntaxKind::EnumDecl {
            lo.lower_enum_decl(&child);
        }
    }

    // Pass 2: lower function/class bodies for every file in the
    // same order.
    for (file, src, _path) in includes {
        lo.source = *src;
        for child in file.syntax().children() {
            if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
                lo.lower_function_body(&fn_decl);
            } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
                lo.lower_class_body(&cls);
            }
        }
    }
    lo.source = entry.1;
    for child in entry.0.syntax().children() {
        if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
            lo.lower_function_body(&fn_decl);
        } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
            lo.lower_class_body(&cls);
        }
    }

    // Pass 3: lower each file's main block separately into a
    // path-keyed map. Statements that are themselves `Stmt::Include`
    // get post-processed by `splice_includes` once the per-file maps
    // are complete.
    use std::collections::BTreeMap;
    let mut per_file_main: BTreeMap<std::path::PathBuf, Vec<Stmt>> = BTreeMap::new();
    for (file, src, path) in includes {
        lo.source = *src;
        let mut main_stmts = Vec::new();
        for child in file.syntax().children() {
            if ast::FnDecl::cast(child.clone()).is_some()
                || ast::ClassDecl::cast(child.clone()).is_some()
            {
                continue;
            }
            if let Some(stmt) = AstStmt::cast(child.clone()) {
                lo.lower_stmt_flat(&stmt, &mut main_stmts);
            }
        }
        per_file_main.insert(path.clone(), main_stmts);
    }
    lo.source = entry.1;
    let mut entry_main = Vec::new();
    for child in entry.0.syntax().children() {
        if ast::FnDecl::cast(child.clone()).is_some()
            || ast::ClassDecl::cast(child.clone()).is_some()
        {
            continue;
        }
        if let Some(stmt) = AstStmt::cast(child.clone()) {
            lo.lower_stmt_flat(&stmt, &mut entry_main);
        }
    }

    // Splice. Walk the entry's main; each `Stmt::Include("name")`
    // becomes the included file's already-lowered main statements
    // (which themselves may have been spliced). Cycle detection
    // happens at the include-graph layer; here we just dedupe by
    // path so a diamond import doesn't double the body.
    let mut already_spliced: std::collections::BTreeSet<std::path::PathBuf> =
        std::collections::BTreeSet::new();
    lo.out.main = splice_includes(
        entry_main,
        entry.2,
        &per_file_main,
        resolved_includes,
        &mut already_spliced,
    );

    (lo.out, lo.diagnostics)
}

fn splice_includes(
    stmts: Vec<Stmt>,
    current_path: &std::path::Path,
    per_file_main: &std::collections::BTreeMap<std::path::PathBuf, Vec<Stmt>>,
    resolved: &std::collections::BTreeMap<(std::path::PathBuf, String), std::path::PathBuf>,
    already: &mut std::collections::BTreeSet<std::path::PathBuf>,
) -> Vec<Stmt> {
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            Stmt::Include(inc) => {
                let included_path = resolved
                    .get(&(current_path.to_path_buf(), inc.path.clone()))
                    .cloned();
                let Some(p) = included_path else {
                    // Unresolved include — drop it. The diagnostic
                    // was already emitted by the include-graph
                    // builder.
                    continue;
                };
                if !already.insert(p.clone()) {
                    // Diamond: file already merged once. Logical-
                    // merge semantics deduplicate.
                    continue;
                }
                let Some(body) = per_file_main.get(&p).cloned() else {
                    continue;
                };
                let spliced = splice_includes(body, &p, per_file_main, resolved, already);
                out.extend(spliced);
            }
            other => out.push(other),
        }
    }
    out
}

pub(crate) struct Lowerer {
    pub(crate) source: SourceId,
    pub(crate) out: HirFile,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Full source text of the file currently being pre-declared, used
    /// to read doc comments (and their `@<backend>-backend:` directives)
    /// that aren't represented in the AST. Set per-file before pass 1.
    pub(crate) source_text: String,
    /// Experimental feature flags for this lowering. Defaults to
    /// [`FeatureFlags::from_env`] for direct callers; the pipeline overrides it
    /// with the flags threaded through its `Input` so the lowering query stays
    /// pure (no env reads).
    pub(crate) flags: leek_span::FeatureFlags,
    /// Source language version, detected from the file's
    /// `@version:N` pragma in [`Lowerer::lower_file`]. Used for the
    /// handful of version-specific lowering decisions (v1 doesn't
    /// process the `\"` escape inside `"…"` strings, etc.).
    pub(crate) version: u8,
    /// External override for [`Self::version`]. When `Some(v)`,
    /// the pragma-detection step in `lower_file` is skipped and
    /// `v` is used directly. Set by
    /// [`lower_file_versioned`](super::lower_file_versioned).
    pub(crate) version_override: Option<u8>,
    /// Stack of lexical scopes mapping names to their `DefId`. The
    /// innermost scope is at the back.
    pub(crate) scopes: Vec<Scope>,
    /// Parallel marker — `true` for scopes that open a function
    /// boundary (lambda / method / function body). Used to skip
    /// builtin-style globals when looking up "local"-style names.
    pub(crate) boundaries: Vec<bool>,
    /// Names of items registered in the file scope after the first
    /// pass. Keyed by name → kind so we know whether `foo` refers
    /// to a function, class, or global.
    pub(crate) file_decls: HashMap<String, NameKind>,
    /// Stack of class contexts — pushed while lowering method or
    /// constructor bodies so bare references to field names can
    /// rewrite to `this.field`.
    pub(crate) class_ctx: Vec<ClassCtx>,
}

#[derive(Default)]
pub(crate) struct ClassCtx {
    pub(crate) field_names: std::collections::HashSet<String>,
    pub(crate) static_field_names: std::collections::HashSet<String>,
    pub(crate) method_names: std::collections::HashSet<String>,
    pub(crate) static_method_names: std::collections::HashSet<String>,
    /// Per method name, the set of declared parameter counts.
    /// `class A { sqrt() {} sqrt(x, y) {} }` records `sqrt → {0, 2}`.
    /// Used so a bare `sqrt(25)` (arity 1) inside a body falls
    /// through to the builtin instead of recursing into the class
    /// method that doesn't match.
    pub(crate) method_arities: std::collections::HashMap<String, std::collections::HashSet<usize>>,
    pub(crate) static_method_arities:
        std::collections::HashMap<String, std::collections::HashSet<usize>>,
}

#[derive(Default)]
pub(crate) struct Scope {
    pub(crate) locals: HashMap<String, DefId>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NameKind {
    Function(DefId),
    Class(DefId),
    #[allow(dead_code)] // globals aren't pre-declared yet
    Global(DefId),
}

impl Lowerer {
    fn new(source: SourceId) -> Self {
        Self {
            source,
            // Default to v4 (the latest); `lower_file` overrides
            // from the file's `@version:N` pragma if present.
            version: 4,
            version_override: None,
            out: HirFile::default(),
            diagnostics: Vec::new(),
            source_text: String::new(),
            // Pure default; the public `lower_file*` entries set this (from env
            // at that boundary, or explicitly via the `*_with_flags` variants
            // the pipeline uses with flags threaded from its `Input`).
            flags: leek_span::FeatureFlags::none(),
            scopes: vec![Scope::default()],
            boundaries: vec![true],
            file_decls: HashMap::new(),
            class_ctx: Vec::new(),
        }
    }

    fn lower_file(&mut self, file: &ast::SourceFile) {
        // Capture the source text up front so pre-declaration can read
        // doc comments (and their backend directives).
        self.source_text = file.syntax().text().to_string();
        // Pin the version. Explicit override wins; otherwise sniff
        // the file's `@version:N` pragma. Falls back to v4 so
        // pragma-less files keep modern semantics.
        if let Some(v) = self.version_override {
            self.version = v;
        } else {
            let (pragmas, _) = leek_syntax::parse_pragmas(&self.source_text, self.source);
            self.version = match pragmas.version {
                leek_syntax::Version::V1 => 1,
                leek_syntax::Version::V2 => 2,
                leek_syntax::Version::V3 => 3,
                leek_syntax::Version::V4 => 4,
            };
        }
        // First pass — register every top-level item so bodies can
        // reference each other in any order.
        for child in file.syntax().children() {
            if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
                self.predeclare_function(&fn_decl);
            } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
                self.predeclare_class(&cls);
            } else if child.kind() == SyntaxKind::EnumDecl {
                self.lower_enum_decl(&child);
            }
        }
        // Second pass — lower bodies (functions / classes) and the
        // main-block statements in source order.
        for child in file.syntax().children() {
            if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
                self.lower_function_body(&fn_decl);
            } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
                self.lower_class_body(&cls);
            } else if let Some(stmt) = AstStmt::cast(child.clone()) {
                let mut buf = Vec::new();
                self.lower_stmt_flat(&stmt, &mut buf);
                self.out.main.extend(buf);
            }
        }
    }

    // ---- DefId / scope plumbing ----

    fn alloc_def(&mut self, def: Def) -> DefId {
        let id = DefId(u32::try_from(self.out.defs.len()).expect("more than u32::MAX defs"));
        self.out.defs.push(def);
        id
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
        self.boundaries.push(false);
    }
    fn push_function_scope(&mut self) {
        self.scopes.push(Scope::default());
        self.boundaries.push(true);
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
        self.boundaries.pop();
    }

    fn declare_local(&mut self, name: &str, span: Span, ty: Option<Type>) -> DefId {
        let id = self.alloc_def(Def::Local(Local {
            name: name.into(),
            ty,
            span,
        }));
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_string(), id);
        }
        id
    }

    /// Register a `Def::Global` and expose its name via the file's
    /// `file_decls` map so references from any function resolve to
    /// `NameRef::Global`. The MIR lowerer reads `file_decls` when
    /// building the program-wide `globals` table.
    fn declare_global(&mut self, name: &str, span: Span, ty: Option<Type>) -> DefId {
        if let Some(NameKind::Global(id)) = self.file_decls.get(name).copied() {
            return id;
        }
        let id = self.alloc_def(Def::Global(Global {
            name: name.into(),
            ty,
            init: None,
            span,
        }));
        self.out.items.push(id);
        self.file_decls.insert(name.into(), NameKind::Global(id));
        id
    }

    /// Look up a name through the scope stack (innermost first).
    /// Crosses lambda boundaries (those have `boundaries[i] =
    /// false`) so closures can capture outer locals, but stops
    /// at method / top-level-function boundaries (`true`) — bare
    /// names inside a method body shouldn't reach across into
    /// the enclosing scope's locals, otherwise outer `var a` and
    /// `class A { a; m() { return a } }`-style field rewrites
    /// fight for the same name.
    fn lookup_local(&self, name: &str) -> Option<DefId> {
        for (i, scope) in self.scopes.iter().enumerate().rev() {
            if let Some(&id) = scope.locals.get(name) {
                return Some(id);
            }
            if i > 0 && self.boundaries[i] {
                return None;
            }
        }
        None
    }

    // ---- Small helpers ----

    // `&self` kept for ergonomic `self.null_expr(span)` use across lowering.
    #[allow(clippy::unused_self)]
    pub(crate) fn null_expr(&self, span: Span) -> Expr {
        Expr {
            kind: ExprKind::Literal(Literal::Null),
            ty: Type::Null,
            span,
        }
    }

    /// Lower an optional AST expression, falling back to a null literal at
    /// `span` when it's absent. Centralizes the recurring
    /// `opt.map(|e| self.lower_expr(&e)).unwrap_or_else(|| self.null_expr(..))`
    /// shape (which can't be a `map_or_else` — both arms borrow `&mut self`).
    pub(crate) fn lower_expr_or_null(&mut self, e: Option<ast::Expr>, span: Span) -> Expr {
        match e {
            Some(e) => self.lower_expr(&e),
            None => self.null_expr(span),
        }
    }

    /// Lower an optional AST statement into a boxed HIR statement, falling
    /// back to an empty block at `span` when absent — the body shape every
    /// loop/branch lowering uses for a missing body.
    pub(crate) fn lower_stmt_or_empty(&mut self, s: Option<ast::Stmt>, span: Span) -> Box<Stmt> {
        match s {
            Some(s) => Box::new(self.lower_stmt(&s)),
            None => Box::new(Stmt::Block(Block {
                stmts: vec![],
                span,
            })),
        }
    }

    pub(crate) fn span_of_node(&self, n: &SyntaxNode) -> Span {
        let r = n.text_range();
        Span::new(self.source, u32::from(r.start()), u32::from(r.end()))
    }
    pub(crate) fn span_of_token(&self, t: &SyntaxToken) -> Span {
        let r = t.text_range();
        Span::new(self.source, u32::from(r.start()), u32::from(r.end()))
    }
}
