//! `miku clean` — remove the project's build output directory.
//!
//! That is `[paths].build` (default `build/`), the one root every
//! command writes under — backend artifacts, `miku doc` pages and
//! fight reports alike. `--doc` narrows the sweep to `<build>/doc/`.

use std::path::Path;

use anyhow::{Context, Result};

use leek_project::Project;

use crate::cli::Clean;

pub fn run(args: &Clean, manifest_path: Option<&Path>, quiet: bool) -> Result<()> {
    let project = Project::discover(manifest_path)?;
    let dir = if args.doc {
        project.doc_dir()
    } else {
        project.build_dir()
    };
    if !dir.exists() {
        if !quiet {
            eprintln!("nothing to clean ({} does not exist)", dir.display());
        }
        return Ok(());
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    if !quiet {
        eprintln!("removed {}", dir.display());
    }
    Ok(())
}
