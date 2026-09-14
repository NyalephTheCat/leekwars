//! The server's single logging seam.
//!
//! Everything the language server has to say goes through [`tracing`]:
//!
//! * the **binaries** install the sink ([`init`]) — a library that reaches
//!   for the global subscriber itself cannot be embedded, which is the same
//!   mistake `run_stdio`'s old `process::exit` made;
//! * the stderr layer writes to the stream `vscode-languageclient` captures
//!   and forwards to the editor's "Leekscript" output channel, exactly as
//!   before;
//! * [`ClientLayer`] additionally mirrors **warnings and errors** to the
//!   editor itself over `window/logMessage`, so a failure the user can feel
//!   (a formatter refusal, a project that failed to index, a handler that
//!   panicked) is reported somewhere the user can actually read it. Anything
//!   below `WARN` stays on stderr: `publishDiagnostics` traces once per
//!   keystroke and would flood the channel.
//!
//! `LEEK_LSP_LOG` is read **once**, in [`init`]. The `trace_enabled()` this
//! replaces re-read the environment on every call, including on the panic
//! path of every request.
//!
//! ## `LEEK_LSP_LOG`
//!
//! | value | meaning |
//! |---|---|
//! | unset | `leek_lsp=info` — lifecycle events only |
//! | `trace` / `debug` / `info` / `warn` / `error` | that level, for this crate |
//! | `1` / `true` | `leek_lsp=trace` (accepted for backwards compatibility) |
//! | anything else | a full [`EnvFilter`] directive list, e.g. `leek_lsp=debug,salsa=off` |
//!
//! An unparseable value falls back to the default rather than aborting the
//! server before it can say why.

use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tower_lsp::lsp_types::MessageType;
use tracing::field::{Field, Visit};
use tracing_subscriber::EnvFilter;

/// Filter applied when `LEEK_LSP_LOG` is unset or unparseable: the
/// lifecycle one-shots, nothing per-keystroke.
const DEFAULT_FILTER: &str = "leek_lsp=info";

/// One log record on its way to the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientMessage {
    /// `window/logMessage` severity.
    pub kind: MessageType,
    /// The rendered event: message text followed by `key=value` fields.
    pub text: String,
    /// Set by an event carrying `notify = true`. The drain additionally
    /// raises the **first** such message as a `window/showMessage`, because
    /// some failures (a refused "Format Document") are otherwise invisible:
    /// the command simply appears to do nothing.
    pub notify: bool,
}

/// The live mirror, installed by [`attach_client_sink`] when a server is
/// constructed. A `Mutex<Option<_>>` rather than a `OnceLock` so a second
/// server in the same process (tests) replaces a stale sender instead of
/// logging into a dead channel.
static SINK: Mutex<Option<UnboundedSender<ClientMessage>>> = Mutex::new(None);

/// Route warnings and errors to the returned receiver until the next call.
///
/// The channel is **unbounded** on purpose: `tracing` events are emitted
/// from synchronous handler code running on a tokio worker, so the send must
/// never block or await — a bounded channel would risk deadlocking the very
/// request it is reporting on.
pub(crate) fn attach_client_sink() -> UnboundedReceiver<ClientMessage> {
    let (tx, rx) = unbounded_channel();
    *lock_sink() = Some(tx);
    rx
}

/// The sink lock, ignoring poisoning: a poisoned mirror should degrade to
/// "stderr only", never take the server down with it.
fn lock_sink() -> MutexGuard<'static, Option<UnboundedSender<ClientMessage>>> {
    SINK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Install the stderr subscriber plus the `window/logMessage` mirror.
///
/// Called by `bins/leek-lsp` and by `miku lsp` before
/// [`run_stdio`](crate::run_stdio). Doing it twice is a no-op, so a test
/// that starts a second server in-process keeps the first subscriber.
pub fn init() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let stderr = tracing_subscriber::fmt::layer()
        // The editor reads this as plain text out of a pipe; escape codes
        // and wall-clock stamps only make the output channel harder to read.
        .with_ansi(false)
        .without_time()
        .with_writer(std::io::stderr);

    let _ = tracing_subscriber::registry()
        .with(env_filter())
        .with(stderr)
        .with(ClientLayer)
        .try_init();
}

