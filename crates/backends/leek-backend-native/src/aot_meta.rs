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
use std::ffi::c_int;

use serde::{Deserialize, Serialize};

use leek_mir::ir::{MirProgram, Rvalue, Statement};

use crate::NativeError;

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
    static_field_owner: Vec<(u32, Vec<(String, u32)>)>,
    static_method_resolve: Vec<(u32, Vec<(String, usize)>)>,
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

        let (owner_map, static_methods) = crate::translate::static_member_tables(program);
        let static_field_owner: Vec<(u32, Vec<(String, u32)>)> = owner_map
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect();
        let static_method_resolve: Vec<(u32, Vec<(String, usize)>)> = static_methods
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
            static_field_owner,
            static_method_resolve,
            lambda_entries,
        }
    }

    /// `(function index, param count)` for each uniform-ABI function the harness
    /// must register an address for.
    pub fn lambda_entries(&self) -> &[(usize, usize)] {
        &self.lambda_entries
    }

    /// Serialize to a JSON blob for embedding in the executable.
    ///
    /// Fallible on purpose: an empty `Vec` used to be indistinguishable from a
    /// serialization failure, so a broken blob got embedded and the produced
    /// executable silently ran with empty dispatch tables.
    pub fn to_blob(&self) -> Result<Vec<u8>, NativeError> {
        serde_json::to_vec(self).map_err(|e| {
            NativeError::compile(format!("serializing the AOT dispatch-table metadata: {e}"))
        })
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
        runtime::set_static_field_owner(
            self.static_field_owner
                .iter()
                .map(|(k, v)| (*k, v.iter().cloned().collect()))
                .collect(),
        );
        runtime::set_static_method_resolve(
            self.static_method_resolve
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

/// `leek_aot_install` succeeded.
pub const LEEK_AOT_INSTALL_OK: c_int = 0;
/// The metadata blob could not be parsed — a compiler/executable format break.
pub const LEEK_AOT_INSTALL_BAD_BLOB: c_int = 1;
/// A null pointer was passed with a non-zero length.
pub const LEEK_AOT_INSTALL_BAD_ARGS: c_int = 2;

/// Reinstall the AOT dispatch tables at process startup. Called by the generated
/// C harness before `leek_main`; the harness aborts on a non-zero return.
///
/// Returns [`LEEK_AOT_INSTALL_OK`], [`LEEK_AOT_INSTALL_BAD_BLOB`] or
/// [`LEEK_AOT_INSTALL_BAD_ARGS`]. It used to return nothing and parse with
/// `unwrap_or_default()`, so a format break installed EMPTY dispatch tables and
/// the program ran on into whatever that produced.
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
) -> c_int {
    // Neither array is ever legitimately null — the generated C passes two
    // file-scope statics — and `from_raw_parts` requires non-null even for a
    // zero length, so reject both outright.
    if blob.is_null() || entries.is_null() {
        return LEEK_AOT_INSTALL_BAD_ARGS;
    }
    // SAFETY: caller's contract — `blob`/`blob_len` is the metadata blob the
    // AOT emitter wrote into the program object, readable for the process;
    // null was rejected above.
    let bytes = unsafe { std::slice::from_raw_parts(blob, blob_len) };
    let Ok(meta) = serde_json::from_slice::<AotMeta>(bytes) else {
        return LEEK_AOT_INSTALL_BAD_BLOB;
    };
    let mut addrs: HashMap<usize, (*const u8, usize)> = HashMap::new();
    // SAFETY: caller's contract — `entries`/`n_entries` is the emitted
    // `leek_uniform_*` address array, a static in the same object; null was
    // rejected above.
    let entry_slice = unsafe { std::slice::from_raw_parts(entries, n_entries) };
    for e in entry_slice {
        addrs.insert(e.idx as usize, (e.func, e.arity as usize));
    }
    meta.install(addrs);
    LEEK_AOT_INSTALL_OK
}

#[cfg(test)]
mod tests {
    //! The blob format is the only channel between the compiler process and the
    //! produced executable. [`leek_aot_install`] used to parse it with
    //! `unwrap_or_default()`, so a format break silently installed EMPTY
    //! dispatch tables; it now reports the break and the generated C harness
    //! exits on it. Round-trip every table here, and pin that failure code.

    use super::{
        AotMeta, LEEK_AOT_INSTALL_BAD_ARGS, LEEK_AOT_INSTALL_BAD_BLOB, LEEK_AOT_INSTALL_OK,
        LeekLambdaEntry, leek_aot_install,
    };

    fn populated() -> AotMeta {
        AotMeta {
            method_resolve: vec![(7, "m".into(), 3), (7, "n".into(), 4), (9, "m".into(), 5)],
            static_init: vec![((7, "count".into()), 6)],
            user_fn_idx: vec![(1, 2), (2, 8)],
            exact_arity: vec![2],
            class_string_method: vec![(7, 3)],
            lambda_byref: vec![(3, vec![true, false]), (4, vec![])],
            class_parent: vec![(9, Some((7, "C".into()))), (7, None)],
            class_ctor_thunk: vec![(7, 10)],
            class_reflect: vec![(7, vec![("m".into(), vec!["x".into(), "y".into()])])],
            static_field_owner: vec![(9, vec![("count".into(), 7)])],
            static_method_resolve: vec![(9, vec![("sm".into(), 11)])],
            lambda_entries: vec![(3, 2), (4, 1), (10, 0)],
        }
    }

    #[test]
    fn every_dispatch_table_round_trips_through_the_blob() {
        let meta = populated();
        let blob = meta.to_blob().expect("serialize blob");
        let back: AotMeta = serde_json::from_slice(&blob).expect("parse blob");
        assert_eq!(back.method_resolve, meta.method_resolve);
        assert_eq!(back.static_init, meta.static_init);
        assert_eq!(back.user_fn_idx, meta.user_fn_idx);
        assert_eq!(back.exact_arity, meta.exact_arity);
        assert_eq!(back.class_string_method, meta.class_string_method);
        assert_eq!(back.lambda_byref, meta.lambda_byref);
        assert_eq!(back.class_parent, meta.class_parent);
        assert_eq!(back.class_ctor_thunk, meta.class_ctor_thunk);
        assert_eq!(back.class_reflect, meta.class_reflect);
        assert_eq!(back.static_field_owner, meta.static_field_owner);
        assert_eq!(back.static_method_resolve, meta.static_method_resolve);
        assert_eq!(back.lambda_entries(), meta.lambda_entries());
    }

    #[test]
    fn the_empty_metadata_of_the_aot_able_subset_round_trips() {
        // Today's AOT subset (scalars, strings, numeric arrays, direct calls)
        // always emits empty tables — the shape the generated C harness embeds.
        let blob = AotMeta::default().to_blob().expect("serialize blob");
        assert!(!blob.is_empty(), "an empty table set must still serialize");
        let back: AotMeta = serde_json::from_slice(&blob).expect("parse blob");
        assert!(back.lambda_entries().is_empty());
        assert!(back.method_resolve.is_empty());
    }

    /// The sentinel-only array the generated C harness always passes.
    const NO_LAMBDAS: &[LeekLambdaEntry] = &[LeekLambdaEntry {
        idx: 0,
        func: std::ptr::null(),
        arity: 0,
    }];

    #[test]
    fn a_blob_that_does_not_parse_is_reported_not_defaulted() {
        let bad = b"{ not json";
        // SAFETY: both pointers are live slices, described by their lengths.
        let rc = unsafe { leek_aot_install(bad.as_ptr(), bad.len(), NO_LAMBDAS.as_ptr(), 0) };
        assert_eq!(
            rc, LEEK_AOT_INSTALL_BAD_BLOB,
            "a corrupt blob must be reported, not silently defaulted to empty tables"
        );
    }

    #[test]
    fn a_well_formed_blob_installs() {
        let blob = populated().to_blob().expect("serialize blob");
        // SAFETY: both pointers are live slices, described by their lengths.
        let rc = unsafe { leek_aot_install(blob.as_ptr(), blob.len(), NO_LAMBDAS.as_ptr(), 0) };
        assert_eq!(rc, LEEK_AOT_INSTALL_OK);
    }

    #[test]
    fn a_null_pointer_is_reported_rather_than_dereferenced() {
        // SAFETY: passing null is exactly what is under test; the function
        // must reject it before constructing any slice.
        let rc = unsafe { leek_aot_install(std::ptr::null(), 0, NO_LAMBDAS.as_ptr(), 0) };
        assert_eq!(rc, LEEK_AOT_INSTALL_BAD_ARGS);
    }
}
