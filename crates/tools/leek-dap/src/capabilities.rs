//! The adapter's advertised [`Capabilities`].

use dap::types::Capabilities;

/// What this adapter supports. Stepping and line breakpoints need no
/// capability flag and work; conditions, logpoints and `breakpointLocations`
/// are advertised because they are implemented.
///
/// `supportsHitConditionalBreakpoints` deliberately stays off. Hit counts *are*
/// honoured when a client sends one anyway, but the count they would advertise
/// is not yet trustworthy: a line whose single statement lowers to several MIR
/// statements arrives several times per pass (#408), so `>3` on such a line
/// counts lowered statements rather than visits. Advertising a flag whose
/// answers are arbitrary is worse than leaving it absent.
///
/// `supportsEvaluateForHovers` stays off for the same reason: `evaluate` works,
/// but a hover sends whatever is under the cursor, and the expression language
/// here is a subset (no calls, no indexing).
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        supports_configuration_done_request: Some(true),
        supports_terminate_request: Some(true),
        supports_conditional_breakpoints: Some(true),
        supports_log_points: Some(true),
        supports_breakpoint_locations_request: Some(true),
        ..Default::default()
    }
}
