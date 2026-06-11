use std::fmt::Write as _;

use leek_hir::{Field, Function, MethodDef};

use super::lambda::captured_by_nested_lambda_stmts;
use super::{ends_with_return, java_type_for, sanitize_ident};
use crate::mangle;

impl<'a> super::Emitter<'a> {
    /// The parameter name to declare in a Java method signature. It must match
    /// how the *body* refers to that parameter:
    /// - exact mode declares `p_x` and rebinds `var u_x = p_x;`, and the body
    ///   names locals `u_x`;
    /// - clean mode has no rebind, so the signature must use the same
    ///   [`mangle::local`] name the body uses (which drops the `u_` prefix for
    ///   ordinary identifiers).
    fn sig_param_name(&self, name: &str) -> String {
        if self.opts.is_clean() {
            mangle::local(self.opts, name)
        } else {
            format!("p_{}", sanitize_ident(name))
        }
    }

    pub(crate) fn emit_function(&mut self, f: &Function) {
        let name = mangle::function(self.opts, &f.name);
        // Exact mode declares params `p_x` then rebinds `var u_x = p_x;` so the
        // body can refer to them as `u_x` (like other locals). Clean mode has no
        // rebind layer — the signature declares the body's own name directly.
        let exact = !self.opts.is_clean();
        let v1 = matches!(self.opts.version, leek_syntax::Version::V1);
        // At v1 args are passed by value, so a `p_x` slot + a copy/box rebind is
        // needed for every param (a `@x` aliases instead of copying). At v2+
        // only exact mode rebinds (`var u_x = p_x`).
        let params = f
            .params
            .iter()
            .map(|p| {
                let n = if exact || v1 {
                    format!("p_{}", sanitize_ident(&p.name))
                } else {
                    self.sig_param_name(&p.name)
                };
                format!("Object {n}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let rebinds = f.params.iter().fold(String::new(), |mut acc, p| {
            let safe = sanitize_ident(&p.name);
            let body = mangle::local(self.opts, &p.name);
            let ai = self.ai_this();
            if self.is_v1_ref_param(p) {
                // `@x` at v1 → bind to a runtime `Box` (alias a passed box, else
                // box the value); body ops route through `Box` methods so
                // mutations propagate to the caller.
                let _ = write!(
                    acc,
                    "Box {body} = p_{safe} instanceof Box ? (Box) p_{safe} : new Box({ai}, load(p_{safe}));"
                );
                self.ref_boxes.borrow_mut().insert(p.def);
            } else if v1 {
                if self.ref_boxes.borrow().contains(&p.def) {
                    // Plain v1 param passed onward at a `@` position → bind
                    // through a runtime `Box` so the inner callee can alias
                    // it (upstream boxes every v1 param: `var u_a = new
                    // Box(AI.this, copy(p_a))`; the ctor's 1 op replaces this
                    // param's share of `v1_param_box_ops`).
                    let _ = write!(acc, "Box {body} = new Box({ai}, copy(p_{safe}));");
                } else {
                    // v1 plain param: passed by value → deep-copy the arg so a
                    // mutation inside doesn't touch the caller's array/map.
                    // `load(...)` first: a dynamically-dispatched call site
                    // (executeArrayAccess) passes the caller's Box and
                    // upstream `clone` passes Boxes through untouched — the
                    // unwrap+clone there happens in the Box-ctor rebind we
                    // don't emit. Charge-identical (load is free; the content
                    // clone is the same either way).
                    let _ = write!(acc, "var {body} = copy(load(p_{safe}));");
                }
            } else if exact {
                if p.is_by_ref {
                    // `@x` at v2+ → alias a passed box, else box a copy
                    // (upstream: `u_x = p_x instanceof Box ? p_x : new
                    // Box(AI.this, LeekOperations.clone(p_x))`).
                    let _ = write!(
                        acc,
                        "Box {body} = p_{safe} instanceof Box ? (Box) p_{safe} : new Box({ai}, copy(p_{safe}));"
                    );
                    self.ref_boxes.borrow_mut().insert(p.def);
                } else if f
                    .body
                    .as_ref()
                    .is_some_and(|b| captured_by_nested_lambda_stmts(&b.stmts, p.def))
                {
                    // Param captured by a nested closure → bind through a
                    // runtime `Box` so writes propagate into the closure;
                    // the 2-arg ctor charges the same 1 op as upstream's
                    // `final var u_x = new Box<>(AI.this, p_x)`.
                    let _ = write!(acc, "final Box {body} = new Box({ai}, p_{safe});");
                    self.ref_boxes.borrow_mut().insert(p.def);
                } else {
                    let _ = write!(acc, "var u_{safe} = p_{safe};");
                }
            }
            acc
        });
        self.writer.add_line(&format!(
            "private Object {name}({params}) throws LeekRunException {{{rebinds}"
        ));
        if self.opts.is_clean() {
            self.writer.push_indent();
        }
        self.in_function = true;
        if let Some(body) = &f.body {
            // Function-body entry tick (matches `FunctionBlock.writeJavaCode`'s
            // `writer.addCounter(1)`). Concatenated with the first body line.
            if self.opts.emit_ops {
                self.writer.add_code("ops(1);");
                let pb = self.v1_param_box_ops(&f.params);
                if !pb.is_empty() {
                    self.writer.add_code(&pb);
                }
            }
            self.emit_stmts(&body.stmts);
            if !ends_with_return(&body.stmts, self.opts.emit_ops) {
                self.writer.add_line("return null;");
            }
        } else {
            self.writer.add_line("return null;");
        }
        self.in_function = false;
        if self.opts.is_clean() {
            self.writer.pop_indent();
        }
        self.writer.add_line("}");

        // Per-arity overloads for default parameter values. Leek
        // accepts `function f(x = 5) { … } f()` — Java doesn't have
        // default args, so for each call arity in [first_default,
        // full_arity) emit a forwarding overload that fills the
        // missing params with their default expressions.
        let first_default = f.params.iter().position(|p| p.default.is_some());
        if let Some(min_arity) = first_default {
            let full_arity = f.params.len();
            for arity in min_arity..full_arity {
                self.emit_default_overload(f, &name, arity);
            }
        }
    }

    pub(crate) fn emit_default_overload(&mut self, f: &Function, _name: &str, arity: usize) {
        // Upstream duplicates the whole body per arity, binding each omitted
        // param as a *local* initialized to its default expression (`var u_a
        // = new Box(AI.this, <default>); …` — no `copy(p_a)` re-bind, since
        // the default value is fresh). Delegating to the full-arity overload
        // instead routed the fresh default through the param path's v1
        // deep-copy, over-charging by a full clone (OPS_DRIFT L3153/L3157).
        // The decl shape is charge-identical to upstream's: `ops(<default>,
        // cost+1)` statically vs. Box-ctor 1 + `ops(cost);` runtime.
        let mut g = f.clone();
        g.params.truncate(arity);
        // Clear remaining defaults so `emit_function` doesn't recursively
        // re-emit the lower-arity overloads (the outer loop already covers
        // every arity — leaving them produced duplicate methods).
        for p in &mut g.params {
            p.default = None;
        }
        if let Some(body) = &mut g.body {
            // Mark the spliced defs so `emit_var_decl` knows to drop the +1
            // declaration tick at v2+ (upstream binds an omitted param with
            // only the default expression's own cost).
            for p in &f.params[arity..] {
                self.synthetic_default_decls.insert(p.def);
            }
            let decls: Vec<leek_hir::Stmt> = f.params[arity..]
                .iter()
                .map(|p| {
                    leek_hir::Stmt::VarDecl(leek_hir::VarDecl {
                        def: p.def,
                        name: p.name.clone(),
                        ty: p.ty.clone(),
                        init: p.default.clone(),
                        is_global: false,
                        span: p.span,
                    })
                })
                .collect();
            body.stmts.splice(0..0, decls);
        }
        self.emit_function(&g);
    }

    /// Emit a user class as a `NativeObjectLeekValue` subclass — real public
    /// Java fields, a field-defaulting constructor, the user constructor(s) as
    /// `init(...)`, and methods as `u_<name>(...)`. Mirrors the upstream
    /// `ClassDeclarationInstruction` shape. Standalone classes only (no parent);
    /// the AI-level `ClassLeekValue` field, `new_<class>` helper, and method
    /// registration are emitted by [`Self::emit_class_ai_member`] /
    /// [`Self::emit_class_registration`].
    pub(crate) fn emit_class(&mut self, c: &'a leek_hir::Class) {
        let name = mangle::class_name(self.opts, &c.name);
        let extends = match &c.parent {
            Some(p) => format!(" extends {}", mangle::class_name(self.opts, p)),
            None => " extends NativeObjectLeekValue".into(),
        };
        self.writer
            .add_line(&format!("public class {name}{extends} {{"));
        self.writer.push_indent();
        let prev = self.current_class.replace(Some(c));

        let inst_fields: Vec<&Field> = c.fields.iter().filter(|f| !f.is_static).collect();

        // Real public Java fields (no inline init — set in the constructor). A
        // `final` field carries the `@Final` annotation the runtime reads
        // reflectively to reject a later write (`final a = 12; a['a'] = 15`
        // keeps 12) — same mechanism as `@Private` on methods.
        for f in &inst_fields {
            let ty = java_type_for(f.ty.as_ref());
            let fin = if f.is_final { "@Final " } else { "" };
            self.writer
                .add_line(&format!("{fin}public {ty} {};", f.name));
        }

        // Field-default constructor: reserve RAM, then init each field.
        self.writer
            .add_line(&format!("public {name}() throws LeekRunException {{"));
        self.writer.push_indent();
        self.writer
            .add_line(&format!("allocateRAM(this, {});", 2 * inst_fields.len()));
        for f in &inst_fields {
            if let Some(init) = &f.init {
                // `coerce_decl` (not `coerce_field_write`) so a nullable scalar
                // field coerces too: `real? a = 12` stores `12.0` via
                // `realOrNull`. The field's Java type is `Object` for nullables,
                // so the boxed `Double`/`Long` drops in.
                let v = Self::coerce_decl(f.ty.as_ref(), self.expr_to_string(init));
                self.writer.add_line(&format!("{} = {v};", f.name));
            }
        }
        self.writer.pop_indent();
        self.writer.add_line("}");

        // Clone constructor `u_C(u_C o, int level)` — `LeekOperations.clone`
        // (behind the `clone(...)` builtin) reflectively invokes it to deep-copy
        // an instance. Each field is shallow-copied at `level == 1`, else
        // recursively `copy`d. A subclass chains to the parent's clone ctor
        // first. Without this, `clone(obj)` returns null.
        self.writer.add_line(&format!(
            "public {name}({name} o, int level) throws LeekRunException {{"
        ));
        self.writer.push_indent();
        if c.parent.is_some() {
            self.writer.add_line("super(o, level);");
        }
        for f in &inst_fields {
            // Cast the deep-copy branch to the field's Java type so it matches
            // the shallow branch (`o.f`): a typed field is a primitive (`long`),
            // an untyped one is `Object`. Both ternary arms must agree, and the
            // result must fit the field slot.
            let jty = java_type_for(f.ty.as_ref());
            self.writer.add_line(&format!(
                "this.{f} = level == 1 ? o.{f} : ({jty}) copy(o.{f}, level - 1);",
                f = f.name
            ));
        }
        self.writer.pop_indent();
        self.writer.add_line("}");

        // User constructor(s) -> `init(params)`, run by `execute(...)` after the
        // field-default constructor. No constructor -> a no-op `init()`.
        let ctors: Vec<&MethodDef> = c.constructors.iter().filter(|m| !m.is_static).collect();
        if ctors.is_empty() {
            self.writer
                .add_line("public Object init() throws LeekRunException { return null; }");
        } else {
            for ctor in ctors {
                self.emit_class_init(ctor);
            }
        }

        for m in c.methods.iter().filter(|m| !m.is_static) {
            self.emit_class_method(m);
        }

        self.current_class.set(prev);
        self.writer.pop_indent();
        self.writer.add_line("}");
    }

    /// A class method as `public Object u_<name>(Object p…)`. The body reads
    /// `this.field` as a direct field access (see `write_expr`'s `Field` arm).
    fn emit_class_method(&mut self, m: &MethodDef) {
        let jname = format!("u_{}", sanitize_ident(&m.name));
        let params = m
            .params
            .iter()
            .map(|p| format!("Object {}", mangle::local(self.opts, &p.name)))
            .collect::<Vec<_>>()
            .join(", ");
        // A `@Private`/`@Protected` annotation on the Java method — the runtime
        // visibility check (`callObjectAccess` with a `null` calling class) reads
        // it reflectively to deny access (e.g. a `private` method called from top
        // level returns null). The `addMethod` `AccessLevel` alone isn't consulted
        // for this; the annotation is. Public methods get no annotation.
        self.writer.add_line(&format!(
            "{}public Object {jname}({params}) throws LeekRunException {{",
            visibility_annotation(m.visibility)
        ));
        self.writer.push_indent();
        if let Some(body) = &m.body {
            self.emit_stmts(&body.stmts);
            if !ends_with_return(&body.stmts, self.opts.emit_ops) {
                self.writer.add_line("return null;");
            }
        } else {
            self.writer.add_line("return null;");
        }
        self.writer.pop_indent();
        self.writer.add_line("}");

        // Default method parameters — same per-arity forwarding as constructors
        // (`m(x = 2)` callable as `o.m()`). `callObjectAccess` resolves `u_m` by
        // arg count via reflection, so each callable arity needs its own Java
        // overload; registration (`emit_class_registration`) likewise adds one
        // `addMethod` per arity.
        if let Some(min_arity) = m.params.iter().position(|p| p.default.is_some()) {
            for arity in min_arity..m.params.len() {
                self.emit_method_default_overload(m, &jname, arity);
            }
        }
    }

    /// A forwarding `u_<m>` overload for a method call with `arity` explicit args
    /// (fills the missing trailing params with their defaults).
    fn emit_method_default_overload(&mut self, m: &MethodDef, jname: &str, arity: usize) {
        let body = self.default_overload_body(m, jname, arity);
        self.writer
            .add_line(&format!("{}{body}", visibility_annotation(m.visibility)));
    }

    /// The Java for a per-arity forwarding overload that fills the missing
    /// trailing params with their defaults. Missing params are bound as LOCALS
    /// (in order) so a default can reference an earlier defaulted param
    /// (`m(x, y = x, z = y)`); a flat inline forward would reference an
    /// undeclared `u_y`.
    fn default_overload_body(&self, m: &MethodDef, jname: &str, arity: usize) -> String {
        let params = m.params[..arity]
            .iter()
            .map(|p| format!("Object {}", mangle::local(self.opts, &p.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut binds = String::new();
        for p in &m.params[arity..] {
            let name = mangle::local(self.opts, &p.name);
            let val = match &p.default {
                Some(d) => self.expr_to_string(d),
                None => "null".into(),
            };
            let _ = write!(binds, " Object {name} = {val};");
        }
        let call_args = m
            .params
            .iter()
            .map(|p| mangle::local(self.opts, &p.name))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "public Object {jname}({params}) throws LeekRunException {{{binds} return {jname}({call_args}); }}"
        )
    }

    /// A user constructor body, emitted as `init(params)` (the runtime
    /// `execute(...)` calls it after the field-default constructor).
    fn emit_class_init(&mut self, m: &MethodDef) {
        let params = m
            .params
            .iter()
            .map(|p| format!("Object {}", mangle::local(self.opts, &p.name)))
            .collect::<Vec<_>>()
            .join(", ");
        self.writer.add_line(&format!(
            "public Object init({params}) throws LeekRunException {{"
        ));
        self.writer.push_indent();
        if let Some(body) = &m.body {
            self.emit_stmts(&body.stmts);
        }
        self.writer.add_line("return null;");
        self.writer.pop_indent();
        self.writer.add_line("}");

        // Default constructor parameters: `constructor(x = 2)` must be callable
        // as `A()`. `execute(...)` dispatches to the `init` overload matching the
        // arg count, so emit one forwarding `init` per call arity in
        // [first_default, full_arity) that fills the missing trailing params with
        // their default expressions (which may reference earlier params, e.g.
        // `constructor(x, y = x)`). These must be `public` like the full `init`.
        if let Some(min_arity) = m.params.iter().position(|p| p.default.is_some()) {
            for arity in min_arity..m.params.len() {
                self.emit_init_default_overload(m, arity);
            }
        }
    }

    /// A forwarding `init` overload for a constructor call with `arity` explicit
    /// args (chained defaults bound as locals — see `default_overload_body`).
    fn emit_init_default_overload(&mut self, m: &MethodDef, arity: usize) {
        let body = self.default_overload_body(m, "init", arity);
        self.writer.add_line(&body);
    }

    /// AI-level members for a class: the `ClassLeekValue` handle field and the
    /// `new_<class>(args)` construction helper. Emitted alongside the AI's
    /// globals (after the inner classes).
    pub(crate) fn emit_class_ai_member(&mut self, c: &leek_hir::Class) {
        let name = mangle::class_name(self.opts, &c.name);
        let parent = match &c.parent {
            Some(p) => mangle::class_name(self.opts, p),
            None => "null".into(),
        };
        self.writer.add_line(&format!(
            "public ClassLeekValue {name} = new ClassLeekValue(this, \"{}\", {parent}, {name}.class);",
            c.name
        ));
        self.writer.add_line(&format!(
            "public {name} new_{name}(Object... args) throws LeekRunException {{ return ({name}) execute({name}, args); }}"
        ));
    }

    /// Register a class's methods on its `ClassLeekValue` — emitted in the AI
    /// constructor so `callObjectAccess` / dynamic dispatch can find them.
    pub(crate) fn emit_class_registration(&mut self, c: &leek_hir::Class) {
        let name = mangle::class_name(self.opts, &c.name);
        if let Some(p) = &c.parent {
            // Wire inheritance: the parent's methods/fields are inherited via
            // the `ClassLeekValue` chain (Java `extends` handles the instance
            // side).
            self.writer.add_line(&format!(
                "{name}.setParent({});",
                mangle::class_name(self.opts, p)
            ));
        }
        self.writer.add_line(&format!(
            "{name}.initFields = new FunctionLeekValue(0) {{public Object run(AI ai, Object u_this, Object... values) throws LeekRunException {{ return null; }}}};"
        ));
        for m in c.methods.iter().filter(|m| !m.is_static) {
            let jname = format!("u_{}", sanitize_ident(&m.name));
            let full = m.params.len();
            let access = access_level(m.visibility);
            // Register one `addMethod` per callable arity: the full arity, plus a
            // shorter one for each leading default param (`m(x = 2)` is callable
            // as `m()` and `m(2)`), each dispatching to the matching `u_m`
            // overload (`emit_method_default_overload`).
            let min = m
                .params
                .iter()
                .position(|p| p.default.is_some())
                .unwrap_or(full);
            for arity in min..=full {
                let call_args = (0..arity)
                    .map(|i| format!("(Object) args[{i}]"))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.writer.add_line(&format!(
                    "{name}.addMethod(\"{m}\", {arity}, new FunctionLeekValue(0) {{ public Object run(AI ai, Object thiz, Object... args) throws LeekRunException {{ return (({name}) thiz).{jname}({call_args}); }}}}, AccessLevel.{access});",
                    m = m.name
                ));
            }
            self.writer
                .add_line(&format!("{name}.addGenericMethod(\"{}\");", m.name));
        }
        // Static methods register separately and dispatch to the AI-level
        // `<class>_<method>_<arity>` bodies (see `emit_class_static_members`).
        for m in c.methods.iter().filter(|m| m.is_static) {
            let arity = m.params.len();
            let access = access_level(m.visibility);
            let body = format!("{name}_{}_{arity}", sanitize_ident(&m.name));
            let call_args = (0..arity)
                .map(|i| format!("(Object) args[{i}]"))
                .collect::<Vec<_>>()
                .join(", ");
            self.writer.add_line(&format!(
                "{name}.addStaticMethod(\"{m}\", {arity}, new FunctionLeekValue(1) {{ public Object run(AI ai, Object thiz, Object... args) throws LeekRunException {{ return {body}({call_args}); }}}}, AccessLevel.{access});",
                m = m.name
            ));
            self.writer
                .add_line(&format!("{name}.addGenericStaticMethod(\"{}\");", m.name));
        }
    }

    /// AI-level static members: the static-method bodies (`<class>_<m>_<n>(…)`)
    /// and the `createStaticClass_<C>` / `initClass_<C>` hooks (`addStaticField`
    /// / `initField`) that `staticInit` runs. Static fields live on the
    /// `ClassLeekValue`, not as Java fields.
    pub(crate) fn emit_class_static_members(&mut self, c: &'a leek_hir::Class) {
        let name = mangle::class_name(self.opts, &c.name);
        let prev = self.current_class.replace(Some(c));
        for m in c.methods.iter().filter(|m| m.is_static) {
            let jname = format!("{name}_{}_{}", sanitize_ident(&m.name), m.params.len());
            let params = m
                .params
                .iter()
                .map(|p| format!("Object {}", mangle::local(self.opts, &p.name)))
                .collect::<Vec<_>>()
                .join(", ");
            self.writer.add_line(&format!(
                "private final Object {jname}({params}) throws LeekRunException {{"
            ));
            self.writer.push_indent();
            if let Some(body) = &m.body {
                self.emit_stmts(&body.stmts);
                if !ends_with_return(&body.stmts, self.opts.emit_ops) {
                    self.writer.add_line("return null;");
                }
            } else {
                self.writer.add_line("return null;");
            }
            self.writer.pop_indent();
            self.writer.add_line("}");

            // Per-arity forwarders for default params — `A.m(4)` with
            // `static m(x=5,y=7,z=10)` dispatches to `<class>_m_1`, which fills
            // the rest. Defaults bound as locals so they can chain.
            let full = m.params.len();
            if let Some(min) = m.params.iter().position(|p| p.default.is_some()) {
                for arity in min..full {
                    let oname = format!("{name}_{}_{arity}", sanitize_ident(&m.name));
                    let oparams = m.params[..arity]
                        .iter()
                        .map(|p| format!("Object {}", mangle::local(self.opts, &p.name)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let mut binds = String::new();
                    for p in &m.params[arity..] {
                        let pn = mangle::local(self.opts, &p.name);
                        let val = p
                            .default
                            .as_ref()
                            .map_or("null".into(), |d| self.expr_to_string(d));
                        let _ = write!(binds, " Object {pn} = {val};");
                    }
                    let all = m
                        .params
                        .iter()
                        .map(|p| mangle::local(self.opts, &p.name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.writer.add_line(&format!(
                        "private final Object {oname}({oparams}) throws LeekRunException {{{binds} return {jname}({all}); }}"
                    ));
                }
            }
        }

        let static_fields: Vec<&Field> = c.fields.iter().filter(|f| f.is_static).collect();
        // `createStaticClass_<C>`: declare each static field on the ClassLeekValue.
        self.writer.add_line(&format!(
            "private void createStaticClass_{}() throws LeekRunException {{",
            c.name
        ));
        self.writer.push_indent();
        for f in &static_fields {
            let access = access_level(f.visibility);
            self.writer.add_line(&format!(
                "{name}.addStaticField(this, \"{}\", null, AccessLevel.{access}, {});",
                f.name, f.is_final
            ));
        }
        self.writer.pop_indent();
        self.writer.add_line("}");
        // `initClass_<C>`: set each static field's value.
        self.writer.add_line(&format!(
            "private void initClass_{}() throws LeekRunException {{",
            c.name
        ));
        self.writer.push_indent();
        for f in &static_fields {
            if let Some(init) = &f.init {
                // Coerce to the declared scalar type like instance fields
                // (`static real? a = 12` → `12.0`).
                let v = Self::coerce_decl(f.ty.as_ref(), self.expr_to_string(init));
                self.writer
                    .add_line(&format!("{name}.initField(\"{}\", {v});", f.name));
            }
        }
        self.writer.pop_indent();
        self.writer.add_line("}");
        self.current_class.set(prev);
    }
}

/// Java annotation prefix (`@Private `/`@Protected `/empty) for a member's
/// visibility — read reflectively by the runtime visibility check.
fn visibility_annotation(v: leek_hir::Visibility) -> &'static str {
    match v {
        leek_hir::Visibility::Public => "",
        leek_hir::Visibility::Private => "@Private ",
        leek_hir::Visibility::Protected => "@Protected ",
    }
}

/// `AccessLevel` enum constant for a member visibility.
fn access_level(v: leek_hir::Visibility) -> &'static str {
    match v {
        leek_hir::Visibility::Public => "PUBLIC",
        leek_hir::Visibility::Private => "PRIVATE",
        leek_hir::Visibility::Protected => "PROTECTED",
    }
}
