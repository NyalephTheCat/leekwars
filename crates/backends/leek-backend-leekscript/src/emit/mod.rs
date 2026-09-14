//! HIR → LeekScript source emission.

mod expr;

use std::cell::RefCell;
use std::collections::BTreeSet;

use leek_diagnostics::{Diagnostic, codes};
use leek_hir::{
    Block, Def, DefId, ForeachBind, Function, Global, HirFile, MethodDef, Param, Stmt, VarDecl,
};
use leek_span::Span;
use leek_syntax::Version;
use leek_types::Type;

use crate::comments::Comments;
use crate::options::Options;
use crate::rename::{self, RenameMap};
use crate::writer::LsWriter;

/// Result of [`emit`].
pub struct EmittedLeekScript {
    /// The generated official-LeekScript source.
    pub source: String,
    /// Semantics the round-trip could not carry across, each anchored at the
    /// declaration or literal that lost them (#154). Warnings, not errors:
    /// `source` is still valid official LeekScript — it just means slightly
    /// less than the input did.
    pub diagnostics: Vec<Diagnostic>,
}

/// Emit valid official (non-experimental) LeekScript from a checked HIR.
#[must_use]
pub fn emit(hir: &HirFile, opts: &Options) -> EmittedLeekScript {
    let owned;
    let hir = if opts.optimize {
        owned = crate::optimize::run(hir);
        &owned
    } else {
        hir
    };

    let names = rename::build(hir, opts);
    let comments = Comments::build(
        if opts.is_compact() {
            None
        } else {
            opts.source_text.as_deref()
        },
        opts.user_source,
        opts.version,
    );
    let mut em = Emitter {
        opts,
        hir,
        w: LsWriter::new(opts.indent.clone(), opts.is_compact()),
        names,
        comments,
        declared_globals: BTreeSet::new(),
        diagnostics: RefCell::new(Vec::new()),
    };
    em.emit_file();
    let diagnostics = em.diagnostics.take();
    EmittedLeekScript {
        source: em.w.into_string(),
        diagnostics,
    }
}

pub(crate) struct Emitter<'a> {
    pub(crate) opts: &'a Options,
    pub(crate) hir: &'a HirFile,
    pub(crate) w: LsWriter,
    pub(crate) names: RenameMap,
    pub(crate) comments: Comments,
    /// Globals this file has already declared. HIR keeps one
    /// `Stmt::VarDecl { is_global: true }` per declaration site but folds
    /// them all onto a single `DefId`, so only the first site writes the
    /// `global` keyword — see [`Emitter::emit_vardecl`].
    declared_globals: BTreeSet<DefId>,
    /// Semantics dropped on the way out, drained into [`EmittedLeekScript`]
    /// by [`emit`]. `RefCell` so the expression emitter — which holds only
    /// `&self` on some paths — can add to it.
    diagnostics: RefCell<Vec<Diagnostic>>,
}

