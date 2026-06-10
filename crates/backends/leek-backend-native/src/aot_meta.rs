//! AOT dispatch-table metadata.
//!
//! The JIT fills the runtime's lambda/method/class dispatch tables *post-finalize*
//! (see the `NativeEmit::Jit` arm). AOT can't do that — the binary runs as a
//! separate process — so we compute the same tables at compile time, serialize
//! them into the executable, and reinstall them at startup via
//! [`leek_aot_install`]. Function *addresses* (the one thing not knowable until
//! link time) come from the object's `leek_uniform_{idx}` symbols, which a
//! generated C harness passes in.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use leek_mir::ir::{MirProgram, Rvalue, Statement};

/// All dispatch tables, with maps flattened to `Vec<(K, V)>` so non-string map
/// keys round-trip cleanly through JSON.
#[derive(Serialize, Deserialize, Default)]
pub struct AotMeta {
    method_resolve: Vec<(u32, String, usize)>,
    static_init: Vec<((u32, String), usize)>,
    user_fn_idx: Vec<(u32, usize)>,
    exact_arity: Vec<u32>,
    class_string_method: Vec<(u32, usize)>,
    lambda_byref: Vec<(usize, Vec<bool>)>,
    class_parent: Vec<(u32, Option<(u32, String)>)>,
    class_ctor_thunk: Vec<(u32, usize)>,
    class_reflect: Vec<(u32, Vec<(String, Vec<String>)>)>,
    /// `(function index, total param count incl. captures)` for every uniform-ABI
    /// function (lambda / thunk / value-method). The harness registers each by
    /// taking the address of `leek_uniform_{idx}`.
    lambda_entries: Vec<(usize, usize)>,
}

impl AotMeta {
    /// Build the metadata from a lowered program and the dispatch tables
    /// [`define_program`](crate::define_program) returned. Mirrors exactly what
    /// the JIT computes after finalize.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build(
        program: &MirProgram,
        lambda_funcs: &HashMap<usize, (cranelift_module::FuncId, usize)>,
        method_resolve: HashMap<u32, HashMap<String, usize>>,
        static_init: HashMap<(u32, String), usize>,
        user_fn_idx: HashMap<u32, usize>,
        exact_arity: HashSet<u32>,
        class_string_method: HashMap<u32, usize>,
        class_thunks: &HashMap<u32, usize>,
    ) -> Self {
        // Per-lambda `@`-by-ref user-param masks (captures excluded).
        let mut ncaptures: HashMap<usize, usize> = HashMap::new();
        for f in &program.functions {
            for b in &f.blocks {
                for s in &b.statements {
                    if let Statement::Assign(
                        _,
                        Rvalue::MakeLambda {
                            function_idx,
                            captures,
                        },
                    ) = s
                    {
                        ncaptures.insert(*function_idx, captures.len());
                    }
                }
            }
        }
        let lambda_byref: Vec<(usize, Vec<bool>)> = lambda_funcs
            .keys()
            .map(|&idx| {
                let f = &program.functions[idx];
                let nc = ncaptures.get(&idx).copied().unwrap_or(0);
                let mask = f
                    .params
                    .iter()
                    .skip(nc)
                    .map(|p| f.locals[p.0 as usize].is_by_ref)
                    .collect();
                (idx, mask)
            })
            .collect();

        let class_parent: Vec<(u32, Option<(u32, String)>)> = program
            .classes
            .iter()
            .map(|c| {
                let parent = c
                    .parent_def
                    .and_then(|pd| program.class(pd).map(|pc| (pd.0, pc.name.clone())));
                (c.def_id.0, parent)
            })
            .collect();

        let class_reflect: Vec<(u32, Vec<(String, Vec<String>)>)> =
            crate::translate::reflect_name_tables(program)
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect();

        let lambda_entries: Vec<(usize, usize)> = lambda_funcs
            .iter()
            .map(|(&idx, &(_, arity))| (idx, arity))
            .collect();

        Self {
            method_resolve: method_resolve
                .into_iter()
                .flat_map(|(cls, methods)| methods.into_iter().map(move |(m, idx)| (cls, m, idx)))
                .collect(),
            static_init: static_init.into_iter().collect(),
            user_fn_idx: user_fn_idx.into_iter().collect(),
            exact_arity: exact_arity.into_iter().collect(),
            class_string_method: class_string_method.into_iter().collect(),
            lambda_byref,
            class_parent,
            class_ctor_thunk: class_thunks.iter().map(|(&k, &v)| (k, v)).collect(),
            class_reflect,
            lambda_entries,
        }
    }

    /// `(function index, param count)` for each uniform-ABI function the harness
    /// must register an address for.
    pub fn lambda_entries(&self) -> &[(usize, usize)] {
        &self.lambda_entries
    }

    /// Serialize to a JSON blob for embedding in the executable.
    pub fn to_blob(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Reinstall every dispatch table into the runtime thread-locals, then
    /// publish the uniform-function addresses (`idx → (addr, arity)`).
    fn install(&self, lambda_addrs: HashMap<usize, (*const u8, usize)>) {
        use crate::runtime;
        let mut method_resolve: HashMap<u32, HashMap<String, usize>> = HashMap::new();
        for (cls, m, idx) in &self.method_resolve {
            method_resolve
                .entry(*cls)
                .or_default()
                .insert(m.clone(), *idx);
        }
        runtime::set_method_resolve(method_resolve);
        runtime::set_static_init(self.static_init.iter().cloned().collect());
        runtime::set_user_fn_idx(self.user_fn_idx.iter().copied().collect());
        runtime::set_user_fn_exact_arity(
            self.exact_arity.iter().copied().collect::<HashSet<u32>>(),
        );
        runtime::set_class_string_method(self.class_string_method.iter().copied().collect());
        runtime::set_lambda_byref(self.lambda_byref.iter().cloned().collect());
        runtime::set_class_parent(self.class_parent.iter().cloned().collect());
        runtime::set_class_ctor_thunk(self.class_ctor_thunk.iter().map(|&(k, v)| (k, v)).collect());
        runtime::set_class_reflect(
            self.class_reflect
                .iter()
                .map(|(k, v)| (*k, v.iter().cloned().collect()))
                .collect(),
        );
        runtime::set_lambda_fns(lambda_addrs);
    }
}

/// One uniform-function address the harness registers: its function index, the
/// linked `leek_uniform_{idx}` pointer, and its param count. `#[repr(C)]` so the
/// generated C `main` can build the array.
#[repr(C)]
pub struct LeekLambdaEntry {
    pub idx: u64,
    pub func: *const u8,
    pub arity: u64,
}

/// Reinstall the AOT dispatch tables at process startup. Called by the generated
/// C harness before `leek_main`.
///
/// # Safety
/// `blob`/`blob_len` must describe the JSON metadata emitted for this program,
/// and `entries`/`n_entries` the matching `leek_uniform_{idx}` address array.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn leek_aot_install(
    blob: *const u8,
    blob_len: usize,
    entries: *const LeekLambdaEntry,
    n_entries: usize,
) {
    let bytes = unsafe { std::slice::from_raw_parts(blob, blob_len) };
    let meta: AotMeta = serde_json::from_slice(bytes).unwrap_or_default();
    let mut addrs: HashMap<usize, (*const u8, usize)> = HashMap::new();
    let entry_slice = unsafe { std::slice::from_raw_parts(entries, n_entries) };
    for e in entry_slice {
        addrs.insert(e.idx as usize, (e.func, e.arity as usize));
    }
    meta.install(addrs);
}
