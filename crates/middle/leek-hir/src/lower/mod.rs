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

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use leek_diagnostics::Diagnostic;
use leek_parser::ast::{self, AstNode, Stmt as AstStmt};
use leek_resolver::include_graph::{ExpandUnit, IncludeExpander};
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, SyntaxNode, SyntaxToken, Version};
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
/// This convenience entry has no out-of-band version, so it settles one
/// itself at this boundary: the file's `@version:N` pragma, else v4. Every
/// caller that knows the version (the pipeline's `LowerHir` step, the
/// corpus runner) uses [`lower_file_versioned`] instead, and the lowerer
/// never re-derives the version from pragmas.
pub fn lower_file(file: &ast::SourceFile, source: SourceId) -> (HirFile, Vec<Diagnostic>) {
    let text = file.syntax().text().to_string();
    let (pragmas, _) = leek_syntax::parse_pragmas(&text, source);
    let mut lo = Lowerer::new(source, pragmas.effective_version(Version::LATEST));
    lo.flags = leek_span::FeatureFlags::from_env();
    lo.lower_file(file);
    (lo.out, lo.diagnostics)
}

/// Like [`lower_file`] but at an explicit language version (a pipeline
/// `version_byte`, 1..=4; out-of-range values mean v4). Any `@version`
/// pragma in the text is ignored: the caller's version is authoritative.
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
    let mut lo = Lowerer::new(source, Version::from_byte(version));
    lo.flags = flags;
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
///
/// The prelude is lowered as a leading unit of [`lower_files`] with no
/// include graph: it contributes declarations but no main block, and the
/// user file's `include(...)` statements are kept as `Stmt::Include`.
pub fn lower_file_with_prelude_with_flags(
    file: &ast::SourceFile,
    source: SourceId,
    version: u8,
    prelude: &ast::SourceFile,
    prelude_source: SourceId,
    flags: leek_span::FeatureFlags,
) -> (HirFile, Vec<Diagnostic>) {
    let version = Version::from_byte(version);
    let prelude_unit = LowerUnit {
        ast: prelude,
        source: prelude_source,
        path: Path::new(PRELUDE_UNIT_PATH),
        version,
    };
    let entry = LowerUnit {
        ast: file,
        source,
        path: Path::new(""),
        version,
    };
    lower_files(entry, &[prelude_unit], None, flags)
}

/// Synthetic path of the library/prelude header unit. No `include`
/// statement resolves to it, so it never contributes a main block.
pub const PRELUDE_UNIT_PATH: &str = "<prelude>";

/// One file of a multi-file lowering: its AST, source id, canonical path,
/// and the language version it was lexed/parsed at (its own explicit
/// `@version` pragma, or the entry's settled version).
#[derive(Debug, Clone, Copy)]
pub struct LowerUnit<'a> {
    pub ast: &'a ast::SourceFile,
    pub source: SourceId,
    pub path: &'a Path,
    pub version: Version,
}

