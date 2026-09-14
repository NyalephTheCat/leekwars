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
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(payload) => {
            report_panic(label, &*payload);
            T::default()
        }
    }
}

/// [`guard`] for a result type that has no [`Default`] — notably a
/// `Result` that can carry a refusal message
/// ([`Refusable`](crate::handlers::refusal::Refusable)). On panic this
/// returns `on_panic`, which callers should set to the type's "no
/// result" value so a panicking handler degrades the same way
/// [`guard`] does, rather than surfacing as a refusal the user never
/// triggered.
pub fn guard_with<T>(label: &str, on_panic: T, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(payload) => {
            report_panic(label, &*payload);
            on_panic
        }
    }
}

/// Report a contained panic at error level, **always** — a swallowed one
/// reaches the user as a hover that silently does nothing, and their bug
/// report then contains nothing either. Warnings and above are mirrored to
/// the editor by [`crate::log`], so this lands in the output channel too.
///
/// `catch_unwind` hands back the payload `panic!` was given: `&str` for a
/// literal message, `String` for a formatted one. Anything else (a
/// `panic_any`) has no printable form, so say so rather than drop the line.
fn report_panic(label: &str, payload: &(dyn std::any::Any + Send)) {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>");
    tracing::error!(
        handler = label,
        panic = message,
        "handler panicked; returning empty result"
    );
}

#[cfg(test)]
mod tests {
    use super::{guard, guard_with};

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
        let _serialised = crate::log::test_lock();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let opt: Option<i32> = guard("boom", || panic!("kaboom"));
        let vec: Vec<i32> = guard("boom", || panic!("kaboom"));
        std::panic::set_hook(prev);
        assert_eq!(opt, None);
        assert!(vec.is_empty());
    }

    #[test]
    fn guard_with_returns_the_fallback_on_panic() {
        // The rename handlers return `Result<Option<_>, Refusal>`, which
        // has no `Default`. A panic there must still degrade to "no
        // result" rather than escaping and aborting the server — and
        // must not be reported to the user as a refusal.
        let _serialised = crate::log::test_lock();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r: Result<Option<i32>, String> = guard_with("boom", Ok(None), || panic!("kaboom"));
        let ok: Result<Option<i32>, String> = guard_with("ok", Ok(None), || Ok(Some(7)));
        std::panic::set_hook(prev);
        assert_eq!(r, Ok(None));
        assert_eq!(ok, Ok(Some(7)));
    }

    /// The panic payload is what makes a bug report actionable, and it must
    /// arrive without anyone having set `LEEK_LSP_LOG` first: a user who
    /// hits a panicking hover reports "hover does nothing", and the server
    /// has one chance to say what actually happened.
    #[test]
    fn panics_are_reported_with_their_payload_and_handler() {
        let _serialised = crate::log::test_lock();
        let mut rx = crate::log::attach_client_sink();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        tracing::subscriber::with_default(client_mirror(), || {
            let _: Option<i32> = guard("hover", || panic!("kaboom in {}", "hover"));
            let _: Result<Option<i32>, String> =
                guard_with("rename", Ok(None), || panic!("static message"));
        });
        std::panic::set_hook(prev);

        let first = rx.try_recv().expect("the panic was reported");
        assert_eq!(first.kind, tower_lsp::lsp_types::MessageType::ERROR);
        assert!(
            first.text.contains("kaboom in hover") && first.text.contains("handler=hover"),
            "the formatted payload and the handler label must both survive; got {first:?}"
        );

        let second = rx.try_recv().expect("the second panic was reported");
        assert!(
            second.text.contains("static message") && second.text.contains("handler=rename"),
            "a `&str` payload must survive too; got {second:?}"
        );
    }

    /// A subscriber with only the client mirror: the assertions read the
    /// records back off the channel instead of scraping stderr.
    fn client_mirror() -> impl tracing::Subscriber {
        use tracing_subscriber::layer::SubscriberExt as _;
        tracing_subscriber::registry().with(crate::log::ClientLayer)
    }
}
