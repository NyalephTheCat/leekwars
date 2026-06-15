//! HIR → LeekScript source emission.

mod expr;

use leek_hir::{
    Block, Def, DefId, ForeachBind, Function, Global, HirFile, MethodDef, Param, Stmt, VarDecl,
};
use leek_span::Span;

use crate::comments::Comments;
use crate::options::Options;
use crate::rename::{self, RenameMap};
use crate::writer::LsWriter;

/// Result of [`emit`].
pub struct EmittedLeekScript {
    /// The generated official-LeekScript source.
    pub source: String,
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
    };
    em.emit_file();
    EmittedLeekScript {
        source: em.w.into_string(),
    }
}

pub(crate) struct Emitter<'a> {
    pub(crate) opts: &'a Options,
    pub(crate) hir: &'a HirFile,
    pub(crate) w: LsWriter,
    pub(crate) names: RenameMap,
    pub(crate) comments: Comments,
}

impl Emitter<'_> {
    fn emit_file(&mut self) {
        let mut first = true;
        for &item in &self.hir.items {
            let Some(def) = self.hir.defs.get(item.0 as usize) else {
                continue;
            };
            if !will_emit(def, self.opts) {
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
                Def::Global(g) => self.emit_global(g),
                Def::Local(_) => {}
            }
        }

        for stmt in &self.hir.main {
            if !first {
                first = false;
            }
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
        // modifiers above) — never `var`.
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
                self.w.token(&crate::emit::expr::string_lit(&i.path));
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
        if v.is_global {
            self.w.token("global");
        } else {
            self.w.token("var");
        }
        self.w.space();
        self.w.token(&v.name);
        if let Some(init) = &v.init {
            self.w.space();
            self.w.token("=");
            self.w.space();
            self.emit_expr(init, 0);
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

/// True when a span comes from a merged prelude / included file rather
/// than the user's own source.
pub(crate) fn is_prelude_span(span: Span, user_source: leek_span::SourceId) -> bool {
    span.source != user_source && span.source != Span::SYNTHETIC_SOURCE
}

/// Whether a top-level definition will actually be emitted (shared by the
/// emitter and the overload-rename pass so renaming ignores dropped defs).
pub(crate) fn will_emit(def: &Def, opts: &Options) -> bool {
    if opts.drop_prelude_defs && is_prelude_span(def.span(), opts.user_source) {
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
