//! URI utilities. The MVP doesn't validate URIs beyond their use as
//! document keys — the path itself is only needed when we want to
//! show source-on-disk; we hold the buffer text in memory regardless.

use tower_lsp::lsp_types::Url;

/// `file:` URLs round-trip to a system path; everything else is
/// passed through as-is.
pub fn display(uri: &Url) -> String {
    uri.to_string()
}
