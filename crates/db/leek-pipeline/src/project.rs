//! Re-exported from [`leek_project`] — see that crate for the unified project
//! model, the pipeline [`Input`] and the `SourceInput`/`LoadedProjectFile`
//! conversions into it.

pub use leek_project::{
    Input, LoadedProjectFile, Project, ProjectError, ProjectIndex, SourceInput, walk_leek_files,
};
