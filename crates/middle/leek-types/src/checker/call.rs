use super::prelude::*;

impl Checker {
    /// Type-check a call and return its result type. Resolves user
    /// function returns, instance/static method returns, and recurses
    /// into the callee/args for sub-expression inference.
    pub(crate) fn check_call(&mut self, c: &CallExpr) -> Type {
        // Infer args (collect their types for the type-check below).
        let mut arg_types = Vec::new();
        let mut arg_spans = Vec::new();
        let mut arg_exprs = Vec::new();
        if let Some(args) = c.arg_list() {
            for a in args.args() {
                let r = a.syntax().text_range();
                let span = Span::new(self.source, u32::from(r.start()), u32::from(r.end()));
                let t = self.infer_expr(&a);
                arg_types.push(t);
                arg_spans.push(span);
                arg_exprs.push(a);
            }
        }
        // Resolve callee for type inference of sub-expressions, and
        // determine the call's result type.
        let mut result = Type::Any;
        let callee_name = match c.callee() {
            Some(Expr::Name(n)) => {
                let nm = n.ident().map(|t| t.text().to_string());
                if let Some(name) = &nm {
                    if let Some(sig) = self.user_fn_generic.get(name) {
                        // Experimental: a generic user function (`f<T>(…)`)
                        // resolves its return against the concrete args, so
                        // `first(intArr)` yields `integer`, not the bare `T`.
                        result = sig.instantiate(&arg_types);
                    } else if let Some(ret) = self.user_fn_return_type.get(name) {
                        result = ret.clone();
                    } else if self.opts.experimental_generics
                        && let Some(sig) = crate::generic::generic_builtin(name)
                    {
                        // Experimental: resolve a generic builtin's return
                        // type against the concrete argument types.
                        result = sig.instantiate(&arg_types);
                    }
                }
                nm
            }
            // `recv.method(...)` / `Class.method(...)`.
            Some(Expr::Field(f)) => {
                if let Some(base) = f.base()
                    && let Some((class, cls_args)) = self.base_class_instance(&base)
                    && let Some(method) = f.field().map(|t| t.text().to_string())
                {
                    // Generic method: seed bindings from the receiver's
                    // class type arguments, then unify the method's own
                    // parameter patterns against the call arguments
                    // (`box.map(x)` resolves both the class `T` and the
                    // method's `U`). Walks the inheritance chain, re-mapping
                    // type args at each `extends Parent<…>` boundary. Falls
                    // back to the plain declared return when nothing
                    // resolves.
                    match self.resolve_generic_method(&class, &cls_args, &method, &arg_types) {
                        Some(t) if !matches!(t, Type::Any) => result = t,
                        _ => {
                            if let Some(ret) = self.lookup_method_return(&class, &method) {
                                result = ret;
                            }
                        }
                    }
                }
                None
            }
            Some(other) => {
                self.infer_expr(&other);
                None
            }
            None => None,
        };

        // IMPOSSIBLE_CAST: passing a user function `f` as an
        // argument where the declared param is `Function<... => R>`
        // requires `f`'s return type to be assignable to `R`. Always
        // checked (not gated on strict) since upstream emits it
        // even at default strictness.
        if let Some(name) = &callee_name
            && let Some(param_tys) = self.user_fn_param_types.get(name).cloned()
        {
            for (i, arg) in arg_exprs.iter().enumerate() {
                let Some(Type::FunctionWithReturn {
                    ret: expected_ret, ..
                }) = param_tys.get(i)
                else {
                    continue;
                };
                let Expr::Name(name_ref) = arg else { continue };
                let Some(arg_ident) = name_ref.ident() else {
                    continue;
                };
                let arg_name = arg_ident.text();
                let Some(actual_ret) = self.user_fn_return_type.get(arg_name).cloned() else {
                    continue;
                };
                // Tighter than `assignable_to`: a void-returning
                // function (collected as `Type::Null`) is NOT a
                // valid Function<... => integer>. The general
                // assignable_to permits Null↔anything for dynamic
                // semantics, but the IMPOSSIBLE_CAST check
                // explicitly rejects it.
                let compatible = match (&actual_ret, expected_ret.as_ref()) {
                    (_, Type::Any) | (Type::Any, _) => true,
                    (a, b) if a == b => true,
                    (Type::Integer, Type::Real) | (Type::Real, Type::Integer) => true,
                    _ => false,
                };
                if !compatible {
                    self.err(
                        codes::IMPOSSIBLE_CAST,
                        arg_spans[i],
                        format!(
                            "cannot pass function `{arg_name}` returning {} \
                             where {} return is expected",
                            type_name(&actual_ret),
                            type_name(expected_ret),
                        ),
                    );
                }
            }
        }

        if !self.opts.strict {
            return result;
        }
        let Some(name) = callee_name else {
            return result;
        };
        // Pick the entry with the highest `min_version` ≤ current,
        // so v4 sees its tighter signature while v1-v3 see the
        // legacy wider one.
        let Some(sig) = BUILTIN_SIGS
            .iter()
            .filter(|s| s.name == name && s.min_version <= self.version as u8)
            .max_by_key(|s| s.min_version)
        else {
            return result;
        };
        for (i, (actual_ty, span)) in arg_types.iter().zip(arg_spans.iter()).enumerate() {
            let Some(allowed) = sig.params.get(i) else {
                break;
            };
            if !type_in_set(actual_ty, allowed) {
                self.err(
                    codes::WRONG_ARGUMENT_TYPE,
                    *span,
                    format!(
                        "`{name}` argument #{}: expected {}, got {}",
                        i + 1,
                        describe_type_set(allowed),
                        type_name(actual_ty),
                    ),
                );
            }
        }
        result
    }
}