/// Lower a multi-file Leekscript project into a single [`HirFile`].
///
/// Inputs:
/// - `includes` — every file the entry transitively includes, in
///   topological order (leaves first), plus any signature-only
///   library/prelude header units (put these first).
/// - `entry` — the entry file.
/// - `resolved_includes` — `(includer_canonical, include_name)` →
///   `included_canonical`, built by
///   `leek_resolver::include_graph::build_include_graph`. `None` when
///   there is no include graph: `include(...)` statements are then kept
///   as `Stmt::Include` instead of being spliced (or dropped).
/// - `flags` — the pipeline's experimental feature flags.
///
/// Each unit is lowered at its **own** version (string-escape rules and
/// other version-dependent lowering follow the file, not a v4 default).
///
/// Semantics — `include` is *inline expansion*, matching upstream's
/// textual splicing:
/// - Top-level declarations (functions, classes, enums, `global`s) from
///   every file are visible everywhere, forward-declared in include-graph
///   order before anything is lowered.
/// - Main-block statements are lowered in **execution order**: the entry's
///   children in source order, and an `include("name")` site expands into
///   the included file's main-block statements right there, in the scope
///   that is live at the site. An included file therefore sees the locals
///   its includer declared above the site, and only those (#118, #339).
/// - A file expands at most once, at the first site that reaches it, so a
///   diamond import doesn't double the body.
///
/// The single returned `HirFile` is what every downstream consumer
/// (resolver, type-checker, MIR, codegen) sees — no consumer needs
/// to know about includes.
pub fn lower_files(
    entry: LowerUnit<'_>,
    includes: &[LowerUnit<'_>],
    resolved_includes: Option<&BTreeMap<(PathBuf, String), PathBuf>>,
    flags: leek_span::FeatureFlags,
) -> (HirFile, Vec<Diagnostic>) {
    let mut lo = Lowerer::new(entry.source, entry.version);
    lo.flags = flags;
    let units: Vec<LowerUnit<'_>> = includes
        .iter()
        .copied()
        .chain(std::iter::once(entry))
        .collect();

    // Pass 1: pre-declare top-level items across every file
    // (included files first, entry last) so cross-file references
    // resolve via `file_decls`.
    for unit in &units {
        lo.enter_unit(unit);
        for child in unit.ast.syntax().children() {
            if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
                lo.predeclare_function(&fn_decl);
            } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
                lo.predeclare_class(&cls);
            } else if child.kind() == SyntaxKind::EnumDecl {
                lo.lower_enum_decl(&child);
            }
        }
    }
    // Then every file's globals, in the same order. Kept a separate
    // sub-loop so `items` stays [functions, classes, enums, globals] —
    // that order drives the LeekScript backend's emission.
    for unit in &units {
        lo.enter_unit(unit);
        lo.predeclare_globals(unit.ast);
    }
    // Bodiless signatures' parameter defaults, deferred out of the item
    // loop so they see every file's globals (see
    // `Lowerer::lower_pending_signatures`). Each entry carries its own
    // unit's source and version, so this needs no per-unit loop.
    lo.lower_pending_signatures();

    // Arm include expansion for passes 2 and 3. Without a graph
    // (`resolved_includes == None`, the prelude path) the lowerer keeps
    // emitting `Stmt::Include` exactly as the single-file entries do.
    lo.include_ctx = resolved_includes.map(|resolved| {
        IncludeExpander::new(
            entry.path,
            units.iter().map(|u| ExpandUnit {
                path: u.path.to_path_buf(),
                root: u.ast.syntax().clone(),
                source: u.source,
                version: u.version,
            }),
            resolved.clone(),
        )
    });

    // Pass 2: lower the entry's main block in source order, expanding
    // every `include(...)` site inline as it is reached. This runs
    // *before* function bodies so a top-level include wins the
    // first-site race against an include nested in a body — the
    // precedence the splicer used to get from running the main walk
    // first.
    lo.enter_unit(&entry);
    let mut main = Vec::new();
    lo.lower_main_children(entry.ast.syntax(), &mut main);
    lo.out.main = main;

    // Pass 3: lower function/class bodies for every file. Header
    // functions are bodiless, so header units contribute nothing here.
    // Bodies open a function scope, so they never see main-block locals
    // regardless of the order the two passes run in.
    for unit in &units {
        lo.enter_unit(unit);
        if let Some(ctx) = lo.include_ctx.as_mut() {
            // Include sites inside this unit's bodies resolve relative
            // to this unit.
            ctx.set_current(unit.path);
        }
        for child in unit.ast.syntax().children() {
            if let Some(fn_decl) = ast::FnDecl::cast(child.clone()) {
                lo.lower_function_body(&fn_decl);
            } else if let Some(cls) = ast::ClassDecl::cast(child.clone()) {
                lo.lower_class_body(&cls);
            }
        }
    }

    (lo.out, lo.diagnostics)
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
    /// Language version of the file currently being lowered, supplied
    /// by the caller (the settled `Input::version_byte`, or a
    /// [`LowerUnit`]'s own version). Never re-derived from pragmas here.
    /// Used for the handful of version-specific lowering decisions (v1
    /// doesn't process the `\"` escape inside `"…"` strings, etc.).
    pub(crate) version: Version,
    /// Stack of lexical scopes mapping names to their `DefId`. The
    /// innermost scope is at the back.
    pub(crate) scopes: Vec<Scope>,
    /// Parallel marker — `true` for the scopes that are *opaque* to
    /// bare-name lookup: top-level function bodies, method bodies and
    /// constructor bodies (everything pushed by
    /// [`Self::push_function_scope`]).
    ///
    /// Lambdas are deliberately **not** opaque: `lower_lambda` uses the
    /// plain [`Self::push_scope`], so [`Self::lookup_local`] walks
    /// through into the enclosing locals and those names resolve as
    /// captures. The opaque marker exists for the opposite case —
    /// inside a method, a bare field name must reach the `this.field`
    /// rewrite driven by [`Self::class_ctx`] rather than binding to an
    /// enclosing scope's local of the same name.
    pub(crate) boundaries: Vec<bool>,
    /// Names of items registered in the file scope after the first
    /// pass. Keyed by name → kind so we know whether `foo` refers
    /// to a function, class, or global.
    pub(crate) file_decls: HashMap<String, NameKind>,
    /// Stack of class contexts — pushed while lowering method or
    /// constructor bodies so bare references to field names can
    /// rewrite to `this.field`.
    pub(crate) class_ctx: Vec<ClassCtx>,
    /// Include graph for a multi-file lowering. `Some` makes every
    /// `include(...)` statement expand inline at its site; `None` (the
    /// single-file and prelude entries) keeps it as a `Stmt::Include`.
    pub(crate) include_ctx: Option<IncludeExpander>,
    /// Bodiless signatures whose parameters pass 1 deferred, in source
    /// order. Drained by [`Lowerer::lower_pending_signatures`].
    pub(crate) pending_signatures: Vec<PendingSignature>,
}