impl Emitter<'_> {
    /// Record that `what` could not be carried through the round-trip, at
    /// `span`.
    ///
    /// The emitted program stays valid; this is the difference between a
    /// silent drop and one the user is told about, which is the whole point
    /// of #154. Where a semantic *can* be preserved the emitter preserves it
    /// and says nothing.
    pub(crate) fn semantic_loss(&self, span: Span, what: &str, note: &str) {
        self.diagnostics.borrow_mut().push(
            Diagnostic::warning(
                codes::LEEK_SCRIPT_SEMANTIC_LOSS,
                span,
                format!("the emitted LeekScript does not preserve {what}"),
            )
            .with_note(note.to_string()),
        );
    }

    /// The type annotation to write for a declaration of `what`, reporting
    /// whatever the round-trip drops.
    ///
    /// `None` means "write no annotation", which is what this backend did
    /// unconditionally before #154 — a declared type is a *coercion*
    /// upstream (`real r = 5` stores `5.0`), so erasing one silently changed
    /// the program's result.
    ///
    /// Annotations are only written when targeting v4. This repo's parser
    /// accepts `real x = 5` at every version — `looks_like_typed_var_decl`
    /// carries no version gate — but the official v1–v3 servers predate the
    /// type syntax, so emitting one there would be a guess about somebody
    /// else's compiler. At v1–v3 the type is dropped and reported instead.
    fn decl_annotation(&self, ty: Option<&Type>, span: Span, what: &str) -> Option<String> {
        let ty = ty?;
        if self.opts.version != Version::V4 {
            self.semantic_loss(
                span,
                &format!("the declared type of {what}"),
                &format!(
                    "`{}` is written back only when targeting v4; at {:?} the declaration \
                     is emitted untyped, so a value assigned to it is no longer coerced",
                    expr::type_str(ty),
                    self.opts.version
                ),
            );
            return None;
        }
        let rendered = expr::decl_type_str(ty);
        if rendered.is_none() {
            self.semantic_loss(
                span,
                &format!("the declared type of {what}"),
                &format!(
                    "`{}` has no official (non-experimental) LeekScript spelling, so the \
                     declaration is emitted untyped",
                    expr::type_str(ty)
                ),
            );
        }
        rendered
    }

    fn emit_file(&mut self) {
        // Globals whose `global x = …;` statement survives in the emitted
        // program: their `Def::Global` item would be a redeclaration.
        let declared = declared_global_defs(self.hir, self.opts);

        let mut first = true;
        for &item in &self.hir.items {
            let Some(def) = self.hir.defs.get(item.0 as usize) else {
                continue;
            };
            if !will_emit(def, self.opts) {
                continue;
            }
            if matches!(def, Def::Global(_)) && declared.contains(&item) {
                continue;
            }

            if !first {
                self.w.newline();
            }
            first = false;

            self.flush_leading(def.span());
            match def {
                Def::Function(f) => self.emit_function(item, f),
                Def::Class(c) => self.emit_class(c),
                Def::Global(g) => {
                    // The item form is the declaration; a later statement
                    // site must degrade to a plain assignment.
                    self.declared_globals.insert(item);
                    self.emit_global(g);
                }
                Def::Local(_) => {}
            }
        }

        for stmt in &self.hir.main {
            self.emit_stmt(stmt);
        }
        self.comments.flush_rest(&mut self.w);
    }

    // ---- declarations ----

    fn emit_function(&mut self, item: DefId, f: &Function) {
        self.w.token("function");
        self.w.space();
        let name = self.fn_name(item, &f.name);
        self.w.token(&name);
        self.emit_params(&f.params);
        self.emit_return_type(
            f.return_type.as_ref(),
            f.span,
            &format!("`{name}`'s return"),
        );
        self.w.space();
        if let Some(b) = &f.body {
            self.emit_block(b);
        } else {
            // Bodiless signature carrying a LeekScript directive body.
            self.w.token("{");
            self.w.newline();
            self.w.indent();
            if let Some(body) = directive_body(f) {
                for line in body.lines() {
                    self.w.token(line.trim());
                    self.w.newline();
                }
            }
            self.w.dedent();
            self.w.token("}");
        }
        self.w.newline();
    }

    fn emit_class(&mut self, c: &leek_hir::Class) {
        self.w.token("class");
        self.w.space();
        self.w.token(&c.name);
        if let Some(parent) = &c.parent {
            self.w.space();
            self.w.token("extends");
            self.w.space();
            self.w.token(parent);
        }
        self.w.space();
        self.w.token("{");
        self.w.newline();
        self.w.indent();
        for field in &c.fields {
            self.emit_field(field);
        }
        for ctor in &c.constructors {
            self.emit_method(ctor, true);
        }
        for m in &c.methods {
            self.emit_method(m, false);
        }
        self.w.dedent();
        self.w.token("}");
        self.w.newline();
    }

    fn emit_field(&mut self, f: &leek_hir::Field) {
        if f.is_static {
            self.w.token("static");
            self.w.space();
        }
        if f.is_final {
            self.w.token("final");
            self.w.space();
        }
        // Visibility keywords precede the declaration in LeekScript.
        match f.visibility {
            leek_hir::Visibility::Public => {}
            leek_hir::Visibility::Private => {
                self.w.token("private");
                self.w.space();
            }
            leek_hir::Visibility::Protected => {
                self.w.token("protected");
                self.w.space();
            }
        }
        // Class fields are bare names (optionally prefixed by the
        // modifiers above, and by the declared type) — never `var`.
        if let Some(ann) = self.decl_annotation(f.ty.as_ref(), f.span, &format!("`{}`", f.name)) {
            self.w.token(&ann);
            self.w.space();
        }
        self.w.token(&f.name);
        if let Some(init) = &f.init {
            self.w.space();
            self.w.token("=");
            self.w.space();
            self.emit_expr(init, 0);
        }
        self.w.token(";");
        self.w.newline();
    }

    fn emit_method(&mut self, m: &MethodDef, is_ctor: bool) {
        if m.is_static {
            self.w.token("static");
            self.w.space();
        }
        match m.visibility {
            leek_hir::Visibility::Public => {}
            leek_hir::Visibility::Private => {
                self.w.token("private");
                self.w.space();
            }
            leek_hir::Visibility::Protected => {
                self.w.token("protected");
                self.w.space();
            }
        }
        if is_ctor {
            self.w.token("constructor");
        } else {
            self.w.token(&m.name);
        }
        self.emit_params(&m.params);
        // A constructor takes no return type in the grammar.
        if !is_ctor {
            self.emit_return_type(
                m.return_type.as_ref(),
                m.span,
                &format!("`{}`'s return", m.name),
            );
        }
        self.w.space();
        if let Some(b) = &m.body {
            self.emit_block(b);
        } else {
            self.w.token("{");
            self.w.token("}");
        }
        self.w.newline();
    }

    fn emit_global(&mut self, g: &Global) {
        self.w.token("global");
        self.w.space();
        if let Some(ann) = self.decl_annotation(g.ty.as_ref(), g.span, &format!("`{}`", g.name)) {
            self.w.token(&ann);
            self.w.space();
        }
        self.w.token(&g.name);
        if let Some(init) = &g.init {
            self.w.space();
            self.w.token("=");
            self.w.space();
            self.emit_expr(init, 0);
        }
        self.w.token(";");
        self.w.newline();
    }

    fn emit_params(&mut self, params: &[Param]) {
        self.w.token("(");
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                self.w.token(",");
                self.w.space();
            }
            // `Param : '@'? ( Type '@'? )? Ident` — the type goes before
            // the by-ref marker (docs/grammar.md §5.2).
            if let Some(ann) = self.decl_annotation(p.ty.as_ref(), p.span, &format!("`{}`", p.name))
            {
                self.w.token(&ann);
                self.w.space();
            }
            if p.is_by_ref {
                self.w.token("@");
            }
            self.w.token(&p.name);
            if let Some(d) = &p.default {
                self.w.space();
                self.w.token("=");
                self.w.space();
                self.emit_expr(d, 0);
            }
        }
        self.w.token(")");
    }

    // ---- statements ----

    pub(crate) fn emit_block(&mut self, b: &Block) {
        self.w.token("{");
        self.w.newline();
        self.w.indent();
        for s in &b.stmts {
            self.emit_stmt(s);
        }
        self.flush_leading_pos(b.span);
        self.w.dedent();
        self.w.token("}");
    }

    /// Emit a statement as a braced body (always wrapping single
    /// statements in `{ }` for unambiguous, always-valid output).
    fn emit_braced(&mut self, body: &Stmt) {
        match body {
            Stmt::Block(b) => self.emit_block(b),
            other => {
                self.w.token("{");
                self.w.newline();
                self.w.indent();
                self.emit_stmt(other);
                self.w.dedent();
                self.w.token("}");
            }
        }
    }

    fn emit_stmt(&mut self, s: &Stmt) {
        if matches!(s, Stmt::Charge(_)) {
            return;
        }
        // A repeat `global x;` with no initializer has nothing left to say
        // once an earlier site declared it — writing the bare name would be
        // a pointless expression statement.
        if let Stmt::VarDecl(v) = s
            && v.is_global
            && v.init.is_none()
            && self.declared_globals.contains(&v.def)
        {
            return;
        }
        self.flush_leading(s.span());
        match s {
            Stmt::Charge(_) => {}
            Stmt::Expr(e) => {
                self.emit_expr(e, 0);
                self.semi();
            }
            Stmt::VarDecl(v) => {
                self.emit_vardecl(v);
                self.semi();
            }
            Stmt::Return(Some(e)) => {
                self.w.token("return");
                self.w.space();
                self.emit_expr(e, 0);
                self.semi();
            }
            Stmt::Return(None) => {
                self.w.token("return");
                self.semi();
            }
            Stmt::Break(_) => {
                self.w.token("break");
                self.semi();
            }
            Stmt::Continue(_) => {
                self.w.token("continue");
                self.semi();
            }
            Stmt::Block(b) => {
                self.emit_block(b);
                self.w.newline();
            }
            Stmt::If(i) => self.emit_if(i),
            Stmt::While(w) => {
                self.w.token("while");
                self.w.space();
                self.w.token("(");
                self.emit_expr(&w.cond, 0);
                self.w.token(")");
                self.w.space();
                self.emit_braced(&w.body);
                self.w.newline();
            }
            Stmt::DoWhile(d) => {
                self.w.token("do");
                self.w.space();
                self.emit_braced(&d.body);
                self.w.space();
                self.w.token("while");
                self.w.space();
                self.w.token("(");
                self.emit_expr(&d.cond, 0);
                self.w.token(")");
                self.semi();
            }
            Stmt::For(f) => {
                self.w.token("for");
                self.w.space();
                self.w.token("(");
                if let Some(init) = &f.init {
                    self.emit_for_init(init);
                }
                self.w.token(";");
                if let Some(c) = &f.cond {
                    self.w.space();
                    self.emit_expr(c, 0);
                }
                self.w.token(";");
                if let Some(st) = &f.step {
                    self.w.space();
                    self.emit_expr(st, 0);
                }
                self.w.token(")");
                self.w.space();
                self.emit_braced(&f.body);
                self.w.newline();
            }
            Stmt::Foreach(fe) => {
                self.w.token("for");
                self.w.space();
                self.w.token("(");
                if let Some(k) = &fe.key {
                    self.emit_foreach_bind(k);
                    self.w.space();
                    self.w.token(":");
                    self.w.space();
                }
                self.emit_foreach_bind(&fe.value);
                self.w.space();
                self.w.token("in");
                self.w.space();
                self.emit_expr(&fe.iter, 0);
                self.w.token(")");
                self.w.space();
                self.emit_braced(&fe.body);
                self.w.newline();
            }
            Stmt::Switch(sw) => {
                self.w.token("switch");
                self.w.space();
                self.w.token("(");
                self.emit_expr(&sw.discriminant, 0);
                self.w.token(")");
                self.w.space();
                self.w.token("{");
                self.w.newline();
                self.w.indent();
                for arm in &sw.arms {
                    if let Some(e) = &arm.case {
                        self.w.token("case");
                        self.w.space();
                        self.emit_expr(e, 0);
                        self.w.token(":");
                    } else {
                        self.w.token("default");
                        self.w.token(":");
                    }
                    self.w.newline();
                    self.w.indent();
                    for s in &arm.body {
                        self.emit_stmt(s);
                    }
                    self.w.dedent();
                }
                self.w.dedent();
                self.w.token("}");
                self.w.newline();
            }
            Stmt::Include(i) => {
                self.w.token("include");
                self.w.token("(");
                self.w
                    .token(&crate::emit::expr::string_lit(&i.path, self.opts.version));
                self.w.token(")");
                self.semi();
            }
            Stmt::Import(i) => {
                self.w.token("import");
                self.w.space();
                self.w.token(&i.path);
                self.semi();
            }
        }
    }

    fn emit_if(&mut self, i: &leek_hir::IfStmt) {
        self.w.token("if");
        self.w.space();
        self.w.token("(");
        self.emit_expr(&i.cond, 0);
        self.w.token(")");
        self.w.space();
        self.emit_braced(&i.then_branch);
        if let Some(els) = &i.else_branch {
            self.w.space();
            self.w.token("else");
            self.w.space();
            if let Stmt::If(nested) = &**els {
                self.flush_leading(nested.span);
                self.emit_if(nested);
            } else {
                self.emit_braced(els);
                self.w.newline();
            }
        } else {
            self.w.newline();
        }
    }

    fn emit_for_init(&mut self, s: &Stmt) {
        match s {
            Stmt::VarDecl(v) => self.emit_vardecl(v),
            Stmt::Expr(e) => self.emit_expr(e, 0),
            other => self.emit_stmt(other),
        }
    }

    fn emit_foreach_bind(&mut self, b: &ForeachBind) {
        if b.is_new {
            self.w.token("var");
            self.w.space();
        }
        if b.is_by_ref {
            self.w.token("@");
        }
        self.w.token(&b.name);
    }

    fn emit_vardecl(&mut self, v: &VarDecl) {
        // Only a *declaration* site may carry the type: the later sites of a
        // folded global are plain assignments, and prefixing one with a type
        // would declare a shadowing local.
        let mut declaring = true;
        if v.is_global {
            // One `global` keyword per global. HIR folds every declaration
            // site of a name onto one `DefId` and keeps them all as
            // statements; upstream hoists the declaration regardless of
            // position, so the sites after the first are assignments.
            declaring = self.declared_globals.insert(v.def);
            if declaring {
                self.w.token("global");
                self.w.space();
            }
        }
        // `Type Ident` replaces the `var` keyword — `real r = 5;`, not
        // `var real r = 5;` (docs/grammar.md §5.1).
        let annotation = if declaring {
            self.decl_annotation(v.ty.as_ref(), v.span, &format!("`{}`", v.name))
        } else {
            None
        };
        match &annotation {
            Some(ann) => {
                self.w.token(ann);
                self.w.space();
            }
            None if !v.is_global => {
                self.w.token("var");
                self.w.space();
            }
            None => {}
        }
        self.w.token(&v.name);
        if let Some(init) = &v.init {
            self.w.space();
            self.w.token("=");
            self.w.space();
            self.emit_expr(init, 0);
        }
    }

    /// Write `-> T` for a declared return type, reporting what is dropped.
    fn emit_return_type(&mut self, ty: Option<&Type>, span: Span, what: &str) {
        if let Some(ann) = self.decl_annotation(ty, span, what) {
            self.w.space();
            self.w.token("->");
            self.w.space();
            self.w.token(&ann);
        }
    }

    fn semi(&mut self) {
        self.w.token(";");
        self.w.newline();
    }

    // ---- helpers ----

    /// Resolve a top-level function's emitted name (post overload-rename).
    fn fn_name(&self, id: DefId, fallback: &str) -> String {
        self.names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| fallback.to_string())
    }

    fn flush_leading(&mut self, span: Span) {
        if span.source == self.opts.user_source {
            self.comments.flush_before(&mut self.w, span.start);
        }
    }

    fn flush_leading_pos(&mut self, span: Span) {
        if span.source == self.opts.user_source {
            self.comments.flush_before(&mut self.w, span.end);
        }
    }
}

