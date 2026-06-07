//! Per-connection debug session state.

use std::collections::HashMap;
use std::sync::Arc;

use crate::debug::NativeDebugSession;
use crate::target::LaunchConfig;

/// The one synthetic thread the adapter exposes. Leekscript programs
/// are single-threaded, so a fixed id is enough.
pub(crate) const MAIN_THREAD_ID: i64 = 1;

/// Mutable state for a single debug session.
pub(crate) struct Session {
    /// Source breakpoints requested by the client, keyed by source
    /// path. Recorded verbatim; not yet honored by the target.
    pub breakpoints: HashMap<String, Vec<i64>>,
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
}

impl Session {
    pub(crate) fn new() -> Self {
        Self {
            breakpoints: HashMap::new(),
            pending_launch: None,
            started: false,
            native_debug: None,
            program_path: None,
        }
    }
}
