//! Overload renaming.
//!
//! Official LeekScript has no function overloading, but the experimental
//! HIR can hold several top-level `Def::Function`s sharing one name. Every
//! call resolves to a concrete `DefId`, so we can give each definition a
//! unique name and emit both definitions and call sites through the map.
//!
//! Names are assigned deterministically (definitions sorted by name, then
//! declaration order): the first keeps the bare name, later ones gain a
//! `_2`, `_3`, … suffix, skipping any already-taken name.

use std::collections::{BTreeMap, BTreeSet};

use leek_hir::{Def, DefId, HirFile};

use crate::emit::will_emit;
use crate::options::Options;

pub(crate) type RenameMap = BTreeMap<DefId, String>;

pub(crate) fn build(hir: &HirFile, opts: &Options) -> RenameMap {
    // Group top-level functions by name, preserving declaration order.
    // Only functions that will actually be emitted participate, so a lone
    // survivor of a partially-lowered overload set keeps its bare name.
    let mut by_name: BTreeMap<&str, Vec<DefId>> = BTreeMap::new();
    for &item in &hir.items {
        if let Some(def @ Def::Function(f)) = hir.defs.get(item.0 as usize)
            && will_emit(def, opts)
        {
            by_name.entry(f.name.as_str()).or_default().push(item);
        }
    }

    let mut map = RenameMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();

    // Unique names first so they get to keep their spelling.
    for (name, ids) in &by_name {
        if ids.len() == 1 {
            used.insert((*name).to_string());
            map.insert(ids[0], (*name).to_string());
        }
    }
    for (name, ids) in &by_name {
        if ids.len() == 1 {
            continue;
        }
        for (i, &id) in ids.iter().enumerate() {
            let mut candidate = if i == 0 {
                (*name).to_string()
            } else {
                format!("{name}_{}", i + 1)
            };
            let mut k = i + 1;
            while used.contains(&candidate) {
                k += 1;
                candidate = format!("{name}_{k}");
            }
            used.insert(candidate.clone());
            map.insert(id, candidate);
        }
    }
    map
}