/// A bodiless signature that has its `DefId` but not yet its parameters.
///
/// Pass 1 allocates every item's `DefId` in source order, so the unit a
/// signature came from is no longer current by the time its parameters are
/// lowered — it carries the source and version its spans and literals need.
pub(crate) struct PendingSignature {
    pub(crate) source: SourceId,
    pub(crate) version: Version,
    pub(crate) def: DefId,
    pub(crate) decl: ast::FnDecl,
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
    Global(DefId),
}

impl Lowerer {
    fn new(source: SourceId, version: Version) -> Self {
        Self {
            source,
            version,
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
            include_ctx: None,
            pending_signatures: Vec::new(),
        }
    }

    fn lower_file(&mut self, file: &ast::SourceFile) {
        // Capture the source text up front so pre-declaration can read
        // doc comments (and their backend directives).
        self.source_text = file.syntax().text().to_string();
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
        self.predeclare_globals(file);
        self.lower_pending_signatures();
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

    /// Switch the per-file state (source id, version, doc-comment text)
    /// to `unit` before lowering any of its items.
    fn enter_unit(&mut self, unit: &LowerUnit<'_>) {
        self.source = unit.source;
        self.version = unit.version;
        self.source_text = unit.ast.syntax().text().to_string();
    }

    /// Lower the top-level main-block statements of the `SourceFile` node
    /// `root` (everything but function and class declarations) into `out`.
    ///
    /// With an [`IncludeExpander`] armed, an `include(...)` among those children
    /// expands here, so `out` ends up holding the included file's
    /// statements at the site rather than a `Stmt::Include`.
    fn lower_main_children(&mut self, root: &SyntaxNode, out: &mut Vec<Stmt>) {
        for child in root.children() {
            if ast::FnDecl::cast(child.clone()).is_some()
                || ast::ClassDecl::cast(child.clone()).is_some()
            {
                continue;
            }
            if let Some(stmt) = AstStmt::cast(child) {
                self.lower_stmt_flat(&stmt, out);
            }
        }
    }

    /// Expand one `include("name")` site: append the included file's
    /// main-block statements to `out`, lowered right here so they see
    /// exactly the scope the site sees.
    ///
    /// Appends nothing when there is no include graph, when the name
    /// didn't resolve (the include-graph builder already reported that),
    /// or when the file has been expanded at an earlier site.
    pub(crate) fn expand_include(&mut self, name: &str, out: &mut Vec<Stmt>) {
        let Some(expansion) = self.include_ctx.as_mut().and_then(|ctx| ctx.enter(name)) else {
            return;
        };

        // Spans, version-dependent lowering and doc comments follow the
        // included file while its statements interleave with ours.
        let saved = (
            self.source,
            self.version,
            std::mem::take(&mut self.source_text),
        );
        self.source = expansion.source;
        self.version = expansion.version;
        self.source_text = expansion.text();
        self.lower_main_children(&expansion.root, out);
        self.source = saved.0;
        self.version = saved.1;
        self.source_text = saved.2;

        if let Some(ctx) = self.include_ctx.as_mut() {
            ctx.leave();
        }
    }

    // ---- DefId / scope plumbing ----

    fn alloc_def(&mut self, def: Def) -> DefId {
        let id = DefId(u32::try_from(self.out.defs.len()).expect("more than u32::MAX defs"));
        self.out.defs.push(def);
        id
    }

    /// Push a transparent scope: blocks, loop headers, lambda bodies.
    /// [`Self::lookup_local`] walks straight through it.
    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
        self.boundaries.push(false);
    }
    /// Push an opaque scope: top-level function, method and constructor
    /// bodies. [`Self::lookup_local`] stops here. Not for lambdas — see
    /// [`Self::boundaries`].
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

