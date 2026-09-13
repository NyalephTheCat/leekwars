//! Panic containment for request handlers.

/// Run a synchronous request-handler body, catching any panic so that one
/// malformed request (or a latent bug in analysis over an incomplete buffer)
/// can't take down the whole language server. The server is long-running and
/// processes untrusted buffers on every keystroke, so a panic must fail the
/// single request, not the process. On panic this returns the result type's
/// default (`None` / empty `Vec`), which clients treat as "no result".
///
/// tower-lsp 0.20 does not catch handler panics: an unwind escaping a handler
/// aborts the serve future and the process with it.
///
/// `tokio::sync::Mutex` does not poison on panic, so a guard held across the
/// caught unwind is released cleanly when it drops.
pub fn guard<T: Default>(label: &str, f: impl FnOnce() -> T) -> T {
    let Ok(value) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) else {
        if crate::trace_enabled() {
            eprintln!("leek-lsp: handler `{label}` panicked; returning empty result");
        }
        return T::default();
    };
    value
}

#[cfg(test)]
mod tests {
    use super::guard;

    #[test]
    fn guard_returns_value_when_no_panic() {
        let r: Option<i32> = guard("ok", || Some(7));
        assert_eq!(r, Some(7));
    }

    #[test]
    fn guard_returns_default_on_panic() {
        // A panicking handler must degrade to the type default (`None` /
        // empty), not unwind out of the request and crash the server.
        // Silence the default panic hook so the expected panic isn't noisy.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let opt: Option<i32> = guard("boom", || panic!("kaboom"));
        let vec: Vec<i32> = guard("boom", || panic!("kaboom"));
        std::panic::set_hook(prev);
        assert_eq!(opt, None);
        assert!(vec.is_empty());
    }
}
