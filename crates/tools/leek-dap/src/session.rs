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
}
