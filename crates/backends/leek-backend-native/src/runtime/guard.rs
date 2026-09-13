//! Panic containment for the C-ABI shims.
//!
//! JIT'd code calls the `leek_*` shims through `extern "C"` functions. A Rust
//! panic may not unwind across that boundary — it aborts the whole host
//! process (generator, LSP, DAP, test runner). Every shim is therefore declared
//! through [`shim!`], which runs the body under [`shim_guard`]: a panic is
//! caught, recorded as the `INTERNAL_PANIC` runtime error (surfaced by `run()`
//! as `NativeError::Runtime`), and the shim returns an inert default so the
//! JIT'd caller keeps a valid value and winds down.
//!
//! Since every shim catches its own panics, no unwind ever crosses a JIT frame,
//! even when a shim re-enters JIT'd code (a higher-order builtin's callback)
//! which then calls another shim that panics.

use super::{handle, raise_runtime_error};
use leek_runtime::Value;

/// The runtime error recorded when a shim panics.
pub const INTERNAL_PANIC: &str = "INTERNAL_PANIC";

/// A shim return type with an inert value to hand back to JIT'd code after a
/// caught panic.
pub(crate) trait ShimReturn {
    /// The value returned in place of the panicked shim's result.
    fn on_panic() -> Self;
}

impl ShimReturn for () {
    fn on_panic() -> Self {}
}

impl ShimReturn for i64 {
    fn on_panic() -> Self {
        0
    }
}

impl ShimReturn for f64 {
    fn on_panic() -> Self {
        0.0
    }
}

impl ShimReturn for *mut Value {
    /// A fresh boxed `null`, never a null pointer: JIT'd code dereferences
    /// every handle it receives.
    fn on_panic() -> Self {
        handle(Value::Null)
    }
}

/// Run a shim body, converting a panic into the `INTERNAL_PANIC` runtime error
/// plus [`ShimReturn::on_panic`]. The non-panicking path is a plain call.
pub(crate) fn shim_guard<R: ShimReturn>(body: impl FnOnce() -> R) -> R {
    // `AssertUnwindSafe`: after a caught panic the run is already failed
    // (`INTERNAL_PANIC` is recorded, and the error flag makes the remaining
    // JIT'd code wind down), so observing half-updated state is harmless.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|_| {
        raise_runtime_error(INTERNAL_PANIC);
        R::on_panic()
    })
}

/// Declare a C-ABI runtime shim: adds `#[unsafe(no_mangle)]` and wraps the body
/// in [`shim_guard`] so a panic can't abort the host. Use for every
/// `extern "C"` function JIT'd or AOT code calls.
macro_rules! shim {
    (
        $(#[$attr:meta])*
        $vis:vis extern "C" fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?
        $body:block
    ) => {
        $(#[$attr])*
        #[unsafe(no_mangle)]
        $vis extern "C" fn $name($($arg: $ty),*) $(-> $ret)? {
            $crate::runtime::guard::shim_guard(move || $body)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_body_records_internal_panic_and_returns_the_default() {
        crate::runtime::reset_runtime_error();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let n: i64 = shim_guard(|| panic!("boom"));
        let h: *mut Value = shim_guard(|| panic!("boom"));
        std::panic::set_hook(hook);
        assert_eq!(n, 0);
        // SAFETY: `on_panic` returns a live boxed handle.
        assert!(matches!(unsafe { &*h }, Value::Null));
        assert_eq!(
            crate::runtime::take_runtime_error().as_deref(),
            Some(INTERNAL_PANIC)
        );
    }

    #[test]
    fn a_normal_body_passes_its_value_through() {
        crate::runtime::reset_runtime_error();
        assert_eq!(shim_guard(|| 7i64), 7);
        assert_eq!(crate::runtime::take_runtime_error(), None);
    }
}
