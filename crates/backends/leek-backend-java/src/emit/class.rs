use std::fmt::Write as _;

use leek_hir::{Field, Function, MethodDef};

use super::{ends_with_return, java_type_for, sanitize_ident};
use crate::mangle;
impl super::Emitter<'_> {
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
        let rebind = !self.opts.is_clean();
        let params = f
            .params
            .iter()
            .map(|p| format!("Object {}", self.sig_param_name(&p.name)))
            .collect::<Vec<_>>()
            .join(", ");
        if rebind {
            // Header + rebinding sequence on the same Java line, to
            // match the reference's `private Object f_X(Object p_n) ... {var u_n = p_n;` shape.
            let rebinds = f.params.iter().fold(String::new(), |mut acc, p| {
                let safe = sanitize_ident(&p.name);
                let _ = write!(acc, "var u_{safe} = p_{safe};");
                acc
            });
            self.writer.add_line(&format!(
                "private Object {name}({params}) throws LeekRunException {{{rebinds}"
            ));
        } else {
            self.writer.add_line(&format!(
                "private Object {name}({params}) throws LeekRunException {{"
            ));
            if self.opts.is_clean() {
                self.writer.push_indent();
            }
        }
        self.in_function = true;
        if let Some(body) = &f.body {
            // Function-body entry tick (matches `FunctionBlock.writeJavaCode`'s
            // `writer.addCounter(1)`). Concatenated with the first body line.
            if self.opts.emit_ops {
                self.writer.add_code("ops(1);");
            }
            self.emit_stmts(&body.stmts);
            if !ends_with_return(&body.stmts) {
                self.writer.add_line("return null;");
            }
        } else {
            self.writer.add_line("return null;");
        }
        self.in_function = false;
        if !rebind && self.opts.is_clean() {
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

    pub(crate) fn emit_default_overload(&mut self, f: &Function, name: &str, arity: usize) {
        let params = f.params[..arity]
            .iter()
            .map(|p| format!("Object {}", self.sig_param_name(&p.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let call_args = f
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if i < arity {
                    self.sig_param_name(&p.name)
                } else {
                    match &p.default {
                        Some(d) => self.expr_to_string(d),
                        // Earlier params without defaults shouldn't
                        // appear past `arity` (Leek convention puts
                        // defaults at the tail). Emit `null` so the
                        // method at least compiles.
                        None => "null".into(),
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.writer.add_line(&format!(
            "private Object {name}({params}) throws LeekRunException {{ return {name}({call_args}); }}"
        ));
    }

    pub(crate) fn emit_class(&mut self, c: &leek_hir::Class) {
        let name = mangle::class_name(self.opts, &c.name);
        let extends = c.parent.as_ref().map_or_else(
            || " extends ObjectLeekValue".into(),
            |p| format!(" extends {}", mangle::class_name(self.opts, p)),
        );
        self.writer
            .add_line(&format!("private class {name}{extends} {{"));
        self.writer.push_indent();

        for field in &c.fields {
            self.emit_field(field);
        }

        // Explicit Java no-arg constructor — without it, javac auto-
        // generates `<name>() { super(); }` which fails because
        // ObjectLeekValue has no no-arg constructor. Calling the
        // `(AI, String[], Object[])` overload with empty arrays
        // satisfies the superclass contract; the user's
        // `m_constructor` runs separately (out of scope for this
        // slice — `new u_X()` still bypasses it).
        let outer = self.opts.class_name();
        self.writer.add_line(&format!(
            "public {name}() throws LeekRunException {{ super({outer}.this, new String[0], new Object[0]); }}"
        ));

        for ctor in &c.constructors {
            self.emit_method(ctor, /* is_constructor */ true);
        }
        for m in &c.methods {
            self.emit_method(m, false);
        }

        self.writer.pop_indent();
        self.writer.add_line("}");
    }

    pub(crate) fn emit_field(&mut self, f: &Field) {
        let static_ = if f.is_static { "static " } else { "" };
        let java_name = if f.is_static {
            mangle::static_field(self.opts, &f.name)
        } else {
            f.name.clone()
        };
        let ty = java_type_for(f.ty.as_ref());
        let init = match &f.init {
            Some(e) => format!(" = {}", self.expr_to_string(e)),
            None => String::new(),
        };
        self.writer
            .add_line(&format!("{static_}private {ty} {java_name}{init};"));
    }

    pub(crate) fn emit_method(&mut self, m: &MethodDef, is_constructor: bool) {
        let static_ = if m.is_static { "static " } else { "" };
        let name = if is_constructor {
            // Java constructors take the class name as their identifier.
            // Use the unmangled name in clean mode is unsafe (the class
            // may have been suffixed); always use mangled here.
            // The simple-name form is required by Java syntax.
            // We rely on the surrounding class name match.
            // Caller emits inside `private class u_C { ... }` so
            // constructor name == the simple class name.
            // Cannot just substitute; thread the class name through if needed.
            return self.emit_constructor(m);
        } else {
            mangle::method(self.opts, &m.name)
        };
        let params = m
            .params
            .iter()
            .map(|p| format!("Object {}", mangle::local(self.opts, &p.name)))
            .collect::<Vec<_>>()
            .join(", ");
        self.writer.add_line(&format!(
            "{static_}public Object {name}({params}) throws LeekRunException {{"
        ));
        self.writer.push_indent();
        if let Some(body) = &m.body {
            self.emit_stmts(&body.stmts);
            if !ends_with_return(&body.stmts) {
                self.writer.add_line("return null;");
            }
        } else {
            self.writer.add_line("return null;");
        }
        self.writer.pop_indent();
        self.writer.add_line("}");
    }

    pub(crate) fn emit_constructor(&mut self, m: &MethodDef) {
        // The class name is unavailable here; emit a placeholder
        // method so the file still compiles. The Java reference
        // names constructors after their class — wiring that
        // requires a parent-class context the current pass doesn't
        // thread through. Fix-up in the parity slice.
        let params = m
            .params
            .iter()
            .map(|p| format!("Object {}", mangle::local(self.opts, &p.name)))
            .collect::<Vec<_>>()
            .join(", ");
        self.writer.add_line(&format!(
            "public Object m_constructor({params}) throws LeekRunException {{"
        ));
        self.writer.push_indent();
        if let Some(body) = &m.body {
            self.emit_stmts(&body.stmts);
        }
        self.writer.add_line("return this;");
        self.writer.pop_indent();
        self.writer.add_line("}");
    }
}
