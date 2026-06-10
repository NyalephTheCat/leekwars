use super::prelude::*;

impl Checker {
    pub(crate) fn infer_binary(&mut self, b: &BinaryExpr) -> Type {
        // Recurse first to gather child types.
        let lhs_ty = b.lhs().map_or(Type::Any, |l| self.infer_expr(&l));
        let rhs_ty = b.rhs().map_or(Type::Any, |r| self.infer_expr(&r));

        let op = b.op();
        let op_kind = op.as_ref().map(rowan::SyntaxToken::kind);

        self.check_plain_assignment(b, op_kind, &rhs_ty);
        if self.opts.strict {
            self.check_null_compound_or_index_assign(b, op_kind);
            self.check_empty_collection_index_assign(b, op_kind);
        }
        binary_result_type(op_kind, &lhs_ty, &rhs_ty)
    }

    /// Strict v4 only: an index-assign to a var that was declared
    /// with an empty `[]` / `[:]` initializer is
    /// `ASSIGNMENT_INCOMPATIBLE_TYPE`. Upstream's type inference
    /// treats empty literals as `Array<nothing>` / `Map<*, nothing>`,
    /// rejecting any subsequent write.
    pub(crate) fn check_empty_collection_index_assign(
        &mut self,
        b: &BinaryExpr,
        op_kind: Option<SyntaxKind>,
    ) {
        if self.version != Version::V4 {
            return;
        }
        let Some(kind) = op_kind else { return };
        if !(matches!(kind, SyntaxKind::Eq) || is_compound_assignment(kind)) {
            return;
        }
        let Some(Expr::Index(idx)) = b.lhs() else {
            return;
        };
        let Some(Expr::Name(base)) = idx.base() else {
            return;
        };
        let Some(ident) = base.ident() else { return };
        if self.empty_collection_vars.contains(ident.text()) {
            self.err(
                codes::ASSIGNMENT_INCOMPATIBLE_TYPE,
                self.span_of(&ident),
                format!(
                    "`{}` was declared as an empty collection; cannot \
                     assign into it (strict v4)",
                    ident.text(),
                ),
            );
        }
    }

    /// Subcase of [`infer_binary`]: plain `=` to a `Name` updates
    /// our recorded type for the binding (clearing null-bindings
    /// once reassigned) and emits `ASSIGNMENT_INCOMPATIBLE_TYPE`
    /// when the RHS is incompatible with the declared type.
    pub(crate) fn check_plain_assignment(
        &mut self,
        b: &BinaryExpr,
        op_kind: Option<SyntaxKind>,
        rhs_ty: &Type,
    ) {
        if !is_plain_assignment(op_kind) {
            return;
        }
        let Some(Expr::Name(name)) = b.lhs() else {
            return;
        };
        let Some(ident) = name.ident() else { return };
        let Some(declared) = self.lookup(ident.text()).cloned() else {
            return;
        };
        if !matches!(declared, Type::Any | Type::Null) && !self.types_assignable(rhs_ty, &declared)
        {
            self.err(
                codes::ASSIGNMENT_INCOMPATIBLE_TYPE,
                self.span_of(&ident),
                format!(
                    "cannot assign {} value to `{}` of type {}",
                    type_name(rhs_ty),
                    ident.text(),
                    type_name(&declared),
                ),
            );
        }
        // Clear a null-binding once the var is reassigned to a
        // non-null value: `var a = null; a = 5; a *= 2` is fine
        // after the reassignment.
        if matches!(declared, Type::Null) && !matches!(rhs_ty, Type::Null) {
            self.declare(ident.text(), Type::Any);
        }
    }

    /// Strict-only: compound-assign / index-assign on a null-bound
    /// binding. `var a = null; a *= 5` and `var a = null; a[1] = 12`
    /// are both ASSIGNMENT_INCOMPATIBLE_TYPE in upstream. Without
    /// strict, upstream lets the runtime handle it.
    pub(crate) fn check_null_compound_or_index_assign(
        &mut self,
        b: &BinaryExpr,
        op_kind: Option<SyntaxKind>,
    ) {
        let Some(kind) = op_kind else { return };
        // Compound assignment with the LHS being a plain Name?
        if is_compound_assignment(kind)
            && let Some(Expr::Name(name)) = b.lhs()
            && let Some(ident) = name.ident()
            && matches!(self.lookup(ident.text()), Some(Type::Null))
        {
            self.err(
                codes::ASSIGNMENT_INCOMPATIBLE_TYPE,
                self.span_of(&ident),
                format!(
                    "`{}` is null; cannot apply compound assignment",
                    ident.text()
                ),
            );
        }
        // Index-assignment (`a[i] = v` or any compound form on an
        // Index) where the base is a null-bound Name.
        if (matches!(kind, SyntaxKind::Eq) || is_compound_assignment(kind))
            && let Some(Expr::Index(idx)) = b.lhs()
            && let Some(Expr::Name(base)) = idx.base()
            && let Some(ident) = base.ident()
            && matches!(self.lookup(ident.text()), Some(Type::Null))
        {
            self.err(
                codes::ASSIGNMENT_INCOMPATIBLE_TYPE,
                self.span_of(&ident),
                format!("`{}` is null; cannot index into it", ident.text()),
            );
        }
    }
}
