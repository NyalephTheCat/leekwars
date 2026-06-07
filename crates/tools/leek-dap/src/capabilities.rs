//! The adapter's advertised [`Capabilities`].

use dap::types::Capabilities;

/// What this adapter supports. Conservative for now: we honor the
/// `configurationDone` handshake and the `terminate` request. Stepping
/// and data/conditional breakpoints stay off until the target seam
/// learns to pause.
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        supports_configuration_done_request: Some(true),
        supports_terminate_request: Some(true),
        ..Default::default()
    }
}
