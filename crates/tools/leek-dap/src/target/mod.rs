//! The execution-engine seam.
//!
//! [`DebugTarget`] is the boundary between the protocol layer and
//! whatever actually runs the program. Today the only implementation
//! is [`native::NativeTarget`], which compiles via the Cranelift
//! backend and runs to completion. When live debugging lands, the
//! pausing/stepping/inspection methods grow here (mirroring the
//! interpreter's existing profiler hook), and the protocol handlers
//! stay unchanged.

pub(crate) mod fight;
pub(crate) mod native;

use std::path::PathBuf;

use serde::Deserialize;

/// Adapter-specific `launch` arguments — the `additional_data` blob of
/// a DAP `launch` request, contributed by the editor's launch config
/// (e.g. VS Code `launch.json`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchConfig {
    /// Path to the `.leek` program to debug.
    pub program: PathBuf,
    /// Leekscript language version (1–4). Defaults to the latest.
    #[serde(default)]
    pub version: Option<u8>,
    /// Whether to compile in strict mode.
    #[serde(default)]
    pub strict: bool,
    /// Break at the program's first statement.
    #[serde(default)]
    pub stop_on_entry: bool,
    /// Run without debugging (native, no instrumentation/breakpoints).
    #[serde(default)]
    pub no_debug: bool,

    // --- fight debugging ---
    /// When set, `program` is debugged *inside* the fight this scenario file
    /// describes (`.toml`/`.json`) instead of running standalone. Breakpoints
    /// in `program` fire during the fight's turn loop.
    #[serde(default)]
    pub scenario: Option<PathBuf>,
    /// Which entity id `program` controls in the scenario. Defaults to the
    /// entity whose `ai` is `program`, else the first entity.
    #[serde(default)]
    pub fight_entity: Option<i64>,
    /// A `[profiles.<name>]` block to apply to the scenario first.
    #[serde(default)]
    pub profile: Option<String>,
    /// Override the fight seed.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Override the turn limit.
    #[serde(default)]
    pub max_turns: Option<u32>,
}

/// The result of running the debuggee to completion.
pub(crate) struct RunOutcome {
    /// Human-readable program output / result, surfaced as an `output`
    /// event. Empty on a clean run with no result text.
    pub output: String,
    /// Process exit code: `0` on success, non-zero on a compile or
    /// runtime failure.
    pub exit_code: i64,
}

impl RunOutcome {
    /// A failed run carrying a diagnostic message (routed to stderr).
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self {
            output: message.into(),
            exit_code: 1,
        }
    }
}
