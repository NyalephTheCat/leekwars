//! Per-connection debug session state.

use std::sync::Arc;

use crate::breakpoints::{BreakpointStore, ProgramMap};
use crate::debug::NativeDebugSession;
use crate::target::LaunchConfig;

/// The one synthetic thread the adapter exposes. Leekscript programs
/// are single-threaded, so a fixed id is enough.
pub(crate) const MAIN_THREAD_ID: i64 = 1;

/// Mutable state for a single debug session.
pub(crate) struct Session {
    /// Source breakpoints requested by the client. The session is the single
    /// source of truth: a running debug controller holds a derived copy that
    /// `setBreakpoints` refreshes wholesale.
    pub breakpoints: BreakpointStore,
    /// Which file is which `SourceId` and which lines carry a safepoint, for
    /// the program launched at `configurationDone`. `None` until then —
    /// before that there is nothing to resolve a breakpoint against.
    pub program: Option<ProgramMap>,
    /// Launch configuration captured at `launch`. DAP defers the
    /// actual program start until `configurationDone`, so we stash it
    /// here and consume it there.
    pub pending_launch: Option<LaunchConfig>,
    /// Whether the debuggee has been started.
    pub started: bool,
    /// The active native debug controller (present once a debug launch has
    /// started). Handlers use it to resume/inspect the parked debuggee.
    pub native_debug: Option<Arc<NativeDebugSession>>,
    /// Absolute path of the launched program, for `stackTrace` source refs.
    pub program_path: Option<String>,
    /// The worker running the debuggee. The debuggee never runs on the
    /// request loop's thread, so a long run (a whole fight) can't leave
    /// `terminate`/`disconnect` unanswered.
    pub run_thread: Option<std::thread::JoinHandle<()>>,
}

/// How long [`Session::stop`] waits for a detached debuggee to finish before
/// giving up on it. A detached worker only has to run out the program with
/// every stop switched off, so this is generous; abandoning it beats leaving
/// the adapter unable to exit because the debuggee has real work left.
const WORKER_EXIT: std::time::Duration = std::time::Duration::from_secs(5);

impl Session {
    pub(crate) fn new() -> Self {
        Self {
            breakpoints: BreakpointStore::default(),
            program: None,
            pending_launch: None,
            started: false,
            native_debug: None,
            program_path: None,
            run_thread: None,
        }
    }

    /// End the session: release the debuggee, uninstall the process-global
    /// debug hook, and wait for the worker to leave.
    ///
    /// `disconnect`/`terminate` — and a client that simply closes the pipe —
    /// arrive as often while the debuggee is parked at a breakpoint as while
    /// it is running. Dropping the session there would drop the worker's
    /// `JoinHandle` unjoined and leave that thread parked forever with the
    /// hook still installed, so the run is detached first and only then
    /// joined.
    pub(crate) fn stop(&mut self) {
        if let Some(debug) = self.native_debug.take() {
            debug.detach();
            // Only a session that installed the process-global hook clears
            // it, and only its own: a `noDebug` session never had one, and
            // the slot may already belong to whoever came next.
            let hook: Arc<dyn leek_backend_native::DebugHook> = debug;
            leek_backend_native::clear_debug_hook(&hook);
        }
        let Some(worker) = self.run_thread.take() else {
            return;
        };
        // `JoinHandle` has no timed join; poll instead so a program that
        // ignores the detach cannot wedge the adapter's exit.
        let deadline = std::time::Instant::now() + WORKER_EXIT;
        while !worker.is_finished() {
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = worker.join();
    }
}
