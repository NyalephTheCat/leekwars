//! Backends present in this toolchain build.

use leek_manifest::BackendKind;

/// Backend implementations linked into the workspace (add kinds here when a
/// crate lands). Used by the test corpus and `miku` to decide what to run.
pub const LINKED: &[BackendKind] = &[BackendKind::Interp, BackendKind::Java, BackendKind::Native];

/// True when the backend crate is part of this build.
pub fn is_linked(kind: BackendKind) -> bool {
    LINKED.contains(&kind)
}
