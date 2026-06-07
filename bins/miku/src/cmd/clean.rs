//! `miku clean` — remove the `build/` directory.

use std::path::Path;

use anyhow::{Context, Result};

use leek_project::Project;

pub fn run(manifest_path: Option<&Path>, quiet: bool) -> Result<()> {
    let project = Project::discover(manifest_path)?;
    let build = project.build_dir();
    if !build.exists() {
        if !quiet {
            eprintln!("nothing to clean ({} does not exist)", build.display());
        }
        return Ok(());
    }
    std::fs::remove_dir_all(&build).with_context(|| format!("removing {}", build.display()))?;
    if !quiet {
        eprintln!("removed {}", build.display());
    }
    Ok(())
}
