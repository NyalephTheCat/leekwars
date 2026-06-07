//! Backend-agnostic HIR transforms.
//!
//! [`fold_constants`] rewrites references to named constants into literal
//! values. It is an *opt-in* pass — the caller supplies the `name →
//! literal` map and decides when to run it — so it stays decoupled from
//! any particular constant source. Its motivating use is the leek-wars
//! fight constants (`WEAPON_PISTOL` → `37`), whose values a driver pulls
//! from `leek_environment::leekwars_constant_values()`; because the pass
//! mutates the shared HIR, *every* backend (Java, native, interpreter)
//! then sees a plain literal instead of an undefined identifier.

use std::collections::HashMap;

use crate::ir::{Block, Def, Expr, ExprKind, HirFile, Literal, NameRef, Stmt};
use crate::visit::{Flow, VisitMut, VisitableMut};

/// Replace every `Name(Builtin(name))` whose `name` is a key in `values`
/// with the corresponding literal, throughout `hir` (function bodies,
/// class members + field inits, globals, and the main block). Returns the
/// number of substitutions made.
///
/// Only `NameRef::Builtin` is folded: those are the unresolved
/// builtin/constant identifiers (a registered environment constant lowers
/// to one). Locals, globals, functions, and classes are real bindings and
/// are left untouched.
pub fn fold_constants(hir: &mut HirFile, values: &HashMap<String, Literal>) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut folder = ConstFolder { values, count: 0 };
    for def in &mut hir.defs {
        match def {
            Def::Function(f) => {
                if let Some(b) = &mut f.body {
                    let _ = b.walk_mut(&mut folder);
                }
            }
            Def::Class(c) => {
                for field in &mut c.fields {
                    if let Some(e) = &mut field.init {
                        let _ = e.walk_mut(&mut folder);
                    }
                }
                for m in c.methods.iter_mut().chain(c.constructors.iter_mut()) {
                    if let Some(b) = &mut m.body {
                        let _ = b.walk_mut(&mut folder);
                    }
                }
            }
            Def::Global(g) => {
                if let Some(e) = &mut g.init {
                    let _ = e.walk_mut(&mut folder);
                }
            }
            Def::Local(_) => {}
        }
    }
    for s in &mut hir.main {
        let _ = s.walk_mut(&mut folder);
    }
    folder.count
}

/// In-place constant-folding visitor. The only node it rewrites is a
/// `Name(Builtin(name))` whose `name` is in `values`; everything else is
/// left for the framework to recurse into. Substituting a literal prunes
/// recursion (a literal has no children) via [`Flow::Skip`].
struct ConstFolder<'a> {
    values: &'a HashMap<String, Literal>,
    count: usize,
}

impl VisitMut<Expr> for ConstFolder<'_> {
    fn visit_mut(&mut self, e: &mut Expr) -> Flow {
        if let ExprKind::Name(NameRef::Builtin(name)) = &e.kind
            && let Some(lit) = self.values.get(name)
        {
            e.kind = ExprKind::Literal(lit.clone());
            self.count += 1;
            return Flow::Skip; // a literal has no children to recurse into
        }
        Flow::Walk
    }
}

// `ConstFolder` only reacts to expressions; blocks and statements use the
// `VisitMut` default no-op so the `HirVisitorMut` umbrella is satisfied.
impl VisitMut<Block> for ConstFolder<'_> {}
impl VisitMut<Stmt> for ConstFolder<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Callee, Stmt};
    use leek_span::Span;

    fn span() -> Span {
        Span::synthetic()
    }

    fn name(n: &str) -> Expr {
        Expr {
            kind: ExprKind::Name(NameRef::Builtin(n.to_string())),
            ty: leek_types::Type::Any,
            span: span(),
        }
    }

    #[test]
    fn folds_builtin_name_in_nested_expression() {
        let mut values = HashMap::new();
        values.insert("WEAPON_PISTOL".to_string(), Literal::Int(37));
        // `setWeapon(WEAPON_PISTOL)` in the main block.
        let mut hir = HirFile {
            defs: Vec::new(),
            items: Vec::new(),
            main: vec![Stmt::Expr(Expr {
                kind: ExprKind::Call(Box::new(crate::ir::Call {
                    callee: Callee::Function(NameRef::Builtin("setWeapon".into())),
                    args: vec![name("WEAPON_PISTOL")],
                    span: span(),
                })),
                ty: leek_types::Type::Any,
                span: span(),
            })],
        };
        let n = fold_constants(&mut hir, &values);
        assert_eq!(n, 1);
        let Stmt::Expr(Expr {
            kind: ExprKind::Call(c),
            ..
        }) = &hir.main[0]
        else {
            panic!()
        };
        assert_eq!(c.args[0].kind, ExprKind::Literal(Literal::Int(37)));
    }

    #[test]
    fn leaves_unknown_names_and_real_bindings_untouched() {
        let values = HashMap::new(); // empty → no-op fast path
        let mut hir = HirFile {
            defs: Vec::new(),
            items: Vec::new(),
            main: vec![Stmt::Expr(name("SOMETHING"))],
        };
        assert_eq!(fold_constants(&mut hir, &values), 0);
        assert_eq!(hir.main[0], Stmt::Expr(name("SOMETHING")));
    }
}