/// The body text of a `leekscript` (or generic) backend directive, if any.
fn directive_body(f: &Function) -> Option<&str> {
    f.backend_directives
        .iter()
        .find(|(backend, _)| backend == "leekscript" || backend.is_empty())
        .map(|(_, body)| body.as_str())
}

/// True when a span comes from a merged library/prelude header.
///
/// Origin is read from [`Options::prelude_sources`] rather than inferred
/// from `user_source`: an included file carries its own `SourceId`, and
/// its definitions belong in the output just like the entry's own.
pub(crate) fn is_prelude_span(span: Span, opts: &Options) -> bool {
    span.source != Span::SYNTHETIC_SOURCE && opts.prelude_sources.contains(&span.source)
}

/// The `DefId`s of globals that a `global x = …;` statement in the emitted
/// program already declares.
///
/// HIR keeps both forms: `declare_global` pushes a `Def::Global` item with
/// no initializer, and the declaration site stays in place as a
/// `Stmt::VarDecl { is_global: true }`. Emitting both yields a
/// redeclaration, which the official compiler rejects — so the item form is
/// skipped for every global a surviving statement covers. A `Def::Global`
/// with no such statement left (the shape `propagate_const_globals` leaves
/// behind) still emits `global x;`, so no declaration is ever lost.
///
/// This answers the *item vs. statement* question only. When a global has
/// several declaration sites — `global g = 1; global g = 2;`, or two
/// included files that both declare `CFG` — every site is a separate
/// statement sharing one `DefId`, and [`Emitter::declared_globals`] keeps
/// the `global` keyword on the first of them. The two mechanisms are one
/// policy: exactly one declaration per global.
///
/// Lambda bodies are leaves in [`leek_hir::visit::walk_stmt_child_stmts`],
/// so a global declared only inside a lambda is not counted and keeps its
/// item — the safe direction: over-counting would drop the sole
/// file-scope declaration.
fn declared_global_defs(hir: &HirFile, opts: &Options) -> BTreeSet<DefId> {
    fn collect(s: &Stmt, out: &mut BTreeSet<DefId>) {
        if let Stmt::VarDecl(v) = s
            && v.is_global
        {
            out.insert(v.def);
        }
        leek_hir::visit::walk_stmt_child_stmts(s, &mut |child| collect(child, out));
    }

    let mut out = BTreeSet::new();
    for stmt in &hir.main {
        collect(stmt, &mut out);
    }
    // Bodies of emitted definitions run too, so a `global` declared inside
    // one is just as real as one in the main block.
    for &item in &hir.items {
        let Some(def) = hir.defs.get(item.0 as usize) else {
            continue;
        };
        if !will_emit(def, opts) {
            continue;
        }
        let bodies: Vec<&Block> = match def {
            Def::Function(f) => f.body.iter().collect(),
            Def::Class(c) => c
                .methods
                .iter()
                .chain(&c.constructors)
                .filter_map(|m: &MethodDef| m.body.as_ref())
                .collect(),
            Def::Global(_) | Def::Local(_) => Vec::new(),
        };
        for body in bodies {
            for stmt in &body.stmts {
                collect(stmt, &mut out);
            }
        }
    }
    out
}

/// Whether a top-level definition will actually be emitted (shared by the
/// emitter and the overload-rename pass so renaming ignores dropped defs).
pub(crate) fn will_emit(def: &Def, opts: &Options) -> bool {
    if opts.drop_prelude_defs && is_prelude_span(def.span(), opts) {
        return false;
    }
    match def {
        // A bodiless signature with no usable LeekScript body is a
        // builtin/extern declaration — its calls emit the bare name.
        Def::Function(f) => f.body.is_some() || directive_body(f).is_some(),
        Def::Class(_) | Def::Global(_) => true,
        Def::Local(_) => false,
    }
}
