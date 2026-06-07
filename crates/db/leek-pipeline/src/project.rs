//! Re-exported from [`leek_project`] — see that crate for the unified project model.

pub use leek_project::{
    LoadedProjectFile, Project, ProjectError, ProjectIndex, SourceInput, walk_leek_files,
};

use crate::Input;
use leek_span::FeatureFlags;

// Project-load → pipeline `Input`. The project model carries no experimental
// flags of its own, so this boundary reads them from the environment once (via
// `FeatureFlags::from_env`) and threads the result — rather than each pass
// re-reading env. A caller that wants explicit flags builds `Input` directly.

impl From<SourceInput> for Input {
    fn from(s: SourceInput) -> Self {
        Self {
            source: s.source,
            text: s.text.into(),
            version_byte: s.version_byte,
            strict: s.strict,
            flags: FeatureFlags::from_env(),
        }
    }
}

impl From<&SourceInput> for Input {
    fn from(s: &SourceInput) -> Self {
        Self {
            source: s.source,
            text: s.text.clone().into(),
            version_byte: s.version_byte,
            strict: s.strict,
            flags: FeatureFlags::from_env(),
        }
    }
}

impl From<LoadedProjectFile> for Input {
    fn from(f: LoadedProjectFile) -> Self {
        Self {
            source: f.source,
            text: f.text.into(),
            version_byte: f.version_byte,
            strict: f.strict,
            flags: FeatureFlags::from_env(),
        }
    }
}
