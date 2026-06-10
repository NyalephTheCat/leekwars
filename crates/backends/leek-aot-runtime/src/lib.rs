//! C-linkable static runtime + harness glue for the AOT native backend.
//!
//! Built once as a `staticlib` (`libleek_aot_runtime.a`), this archive bundles
//! the Leekscript runtime — the `leek_*` shims from `leek-backend-native` and
//! the math builtins from `leek-runtime`, with the Rust standard library — plus
//! the small `extern "C"` glue a generated C `main` calls. The AOT backend then
//! links a compiled program object against this with `cc`, so emitting an
//! executable needs no per-program `cargo` or `rustc` invocation.
//!
//! The runtime shims this archive must export are pulled in transitively (the
//! glue depends on `leek-backend-native`/`leek-runtime`) and force-retained by
//! [`leek_aot_force_link`].

// `#[unsafe(no_mangle)]` is unsafe code; the workspace denies it by default.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char};

use leek_runtime::Value;

/// Per-run runtime initialization, called by the generated `main` before
/// `leek_main`. `strict` is a C boolean (0 / non-0).
#[unsafe(no_mangle)]
pub extern "C" fn leek_aot_setup(strict: i32, op_limit: u64) {
    leek_backend_native::aot::aot_setup(strict != 0, op_limit);
}

/// The runtime error recorded during the run as a freshly-allocated C string,
/// or null if the program completed cleanly. The caller owns the string, but
/// since it only appears on the error-exit path the small leak is harmless.
#[unsafe(no_mangle)]
pub extern "C" fn leek_aot_error() -> *mut c_char {
    match leek_backend_native::aot::aot_take_error() {
        Some(msg) => CString::new(msg).map_or(std::ptr::null_mut(), CString::into_raw),
        None => std::ptr::null_mut(),
    }
}

fn print_value(v: &Value, version: i32) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    leek_runtime::DISPLAY_VERSION.with(|c| c.set(version as u8));
    println!("{v}");
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_aot_print_int(v: i64, version: i32) {
    print_value(&Value::Int(v), version);
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_aot_print_real(v: f64, version: i32) {
    print_value(&Value::Real(v), version);
}

#[unsafe(no_mangle)]
pub extern "C" fn leek_aot_print_bool(v: i64, version: i32) {
    print_value(&Value::Bool(v != 0), version);
}

/// Print a `Ref`-typed result.
///
/// # Safety
/// `ptr` must be the pointer returned by a `Ref`-typed `leek_main`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn leek_aot_print_ref(ptr: *mut Value, version: i32) {
    let v = unsafe { leek_backend_native::aot::aot_finish_ref(ptr) };
    print_value(&v, version);
}

/// References every runtime shim + math builtin so they survive static-archive
/// creation (otherwise rustc may drop the dependency symbols this crate doesn't
/// itself call, leaving the program object's calls unresolved at link time).
/// Never invoked — its mere reachability from a retained `no_mangle` root keeps
/// the referenced symbols in the archive.
#[unsafe(no_mangle)]
pub extern "C" fn leek_aot_force_link() -> usize {
    let mut acc: usize = 0;
    for (_, addr) in leek_backend_native::aot::runtime_symbols() {
        acc ^= addr as usize;
    }
    for b in leek_runtime::math_builtins() {
        acc ^= b.addr as usize;
    }
    acc ^= leek_runtime::ipow_addr().1 as usize;
    // The AOT startup table-installer lives in `leek-backend-native`; reference
    // it so it survives static-archive creation (the program's C `main` calls it).
    acc ^= leek_backend_native::aot_meta::leek_aot_install as *const () as usize;
    acc
}
