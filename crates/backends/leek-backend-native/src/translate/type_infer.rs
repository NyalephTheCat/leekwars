//! Local-type inference (moved verbatim from translate/mod.rs).

use super::{
    BinOp, FnRets, HashMap, HashSet, Lang, LocalId, MirFunction, MirProgram, NativeError, Place,
    Rvalue, Statement, Type, ValTy, aliased_class_locals, call_result_ty, classref_locals,
    is_const_zero, join, new_class_locals, pinned_valty, rvalue_ty, scalar_valty,
};

/// Static value-kind per local.
///
/// Locals with an explicit `integer` / `real` declared type are *pinned*
/// to that kind: Leekscript's `write_place` coerces every assignment to
/// the declared type (so `integer x = 42.9` stores `42`, `real y = 3`
/// stores `3.0`), and we mirror that by coercing at the store site.
///
/// Untyped `var` locals (declared `Any`) have no such coercion — their
/// runtime kind is whatever the last assignment produced. We infer it by
/// a fixpoint join over assignments. When such a local is assigned *both*
/// a real and a non-real value (`var a = 5.5; a = 2`), its observable
/// kind changes across the program and a single static slot can't model
/// it faithfully — so we bail and let the case skip.
pub(super) fn infer_local_tys(
    f: &MirFunction,
    lang: Lang,
    rets: &FnRets,
    // When true (uniform-ABI lambda body), every param is a boxed `Ref`
    // (loaded from `argv`), regardless of its declared type.
    force_ref_params: bool,
    program: &MirProgram,
) -> Result<HashMap<LocalId, ValTy>, NativeError> {
    // Receiver-class maps for typing method-call results (a user method's
    // call result is `Ref`, even when its name collides with a builtin).
    let nc = new_class_locals(f);
    let alias = aliased_class_locals(f);
    let crefs = classref_locals(f);
    // Pin explicit integer/real declared types (boolean is intentionally
    // left to inference: a `boolean` slot in Leekscript keeps a stored
    // int/real as-is, so it behaves like an untyped slot here). In strict
    // mode an untyped `var` also coerces every write to its inferred type
    // (the interp's `write_place`), so pin from `inferred_ty` too.
    let mut pinned: HashMap<LocalId, ValTy> = HashMap::new();
    for (i, decl) in f.locals.iter().enumerate() {
        // A lambda-captured local is backed by a shared `Value::Cell` whose
        // contents are dynamically typed — a closure may store any value into
        // it regardless of the declared scalar type (`var n = 1` captured then
        // reassigned to a string). Pin it to a boxed `Ref` so reads/returns
        // never narrow-coerce the dynamic contents.
        if decl.is_shared {
            pinned.insert(LocalId(i as u32), ValTy::Ref);
            continue;
        }
        let pin = pinned_valty(&decl.ty).or_else(|| {
            if lang.strict {
                decl.inferred_ty.as_ref().and_then(pinned_valty)
            } else {
                None
            }
        });
        if let Some(vt) = pin {
            pinned.insert(LocalId(i as u32), vt);
        }
    }
    // Parameters take their kind from the calling convention (matching
    // `function_sig`): scalar declared types unboxed, everything else a
    // boxed `Ref`. This must override the loop above so an untyped param
    // is seen as `Ref`, not inferred from (absent) assignments.
    for pid in &f.params {
        let vt = if force_ref_params {
            ValTy::Ref
        } else {
            scalar_valty(&f.locals[pid.0 as usize].ty).unwrap_or(ValTy::Ref)
        };
        pinned.insert(*pid, vt);
    }

    // Locals used as an object base: the receiver of a `base.field` read.
    // Such a local has to be able to hold an object. When inference would
    // otherwise narrow it to a scalar — `Cell | integer x = 3` lowers `x`'s
    // type to `Any` and its only assignment is `x = 3`, so the join is
    // `Int`, yet a *reachable-but-dead* `x.id` in the `instanceof` arm still
    // has to compile — promote it to a boxed `Ref` so the dynamic field path
    // applies. A genuinely scalar-declared `integer x` is never a field base
    // in valid source, so it's excluded via `pinned`. Field *writes* and
    // method receivers are deliberately omitted: those go through paths that
    // assume a statically-known class and would mis-handle a boxed scalar.
    let object_base: HashSet<LocalId> = {
        let mut set = HashSet::new();
        for block in &f.blocks {
            for s in &block.statements {
                if let Statement::Assign(_, Rvalue::Field(id, _)) = s {
                    set.insert(*id);
                }
            }
        }
        set
    };

    // A local that takes both real and non-real *scalar* values can't be a
    // single unboxed scalar slot — promote it to a boxed `Ref` (which holds
    // either) and re-run inference so dependents see the new kind. Iterates
    // to a fixpoint (each round only adds to `forced_ref`, monotonic).
    let mut forced_ref: HashSet<LocalId> = HashSet::new();
    loop {
        let mut tys = pinned.clone();
        for id in &forced_ref {
            tys.insert(*id, ValTy::Ref);
        }
        let frozen: HashSet<LocalId> = pinned
            .keys()
            .copied()
            .chain(forced_ref.iter().copied())
            .collect();
        loop {
            let mut changed = false;
            for block in &f.blocks {
                for s in &block.statements {
                    let (id, t) = match s {
                        // v1 division by a statically-zero divisor is `null`
                        // (a boxed `Ref`), not a real — see `binary`.
                        Statement::Assign(Place::Local(id), Rvalue::Binary(BinOp::Div, _, r))
                            if lang.version == 1 && is_const_zero(r) =>
                        {
                            (*id, Some(ValTy::Ref))
                        }
                        Statement::Assign(Place::Local(id), rv) => (*id, rvalue_ty(rv, &tys)),
                        Statement::Call {
                            dest: Some(Place::Local(id)),
                            call,
                        } => (
                            *id,
                            call_result_ty(
                                call, &tys, rets, program, &f.locals, &nc, &alias, &crefs, lang,
                            ),
                        ),
                        _ => continue,
                    };
                    if frozen.contains(&id) {
                        continue;
                    }
                    let Some(t) = t else { continue };
                    let merged = match tys.get(&id) {
                        None => t,
                        Some(&c) => join(c, t),
                    };
                    if tys.get(&id) != Some(&merged) {
                        tys.insert(id, merged);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }

        // Find untyped locals that took both real and non-real values.
        let mut new_mixed = false;
        for i in 0..f.locals.len() {
            let id = LocalId(i as u32);
            if frozen.contains(&id) {
                continue;
            }
            let (mut saw_real, mut saw_nonreal, mut saw_ref) = (false, false, false);
            for block in &f.blocks {
                for s in &block.statements {
                    let t = match s {
                        Statement::Assign(Place::Local(aid), rv) if *aid == id => {
                            rvalue_ty(rv, &tys)
                        }
                        Statement::Call {
                            dest: Some(Place::Local(aid)),
                            call,
                        } if *aid == id => call_result_ty(
                            call, &tys, rets, program, &f.locals, &nc, &alias, &crefs, lang,
                        ),
                        _ => continue,
                    };
                    match t {
                        Some(ValTy::Real) => saw_real = true,
                        Some(ValTy::Ref) => saw_ref = true,
                        Some(_) => saw_nonreal = true,
                        None => {}
                    }
                }
            }
            // A `Ref` slot boxes whatever it holds, so a real/int mix there is
            // fine; a purely scalar real/int mix is promoted to `Ref`.
            if saw_real && saw_nonreal && !saw_ref {
                forced_ref.insert(id);
                new_mixed = true;
            }
        }
        // Object-base locals that inference left as an unboxed scalar must be
        // boxed so the dynamic field path can apply (see `object_base`).
        for &id in &object_base {
            if pinned.contains_key(&id) || forced_ref.contains(&id) {
                continue;
            }
            if matches!(tys.get(&id), Some(ValTy::Int | ValTy::Real | ValTy::Bool)) {
                forced_ref.insert(id);
                new_mixed = true;
            }
        }
        if !new_mixed {
            // A never-assigned untyped local (`var x` with no initializer) is
            // null at runtime — model it as a boxed `Ref` (null handle) so a
            // read or return yields null, not a `0` scalar. Scalar/boolean
            // declared types keep their pinned/zero default (they can't hold
            // null anyway). Params are already pinned, so they're untouched.
            for (i, decl) in f.locals.iter().enumerate() {
                let id = LocalId(i as u32);
                if matches!(decl.ty, Type::Any) && !tys.contains_key(&id) {
                    tys.insert(id, ValTy::Ref);
                }
            }
            return Ok(tys);
        }
    }
}
