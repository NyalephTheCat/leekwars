//! `miku new <name>` and `miku init` — create / initialize a project
//! skeleton.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli::{Init, New};

pub fn new(args: New, quiet: bool) -> Result<()> {
    let dir = args.name.as_path();
    let project_name = project_name_from(dir)?;
    if dir.exists() {
        bail!("`{}` already exists", dir.display());
    }
    write_skeleton(dir, &project_name, quiet)?;
    if !quiet {
        eprintln!("Created project `{project_name}` at {}", dir.display());
    }
    Ok(())
}

pub fn init(args: Init, quiet: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("determining current directory")?;
    let project_name = match args.name {
        Some(n) if !n.is_empty() => n,
        _ => project_name_from(&cwd)?,
    };
    let manifest = cwd.join("Miku.toml");
    if manifest.exists() {
        bail!("Miku.toml already exists in {}", cwd.display());
    }
    write_skeleton(&cwd, &project_name, quiet)?;
    if !quiet {
        eprintln!("Initialized project `{project_name}` in {}", cwd.display());
    }
    Ok(())
}

/// Write the standard three files (Miku.toml, src/main.leek,
/// .gitignore) into `dir`. Skips files that already exist *except*
/// Miku.toml, which the caller must have validated as absent.
fn write_skeleton(dir: &Path, project_name: &str, _quiet: bool) -> Result<()> {
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir).with_context(|| format!("creating {}", src_dir.display()))?;

    let manifest = format!(
        "[project]\n\
         name    = \"{project_name}\"\n\
         version = \"0.1.0\"\n\
         \n\
         [backend.native]\n\
         enable  = true\n\
         default = true\n",
    );
    std::fs::write(dir.join("Miku.toml"), manifest)
        .with_context(|| format!("writing {}/Miku.toml", dir.display()))?;

    let main_path = src_dir.join("main.leek");
    if !main_path.exists() {
        let main = "// @version:4\n\
                    \n\
                    var greeting = \"hello, leek\";\n\
                    debug(greeting);\n";
        std::fs::write(&main_path, main)
            .with_context(|| format!("writing {}", main_path.display()))?;
    }

    let gitignore_path = dir.join(".gitignore");
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, "/build/\n/target/\n")
            .with_context(|| format!("writing {}", gitignore_path.display()))?;
    }
    Ok(())
}

fn project_name_from(dir: &Path) -> Result<String> {
    let name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("could not derive project name from {}", dir.display()))?;
    if name.is_empty() {
        bail!("project name is empty");
    }
    Ok(name.to_string())
}
