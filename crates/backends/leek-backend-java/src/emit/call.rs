use leek_hir::{Call, Callee, Def, Expr, ExprKind, Literal, NameRef};
use leek_types::Type;

use super::EmitExpr;
use super::{
    builtin_arity, builtin_arity_strict, is_primitive_number_expr, is_string_expr,
    needs_v1_3_suffix, takes_function_arg,
};
use crate::mangle;
impl super::Emitter<'_> {
    pub(crate) fn write_call(&self, buf: &mut String, c: &Call) {
        // If the callee is a builtin name the source has
        // reassigned (`cos = function() {…}; cos(...)`), route
        // through the shadow map instead of the builtin dispatch.
        // Read the shadow's value as a FunctionLeekValue and
        // `execute(...)` it.
        if let Callee::Function(NameRef::Builtin(name)) = &c.callee
            && self.shadowed_builtins.borrow().contains(name)
        {
            buf.push_str("(__shadows.containsKey(\"");
            buf.push_str(name);
            buf.push_str("\") ? execute(__shadows.get(\"");
            buf.push_str(name);
            buf.push_str("\")");
            self.write_execute_args(buf, &c.args);
            buf.push_str(") : (");
            // Fall through to the original builtin call by
            // re-entering write_call with shadowing
            // temporarily disabled so we don't infinite-loop.
            let prev: Vec<String> = self.shadowed_builtins.borrow().iter().cloned().collect();
            self.shadowed_builtins.borrow_mut().clear();
            self.write_call(buf, c);
            self.shadowed_builtins.borrow_mut().extend(prev);
            buf.push_str("))");
            return;
        }
        match &c.callee {
            Callee::Function(NameRef::Builtin(name)) => {
                // Two dispatch shapes from `builtins::lookup`:
                //   • Static  → `<Class>.<name>(this, args...)`  (NumberClass.abs, StringClass.length, …)
                //   • Receiver → `((<class>) arg0).<name>(this, args[1..])`
                //                — for `push`/`count`/`arrayMap`/etc., which
                //                  the upstream emits as instance-method
                //                  calls on the receiver value-class.
                // Unknown names fall back to bare `name(...)`.
                if let Some(builtin) = crate::builtins::lookup(name) {
                    let (class, is_receiver) = builtin.resolved_class(self.opts.version);
                    // Method-name suffix at v1–v3 for builtins whose
                    // return type changed shape (Array, Map, etc.) —
                    // upstream's `JavaWriter.writeFunctionCall` appends
                    // `_v1_3` so the call dispatches to the legacy
                    // implementation. See `LegacyArrayLeekValue` for
                    // the `*_v1_3` overloads.
                    let suffix = if !matches!(self.opts.version, leek_syntax::Version::V4)
                        && needs_v1_3_suffix(name)
                    {
                        "_v1_3"
                    } else {
                        ""
                    };
                    if is_receiver {
                        // ((<class>) (Object) arg0).<name>(this, args[1..])
                        //
                        // The interstitial `(Object)` lets us cast from
                        // statically-incompatible types (e.g. a String
                        // literal where the user wrote `count('hello')`)
                        // without a javac "inconvertible types" error.
                        // For `count`, dispatch on the receiver's
                        // actual runtime type — Leek returns 0 when
                        // count is called on a non-Array value (the
                        // upstream uses a generic-helper for this).
                        if let Some(receiver) = c.args.first() {
                            if name == "count" && c.args.len() == 1 {
                                buf.push_str("((Object) ");
                                self.write_expr(buf, receiver, true);
                                buf.push_str(" instanceof GenericArrayLeekValue) ? (long) ((GenericArrayLeekValue) (Object) ");
                                self.write_expr(buf, receiver, true);
                                buf.push_str(").size() : 0l");
                                return;
                            }
                            buf.push_str("((");
                            buf.push_str(class);
                            buf.push_str(") (Object) ");
                            self.write_expr(buf, receiver, false);
                            buf.push(')');
                            buf.push('.');
                            buf.push_str(name);
                            buf.push_str(suffix);
                            buf.push('(');
                            buf.push_str(self.ai_this());
                            let cast_fn_arg = takes_function_arg(name);
                            for (i, a) in c.args[1..].iter().enumerate() {
                                buf.push_str(", ");
                                if i == 0 && cast_fn_arg {
                                    buf.push_str("(FunctionLeekValue) ");
                                    self.write_expr(buf, a, true);
                                } else {
                                    self.write_expr(buf, a, false);
                                }
                            }
                            buf.push(')');
                        } else {
                            // Receiver builtin called with zero args
                            // (typically `isEmpty()` after the user
                            // shadowed the name via assign-to-builtin
                            // in Fix 10). The receiver dispatch needs
                            // a receiver; without one javac can't find
                            // a method to call. Short-circuit to null
                            // for compile-only success.
                            buf.push_str("(Object) null");
                        }
                    } else {
                        // Null arg to a NumberClass function used to
                        // short-circuit the whole call to `(Object)
                        // null`. Upstream actually coerces null → 0
                        // (Math operations treat the missing operand
                        // as zero), so corpus tests for
                        // `abs(null)` / `cos(null)` etc. expect `0`
                        // (v1) or `0.0` (v2+). We substitute the
                        // null literal with a primitive zero in the
                        // arg list below instead of short-circuiting
                        // here.
                        // `cos()` / `abs()` with too few args → would
                        // emit `NumberClass.cos(this)` (missing the
                        // double parameter) and javac rejects with
                        // "method cannot be applied". Lower to a
                        // runtime null so the surrounding program
                        // compiles. We use `builtin_arity_strict` —
                        // only short-circuit when we KNOW the
                        // expected arity is exactly that value (no
                        // overloads, not a receiver builtin). Over-
                        // arity calls (`sqrt(25, 16, 9)`) hit the
                        // same javac error and get the same null.
                        // Strict-arity builtins (`sqrt`, `cos`, …)
                        // get padded with primitive zeros on under-
                        // arity calls and truncated on over-arity.
                        // Upstream evaluates `sqrt()` as `sqrt(0)`
                        // → 0, and `sqrt(25, 16, 9)` ignores the
                        // extra args. We match that here so the
                        // corpus stops counting them as null.
                        let strict_arity = builtin_arity_strict(name);
                        let loose_arity = builtin_arity(name);
                        buf.push_str(class);
                        buf.push('.');
                        buf.push_str(name);
                        buf.push_str(suffix);
                        buf.push('(');
                        buf.push_str(self.ai_this());
                        // Pick the primitive shape Java can overload on.
                        // If any arg is statically a real, force all the
                        // Object args to double too — Java overload
                        // resolution needs a uniform pick. Otherwise use
                        // the per-name `prefer_long` hint from
                        // `builtins.tsv`.
                        // A literal `Real` arg forces doubleValue on
                        // every arg in this call (Java's overload
                        // resolution would otherwise pick the
                        // truncating long overload). HIR's
                        // `Type::Any` for `var a = 0.8 ; max(0, a)`
                        // doesn't reach here today — a proper
                        // local-type-propagation slice (deferred)
                        // would close that gap.
                        let any_real_arg = c.args.iter().any(|a| {
                            matches!(&a.kind, ExprKind::Literal(Literal::Real(_)))
                                || matches!(a.ty, Type::Real)
                        });
                        // `prefer_long` describes the RETURN type
                        // hint, not arg coercion. For rounding-style
                        // builtins (`floor`/`ceil`/`round`) the input
                        // semantically is a real — coercing it to
                        // long first truncates `-14.7` to `-14`
                        // before flooring. Force doubleValue on
                        // their args so the math runs on the
                        // pre-rounded value.
                        //
                        // `abs` is deliberately excluded: it has
                        // separate long/double overloads and the
                        // corpus expects the return type to track
                        // the input type (`abs(['a', -15][1])` →
                        // long `15`, not double `15.0`). Forcing
                        // doubleValue would break those v2+ cases.
                        let real_arg_builtin =
                            matches!(name.as_str(), "floor" | "ceil" | "round" | "signum");
                        let coerce = if any_real_arg || real_arg_builtin {
                            "doubleValue"
                        } else if builtin.prefer_long {
                            "longValue"
                        } else {
                            "doubleValue"
                        };
                        // Decide the visible arity for padding /
                        // trimming. `strict_arity` is authoritative
                        // for the math-family one-arg/two-arg
                        // builtins; outside that, fall back to
                        // `loose_arity` (which includes the arity-0
                        // System* family). Padding/trimming only
                        // applies to NumberClass calls — receiver
                        // builtins and others handle their own
                        // overloads.
                        let target_arity = strict_arity.unwrap_or(loose_arity);
                        let pad_zero = if coerce == "longValue" { "0l" } else { "0.0" };
                        // Don't truncate when we don't have a
                        // reliable target arity (avoid silently
                        // dropping args from over-arity overloads
                        // like `max(a, b, c)`).
                        let take = if strict_arity.is_some() {
                            c.args.len().min(target_arity)
                        } else {
                            c.args.len()
                        };
                        for a in c.args.iter().take(take) {
                            buf.push_str(", ");
                            // Null literal in a NumberClass arg
                            // position coerces to a zero of the
                            // appropriate primitive shape. v1 picks
                            // the long overload (display `"0"`),
                            // v2+ picks the double overload
                            // (display `"0.0"`) — upstream's
                            // version-aware emit matches this.
                            if class == "NumberClass"
                                && matches!(&a.kind, ExprKind::Literal(Literal::Null))
                            {
                                let use_long = coerce == "longValue"
                                    && matches!(self.opts.version, leek_syntax::Version::V1,);
                                buf.push_str(if use_long { "0l" } else { "0.0" });
                            } else if class == "NumberClass" && !is_primitive_number_expr(a) {
                                buf.push_str("((Number) ");
                                self.write_expr(buf, a, false);
                                buf.push_str(").");
                                buf.push_str(coerce);
                                buf.push_str("()");
                            } else if class == "StringClass"
                                && !is_string_expr(a)
                                && !is_primitive_number_expr(a)
                            {
                                // StringClass.<name> overloads accept
                                // either `String` or `long` per arg
                                // position. Primitive-typed args
                                // (`substring(s, 2, 3)`) need no cast;
                                // Object-typed locals (`u_big`, `u_rep`)
                                // do — narrow to String.
                                buf.push_str("(String) ");
                                self.write_expr(buf, a, true);
                            } else {
                                self.write_expr(buf, a, false);
                            }
                        }
                        // Pad missing args with primitive zeros so
                        // `sqrt()` becomes `sqrt(this, 0l)` (v1) or
                        // `sqrt(this, 0.0)` (v2+) — upstream
                        // evaluates them at the math identity.
                        // Only fires when the strict-arity table
                        // claims a definitive arity — otherwise
                        // we'd pad arity-0 builtins like `rand()`
                        // into a non-existent overload.
                        if class == "NumberClass" && strict_arity.is_some() {
                            for _ in c.args.len()..target_arity {
                                buf.push_str(", ");
                                buf.push_str(pad_zero);
                            }
                        }
                        buf.push(')');
                    }
                } else if let Some(env) = self.opts.environment.clone()
                    && let Some(b) = env.lookup(name)
                    && b.is_static
                {
                    // Host-environment (combat/game) function: emit the
                    // generator-compatible static dispatch
                    // `<DispatchClass>.<name>(ai, args)`, mirroring the
                    // official `LeekFunctionCall` v4-static shape. The
                    // `import <namespace>;` is emitted in the file header.
                    buf.push_str(&b.dispatch_class);
                    buf.push('.');
                    buf.push_str(name);
                    buf.push('(');
                    buf.push_str(self.ai_this());
                    for a in &c.args {
                        buf.push_str(", ");
                        self.write_expr(buf, a, false);
                    }
                    buf.push(')');
                } else if self.write_builtin_class_construct(buf, name, &c.args) {
                    // A built-in class called as a constructor (`Array()`,
                    // `Map()`, `Set(1, 2)`, `Integer()`, …) — handled above.
                } else {
                    buf.push_str(name);
                    buf.push('(');
                    for (i, a) in c.args.iter().enumerate() {
                        if i > 0 {
                            buf.push_str(", ");
                        }
                        self.write_expr(buf, a, false);
                    }
                    buf.push(')');
                }
            }
            Callee::Function(NameRef::Function(id)) => {
                let name = self.def_name(*id).to_string();
                // Look up the callee's `@java-backend:` directive (if any)
                // and whether it's a bodiless signature.
                let fn_info = self.hir.defs.iter().find_map(|d| match d {
                    Def::Function(f) if f.name == name => Some((
                        f.backend_directives
                            .iter()
                            .find(|(b, _)| b == "java")
                            .map(|(_, body)| body.clone()),
                        f.body.is_none(),
                    )),
                    _ => None,
                });
                if let Some((java_directive, bodiless)) = fn_info {
                    // FFI override: emit the directive's substituted body
                    // (`%0`, `%1`, … → the rendered arguments).
                    if let Some(body) = java_directive {
                        let args: Vec<String> = c
                            .args
                            .iter()
                            .map(|a| {
                                let mut s = String::new();
                                self.write_expr(&mut s, a, false);
                                s
                            })
                            .collect();
                        buf.push_str(&leek_syntax::doc::substitute(&body, &args));
                        return;
                    }
                    // `@java-dispatch: Class[.method]` — host-environment
                    // (combat/game) dispatch. Emit
                    // `Class.method(ai, <coerced args>)`, coercing each
                    // argument to the callee's declared parameter type
                    // (the typed `.leek` library signature is the source
                    // of truth). Unlike a `%N` template this handles
                    // optional / variadic arguments. Checked before the
                    // bodiless-builtin fallback so game signatures aren't
                    // mistaken for language builtins.
                    if let Some(disp) = self.hir.defs.iter().find_map(|d| match d {
                        Def::Function(f) if f.name == name => f
                            .backend_directives
                            .iter()
                            .find(|(b, _)| b == "java-dispatch")
                            .map(|(_, v)| v.clone()),
                        _ => None,
                    }) {
                        // Widest same-name parameter list — overloads are
                        // a prefix-superset, so per-position types are
                        // stable across arities.
                        let params: Vec<Option<Type>> = self
                            .hir
                            .defs
                            .iter()
                            .filter_map(|d| match d {
                                Def::Function(f) if f.name == name => {
                                    Some(f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>())
                                }
                                _ => None,
                            })
                            .max_by_key(Vec::len)
                            .unwrap_or_default();
                        self.write_env_dispatch(buf, &disp, &name, &c.args, &params);
                        return;
                    }
                    // Signature-file migration: a bodiless function with no
                    // directive is an existing builtin — re-emit through the
                    // builtin path by name (reusing the backend's emission).
                    if bodiless {
                        let builtin_call = Call {
                            callee: Callee::Function(NameRef::Builtin(name.clone())),
                            args: c.args.clone(),
                            span: c.span,
                        };
                        self.write_call(buf, &builtin_call);
                        return;
                    }
                }
                // Look up the callee's declared parameter + return
                // types so we can promote int → real at the call
                // site for `real`-typed params, AND coerce the
                // return value back to a long when the function is
                // declared `=> integer`. Upstream's typed function
                // signatures do both halves.
                let (param_tys, return_ty): (Vec<Option<Type>>, Option<Type>) = self
                    .hir
                    .defs
                    .iter()
                    .find_map(|d| match d {
                        Def::Function(f) if f.name == name => Some((
                            f.params.iter().map(|p| p.ty.clone()).collect(),
                            f.return_type.clone(),
                        )),
                        _ => None,
                    })
                    .unwrap_or_default();
                let wrap_long = matches!(return_ty, Some(Type::Integer));
                if wrap_long {
                    buf.push_str("((Number) AI.load((Object) ");
                }
                buf.push_str(&mangle::function(self.opts, &name));
                buf.push('(');
                for (i, a) in c.args.iter().enumerate() {
                    if i > 0 {
                        buf.push_str(", ");
                    }
                    let wants_real =
                        matches!(param_tys.get(i).cloned().flatten(), Some(Type::Real),);
                    let arg_is_int_literal = matches!(&a.kind, ExprKind::Literal(Literal::Int(_)));
                    if wants_real && arg_is_int_literal {
                        // Promote integer literal `12` → `12.0` so
                        // the typed-real parameter receives a
                        // Double box. Display is then `"12.0"`.
                        buf.push_str("(double)(");
                        self.write_expr(buf, a, false);
                        buf.push(')');
                    } else if wants_real {
                        // Non-literal arg: route through Number
                        // coercion so any int / bool / null box
                        // becomes a Double.
                        buf.push_str("((Number) AI.load((Object) ");
                        self.write_expr(buf, a, false);
                        buf.push_str(")).doubleValue()");
                    } else {
                        self.write_expr(buf, a, false);
                    }
                }
                buf.push(')');
                if wrap_long {
                    buf.push_str(")).longValue()");
                }
            }
            Callee::Function(NameRef::Unresolved(name)) => {
                // Best-effort: emit `f_name(args)` so the compile error
                // surfaces at the user's call site instead of here.
                buf.push_str(&mangle::function(self.opts, name));
                buf.push('(');
                for (i, a) in c.args.iter().enumerate() {
                    if i > 0 {
                        buf.push_str(", ");
                    }
                    self.write_expr(buf, a, false);
                }
                buf.push(')');
            }
            Callee::Function(name_ref @ (NameRef::Local(_) | NameRef::Global(_))) => {
                // `var f = cos; f(0.5)` — the callee is a local /
                // global holding a FunctionLeekValue. Route through
                // `execute(fn, args...)` like the `Callee::Expr` arm
                // does for arbitrary expressions.
                buf.push_str("execute(");
                self.write_name(buf, name_ref);
                self.write_execute_args(buf, &c.args);
                buf.push(')');
            }
            Callee::Function(_) => {
                buf.push_str("null");
            }
            Callee::Method { receiver, method } => {
                // `callMethod` is on ObjectLeekValue, not Object —
                // narrow the receiver so javac picks the right
                // dispatch. The interstitial `(Object)` lets
                // unrelated static types (`String`, etc.) through.
                buf.push_str("((ObjectLeekValue) (Object) ");
                self.write_expr(buf, receiver, false);
                buf.push(')');
                buf.push('.');
                // The runtime dispatches by `<name>_<argCount>` —
                // see `ObjectLeekValue.callMethod`'s lookup that
                // splits on the last underscore and parses the
                // suffix as an int. Without the suffix it falls
                // into the not-found path and NumberFormatExceptions.
                buf.push_str("callMethod(\"");
                buf.push_str(method);
                buf.push('_');
                buf.push_str(&c.args.len().to_string());
                // 2nd arg is a ClassLeekValue for visibility checks
                // (`null` = no class context, allows public access).
                buf.push_str("\", (ClassLeekValue) null");
                for a in &c.args {
                    buf.push_str(", ");
                    self.write_expr(buf, a, false);
                }
                buf.push(')');
            }
            Callee::Expr(e) => {
                // `execute(fn, args...)` — AI instance method that
                // dispatches to the `FunctionLeekValue.run(...)` of
                // the callable, varargs-style for the trailing
                // arguments.
                buf.push_str("execute(");
                self.write_expr(buf, e, false);
                self.write_execute_args(buf, &c.args);
                buf.push(')');
            }
        }
    }

    /// Emit a host-environment dispatch `Class.method(ai, coerced…)` for a
    /// `@java-dispatch:` function. The directive value is `Class` (method =
    /// the Leekscript name) or `Class.method`. Each argument is coerced to
    /// the callee's declared parameter type so the generator's concretely
    /// typed dispatch methods (`moveToward(EntityAI, long, long)`) accept
    /// our otherwise `Object`-typed values. `Object` parameters take the
    /// value as-is (the dispatch method converts internally).
    fn write_env_dispatch(
        &self,
        buf: &mut String,
        dispatch: &str,
        name: &str,
        args: &[Expr],
        params: &[Option<Type>],
    ) {
        // The directive value is the dispatch class — fully qualified
        // (`com.leekwars.generator.classes.FightClass`) so no import is
        // needed. The Java method is the Leekscript name, unless an
        // explicit `Class#method` override is given (dots in an FQN make
        // `#` the unambiguous separator).
        let (class, method) = dispatch.split_once('#').unwrap_or((dispatch, name));
        buf.push_str(class);
        buf.push('.');
        buf.push_str(method);
        buf.push('(');
        buf.push_str(self.ai_this());
        for (i, a) in args.iter().enumerate() {
            buf.push_str(", ");
            match params.get(i).cloned().flatten() {
                Some(Type::Integer) => {
                    buf.push_str("((Number) AI.load((Object) ");
                    self.write_expr(buf, a, false);
                    buf.push_str(")).longValue()");
                }
                Some(Type::Real) => {
                    buf.push_str("((Number) AI.load((Object) ");
                    self.write_expr(buf, a, false);
                    buf.push_str(")).doubleValue()");
                }
                Some(Type::String) => {
                    buf.push_str("((String) AI.load((Object) ");
                    self.write_expr(buf, a, false);
                    buf.push_str("))");
                }
                _ => self.write_expr(buf, a, false),
            }
        }
        buf.push(')');
    }

    /// Emit the varargs tail of an `execute(fn, …)` call. A bare
    /// single `null` arg would degrade to `values = null` inside the
    /// callee (instead of `values = new Object[]{null}`), so the
    /// callee crashes on `values.length`. Wrap that case in an
    /// explicit `new Object[]{null}` — same fix the upstream does
    /// in `LeekFunctionCall.compileL`.
    pub(crate) fn write_execute_args(&self, buf: &mut String, args: &[Expr]) {
        let single_null =
            args.len() == 1 && matches!(&args[0].kind, ExprKind::Literal(Literal::Null));
        if single_null {
            buf.push_str(", new Object[] { null }");
            return;
        }
        for a in args {
            buf.push_str(", ");
            self.write_expr(buf, a, false);
        }
    }
}