/// Build the filter from `LEEK_LSP_LOG`, reading the variable once.
fn env_filter() -> EnvFilter {
    EnvFilter::try_new(filter_spec(std::env::var("LEEK_LSP_LOG").ok().as_deref()))
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

/// `LEEK_LSP_LOG`'s value as an [`EnvFilter`] directive list.
///
/// A bare level applies to this crate only — `LEEK_LSP_LOG=trace` has always
/// meant "the server's traces", and turning on salsa's and tower-lsp's at the
/// same time would bury them.
fn filter_spec(value: Option<&str>) -> String {
    match value.map(str::trim) {
        None | Some("") => DEFAULT_FILTER.to_string(),
        // The historical spelling, from when this was a boolean.
        Some("1" | "true") => "leek_lsp=trace".to_string(),
        Some(level @ ("trace" | "debug" | "info" | "warn" | "error")) => {
            format!("leek_lsp={level}")
        }
        Some(directives) => directives.to_string(),
    }
}

/// The [`tracing`] layer that mirrors `WARN` and above to the editor.
pub(crate) struct ClientLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ClientLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let kind = match *event.metadata().level() {
            tracing::Level::ERROR => MessageType::ERROR,
            tracing::Level::WARN => MessageType::WARNING,
            // Info and below stay on stderr. `publishDiagnostics` traces on
            // every keystroke; mirroring it would flood the output channel.
            _ => return,
        };
        let guard = lock_sink();
        let Some(tx) = guard.as_ref() else { return };
        let mut render = Render::default();
        event.record(&mut render);
        // A closed receiver means the server it belonged to is gone; the
        // stderr layer still has the record.
        let _ = tx.send(ClientMessage {
            kind,
            text: format!("leek-lsp: {}{}", render.message, render.fields),
            notify: render.notify,
        });
    }
}

/// Flattens an event into the one line the editor shows.
#[derive(Default)]
struct Render {
    message: String,
    fields: String,
    notify: bool,
}

impl Visit for Render {
    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "notify" {
            self.notify = value;
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // The `message` field is `fmt::Arguments`; its `Debug` is the
            // formatted text itself, unquoted.
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// Serialises tests that install the global [`SINK`] (and, in
/// [`crate::util::guard`], the global panic hook): cargo runs them on
/// several threads, and a second `attach_client_sink` would otherwise steal
/// the first test's receiver.
#[cfg(test)]
pub(crate) fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use tower_lsp::lsp_types::MessageType;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::{ClientLayer, DEFAULT_FILTER, EnvFilter, attach_client_sink, filter_spec};

    #[test]
    fn bare_levels_scope_to_this_crate_and_junk_falls_back() {
        assert_eq!(filter_spec(None), DEFAULT_FILTER);
        assert_eq!(filter_spec(Some("")), DEFAULT_FILTER);
        assert_eq!(filter_spec(Some("trace")), "leek_lsp=trace");
        assert_eq!(filter_spec(Some(" debug ")), "leek_lsp=debug");
        // The boolean spelling the variable used to have.
        assert_eq!(filter_spec(Some("1")), "leek_lsp=trace");
        assert_eq!(filter_spec(Some("true")), "leek_lsp=trace");
        // Anything else is passed through as a directive list…
        assert_eq!(
            filter_spec(Some("leek_lsp=debug,salsa=off")),
            "leek_lsp=debug,salsa=off"
        );
        // …and an unparseable one must not take the server down: the filter
        // falls back to the default instead of panicking.
        assert!(EnvFilter::try_new(filter_spec(Some("=<>="))).is_err());
        assert_eq!(
            EnvFilter::try_new(filter_spec(Some("=<>=")))
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
                .to_string(),
            EnvFilter::new(DEFAULT_FILTER).to_string()
        );
    }

    /// A warning reaches the editor; a trace does not.
    #[test]
    fn warnings_and_errors_mirror_to_the_client() {
        let _serialised = super::test_lock();
        let mut rx = attach_client_sink();
        let subscriber = tracing_subscriber::registry().with(ClientLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(uri = "file:///a.leek", "didOpen");
            tracing::warn!(notify = true, uri = "file:///a.leek", "refusing to format");
            tracing::error!(handler = "hover", "handler panicked");
        });

        let warn = rx.try_recv().expect("the warning reached the client sink");
        assert_eq!(warn.kind, MessageType::WARNING);
        assert!(warn.notify, "`notify = true` must ask for a showMessage");
        assert_eq!(warn.text, "leek-lsp: refusing to format uri=file:///a.leek");

        let error = rx.try_recv().expect("the error reached the client sink");
        assert_eq!(error.kind, MessageType::ERROR);
        assert!(!error.notify);
        assert_eq!(error.text, "leek-lsp: handler panicked handler=hover");

        // The trace never arrived — only stderr sees per-keystroke events.
        assert!(rx.try_recv().is_err());
    }
}