    /// Register every `global` the file declares, before any body is
    /// lowered, so a use that appears *above* its declaration still
    /// resolves to `NameRef::Global` (#53).
    ///
    /// Without this, a name used before its `global` statement fell through
    /// [`Self::resolve_name`] to `NameRef::Builtin(name)`; MIR then keyed
    /// that write to `Place::Global(DefId(0), name)`, which the `DefId`-based
    /// HIR passes couldn't see. `lower_files` made it unconditional — it
    /// lowers every body before any main block.
    ///
    /// `global` is legal in any statement position, so this walks the whole
    /// syntax tree: nested blocks, function, method and lambda bodies
    /// included. Only *direct* `Ident` tokens of a `VarDeclStmt` are
    /// declarator names — a leading type is a nested `TypeRef` node and an
    /// initializer is a nested expression node (the same shape
    /// [`Self::lower_var_decls`] walks). [`Self::declare_global`] is
    /// idempotent, so the later `VarDecl` lowering reuses this `DefId`.
    fn predeclare_globals(&mut self, file: &ast::SourceFile) {
        for node in file.syntax().descendants() {
            if node.kind() != SyntaxKind::VarDeclStmt {
                continue;
            }
            let tokens = || {
                node.children_with_tokens()
                    .filter_map(rowan::NodeOrToken::into_token)
            };
            if !tokens().any(|t| t.kind() == SyntaxKind::KwGlobal) {
                continue;
            }
            for t in tokens().filter(|t| t.kind() == SyntaxKind::Ident) {
                let span = self.span_of_token(&t);
                self.declare_global(t.text(), span, None);
            }
        }
    }

    /// Lower the parameters of every bodiless signature pass 1 deferred,
    /// now that every item and every `global` is registered.
    ///
    /// A signature's parameter defaults are real expressions, and for an
    /// *overloaded* signature pass 1 is the only place they are lowered:
    /// [`Self::lower_function_body`] refills whichever declaration
    /// `file_decls` remembers, which is the last same-named one. Lowering
    /// them inside the item loop resolved them against a half-built
    /// `file_decls` — a default naming a `global` declared anywhere in the
    /// file missed and fell through to `Builtin`/`Unresolved`, so the read
    /// reached the global's slot by name only and was invisible to every
    /// `DefId`-keyed HIR pass (#53). Deferring to just after
    /// [`Self::predeclare_globals`], still before any body, gives a default
    /// the same view of the file every body has.
    fn lower_pending_signatures(&mut self) {
        let (source, version) = (self.source, self.version);
        for pending in std::mem::take(&mut self.pending_signatures) {
            self.source = pending.source;
            self.version = pending.version;
            self.push_function_scope();
            let params = self.lower_params(pending.decl.syntax());
            self.pop_scope();
            if let Some(Def::Function(f)) = self.out.defs.get_mut(pending.def.0 as usize) {
                f.params = params;
            }
        }
        self.source = source;
        self.version = version;
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
    /// Crosses lambda scopes (those have `boundaries[i] = false`,
    /// pushed by `lower_lambda`) so closures can capture outer
    /// locals, but stops at method / top-level-function boundaries
    /// (`true`, pushed by [`Self::push_function_scope`]) — bare
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
