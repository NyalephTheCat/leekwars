//! Callee-side default-argument handling: deciding which omitted-argument
//! defaults the entry mechanism can fill (`fillable_default` /
//! `default_return_blocks`) and compile-time-folding self-contained defaults
//! to a constant (`const_default`) or a composite [`leek_runtime::Value`]
//! (`const_eval_default`) the call site can splice in.

use std::collections::{HashMap, HashSet};

use super::{
    BlockId, Const, LocalId, MirFunction, Operand, Place, Rvalue, SetElem, Statement, Terminator,
};

/// A parameter's `default_init` entry block id IF the default is "fillable" by
/// the callee-side entry mechanism: its sub-CFG (reachable from the entry
/// block) consists only of control-flow (`Goto`/`Branch`/`Switch`) and
/// value-returning `Return(Some(_))` exits — covering both a single
/// `return <expr>` block and a multi-block conditional (`y = c ? a : b`). A
/// `Return(None)` / `Unreachable` makes it unfillable (the function still
/// compiles, with the default sub-CFG dead, and the call site skips on omit).
pub(super) fn fillable_default(f: &MirFunction, param: LocalId) -> Option<BlockId> {
    let entry = f.locals[param.0 as usize].default_init?;
    let by_id: HashMap<BlockId, &leek_mir::ir::BasicBlock> =
        f.blocks.iter().map(|b| (b.id, b)).collect();
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![entry];
    while let Some(bid) = stack.pop() {
        if !seen.insert(bid) {
            continue;
        }
        let b = by_id.get(&bid)?;
        match &b.terminator {
            Terminator::Return(Some(_)) => {}
            Terminator::Return(None) | Terminator::Unreachable => return None,
            Terminator::Goto(t) => stack.push(*t),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                stack.push(*then_block);
                stack.push(*else_block);
            }
            Terminator::Switch { arms, default, .. } => {
                stack.extend(arms.iter().map(|(_, t)| *t));
                stack.push(*default);
            }
        }
    }
    Some(entry)
}

/// The `Return(Some(_))`-terminated blocks in a default's sub-CFG (reachable
/// from `entry`) — each becomes an entry-time param filler (store the returned
/// value into the param var + jump to the continuation).
pub(super) fn default_return_blocks(f: &MirFunction, entry: BlockId) -> Vec<BlockId> {
    let by_id: HashMap<BlockId, &leek_mir::ir::BasicBlock> =
        f.blocks.iter().map(|b| (b.id, b)).collect();
    let mut seen: HashSet<BlockId> = HashSet::new();
    let mut stack = vec![entry];
    let mut out = Vec::new();
    while let Some(bid) = stack.pop() {
        if !seen.insert(bid) {
            continue;
        }
        let Some(b) = by_id.get(&bid) else { continue };
        match &b.terminator {
            Terminator::Return(Some(_)) => out.push(bid),
            Terminator::Goto(t) => stack.push(*t),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                stack.push(*then_block);
                stack.push(*else_block);
            }
            Terminator::Switch { arms, default, .. } => {
                stack.extend(arms.iter().map(|(_, t)| *t));
                stack.push(*default);
            }
            _ => {}
        }
    }
    out
}

/// A parameter's *self-contained constant* default value, if any — the
/// `default_init` block is a single `Return` of a constant (directly, or a
/// const assigned to the returned local). `None` when the param has no
/// default or its default references other params / builds a composite (in
/// which case it must be evaluated in the callee's frame, which the call
/// site can't do).
pub(super) fn const_default(f: &MirFunction, param: LocalId) -> Option<Const> {
    let bb = f.locals[param.0 as usize].default_init?;
    let block = f.blocks.get(bb.0 as usize).filter(|b| b.id == bb)?;
    match &block.terminator {
        Terminator::Return(Some(Operand::Const(c))) => Some(c.clone()),
        Terminator::Return(Some(Operand::Local(t))) => {
            // `tmp = <const>; return tmp` — only a constant assignment to the
            // returned local (no other statements touching it) qualifies.
            block.statements.iter().rev().find_map(|s| match s {
                Statement::Assign(
                    Place::Local(id),
                    Rvalue::Use(Operand::Const(c)) | Rvalue::UseFresh(Operand::Const(c)),
                ) if id == t => Some(c.clone()),
                _ => None,
            })
        }
        _ => None,
    }
}

/// A resolved default for an omitted trailing call argument: either a single
/// scalar constant (padded via an `Operand::Const`) or a compile-time-folded
/// composite value (boxed + deep-cloned fresh per call).
pub(super) enum DefaultArg {
    Const(Const),
    Composite(leek_runtime::Value),
}

