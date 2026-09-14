//! The adapter's advertised [`Capabilities`].

use dap::types::Capabilities;

/// What this adapter supports. Stepping and line breakpoints need no
/// capability flag and work; what stays off is what is genuinely not
/// implemented — conditional breakpoints, hit counts and logpoints — since a
/// client that believes the flag will send conditions the adapter silently
/// ignores.
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        supports_configuration_done_request: Some(true),
        supports_terminate_request: Some(true),
        ..Default::default()
    }
}
