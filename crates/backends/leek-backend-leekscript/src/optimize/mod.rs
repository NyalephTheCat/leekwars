//! Opt-in HIR→HIR optimization passes run before emission.
//!
//! Conservative by design: only transformations that provably preserve
//! observable behavior under official semantics. The pass set is
//! constant folding (a safe arithmetic/logic subset) followed by
//! dead-code elimination. Function inlining is intentionally deferred.

mod const_fold;
mod dead_code;

use leek_hir::{Def, HirFile, Stmt};

/// Run the optimization pipeline on a copy of `hir`.
#[must_use]
pub fn run(hir: &HirFile) -> HirFile {
    let mut hir = hir.clone();
    for def in &mut hir.defs {
        match def {
            Def::Function(f) => {
                if let Some(b) = &mut f.body {
                    opt_block(&mut b.stmts);
                }
            }
            Def::Class(c) => {
                for m in c.methods.iter_mut().chain(c.constructors.iter_mut()) {
                    if let Some(b) = &mut m.body {
                        opt_block(&mut b.stmts);
                    }
                }
                for fld in &mut c.fields {
                    if let Some(init) = &mut fld.init {
                        const_fold::fold_expr(init);
                    }
                }
            }
            Def::Global(g) => {
                if let Some(init) = &mut g.init {
                    const_fold::fold_expr(init);
                }
            }
            Def::Local(_) => {}
        }
    }
    opt_block(&mut hir.main);
    hir
}

fn opt_block(stmts: &mut Vec<Stmt>) {
    for s in stmts.iter_mut() {
        const_fold::fold_stmt(s);
    }
    dead_code::run_stmts(stmts);
}