/// Compile-time-evaluate a parameter's `default_init` block to a constant
/// `leek_runtime::Value` when the default is *self-contained* — only literals,
/// composite literals (`[1, [2, 3]]`, `['x': (1+2)*3]`, sets, objects), and
/// constant arithmetic. Returns `None` when it references a param/`this`/
/// global, indexes/fields, calls a function, or uses any other rvalue (the
/// call site then skips). Composite construction mirrors the interpreter's
/// `Rvalue::{Array,Map,Set,Object}` exactly (same `key_repr` canonicalization),
/// so the folded value matches. The caller boxes it once and deep-clones per
/// call, matching the interpreter's fresh-per-call default re-evaluation.
pub(super) fn const_eval_default(
    f: &MirFunction,
    param: LocalId,
    version: u8,
) -> Option<leek_runtime::Value> {
    let bb = f.locals[param.0 as usize].default_init?;
    let block = f.blocks.get(bb.0 as usize).filter(|b| b.id == bb)?;
    let mut scratch: HashMap<LocalId, leek_runtime::Value> = HashMap::new();
    for s in &block.statements {
        match s {
            // Op-metering charges are runtime no-ops for a pure value fold.
            Statement::Charge(_) => {}
            Statement::Assign(Place::Local(id), rv) => {
                let v = const_eval_rvalue(rv, &scratch, version)?;
                scratch.insert(*id, v);
            }
            // Any other statement (field/index/global write) means the default
            // isn't a self-contained value — bail.
            _ => return None,
        }
    }
    match &block.terminator {
        Terminator::Return(Some(op)) => const_eval_operand(op, &scratch, version),
        _ => None,
    }
}

fn const_eval_operand(
    op: &Operand,
    scratch: &HashMap<LocalId, leek_runtime::Value>,
    version: u8,
) -> Option<leek_runtime::Value> {
    match op {
        Operand::Const(c) => Some(const_to_value(c, version)),
        Operand::Local(id) => scratch.get(id).cloned(),
    }
}

fn const_to_value(c: &Const, _version: u8) -> leek_runtime::Value {
    use leek_runtime::Value as V;
    match c {
        Const::Null => V::Null,
        Const::Bool(b) => V::Bool(*b),
        Const::Int(i) => V::Int(*i),
        Const::Real(bits) => V::Real(f64::from_bits(*bits)),
        Const::BigInt(s) => V::BigInt(std::rc::Rc::new(leek_runtime::big_from_decimal(s))),
        Const::String(s) => V::String(std::rc::Rc::new(s.clone())),
    }
}

fn const_eval_rvalue(
    rv: &Rvalue,
    scratch: &HashMap<LocalId, leek_runtime::Value>,
    version: u8,
) -> Option<leek_runtime::Value> {
    use leek_runtime::Value as V;
    use std::cell::RefCell;
    use std::rc::Rc;
    match rv {
        Rvalue::Use(op) | Rvalue::UseFresh(op) => const_eval_operand(op, scratch, version),
        Rvalue::Array(elems) => {
            let vs = elems
                .iter()
                .map(|o| const_eval_operand(o, scratch, version))
                .collect::<Option<Vec<_>>>()?;
            Some(V::Array(Rc::new(RefCell::new(vs))))
        }
        Rvalue::Set(items) => {
            let mut s = leek_runtime::SetData::new();
            for item in items {
                match item {
                    SetElem::One(o) => {
                        s.insert(const_eval_operand(o, scratch, version)?);
                    }
                    // Range length depends on runtime bound values; don't const-fold.
                    SetElem::Range(..) => return None,
                }
            }
            Some(V::Set(Rc::new(RefCell::new(s))))
        }
        Rvalue::Map(pairs) => {
            let mut m = leek_runtime::MapData::new();
            for (k, v) in pairs {
                let kv = const_eval_operand(k, scratch, version)?;
                let vv = const_eval_operand(v, scratch, version)?;
                let canon = leek_runtime::key_repr(&kv);
                m.insert_canonical(canon, kv, vv);
            }
            Some(V::Map(Rc::new(RefCell::new(m))))
        }
        Rvalue::Object(pairs) => {
            let mut o = leek_runtime::ObjectData::new();
            for (k, v) in pairs {
                o.set(k, const_eval_operand(v, scratch, version)?);
            }
            Some(V::Object(Rc::new(RefCell::new(o))))
        }
        Rvalue::Binary(op, l, r) => {
            let lv = const_eval_operand(l, scratch, version)?;
            let rv2 = const_eval_operand(r, scratch, version)?;
            Some(crate::runtime::apply_binop(*op, &lv, &rv2, version))
        }
        // Unary, Index, Field, New, calls, refs — not self-contained / not
        // modelled here. Skip (the call site falls back to the existing
        // const-default-or-skip path).
        _ => None,
    }
}
