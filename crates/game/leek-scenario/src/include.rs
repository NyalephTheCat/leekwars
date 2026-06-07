//! File composition: resolve `extends` inheritance and per-entity `leek`
//! references into one fully inlined [`Scenario`] before anything runs.
//!
//! After [`load`], a scenario has no `extends` and no entity `leek` fields —
//! the rest of the crate (the fight loader, the testing drivers, and later the
//! debugger) never sees a reference.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::merge::{overlay_entity, overlay_scenario};
use crate::schema::{EntitySpec, Scenario};

/// Load and fully resolve a scenario file (`.toml` or `.json`).
///
/// # Errors
/// File-not-found, parse errors, and `extends` cycles surface as errors naming
/// the offending path.
pub fn load(path: &Path) -> Result<Scenario> {
    let mut on_stack = HashSet::new();
    load_inner(path, &mut on_stack)
}

fn load_inner(path: &Path, on_stack: &mut HashSet<PathBuf>) -> Result<Scenario> {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !on_stack.insert(key.clone()) {
        bail!("scenario `extends` cycle through {}", path.display());
    }

    let mut scn = parse_scenario_file(path)?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));

    // Inheritance: load the parent, then overlay this file on top of it.
    if let Some(parent_ref) = scn.extends.take() {
        let parent_path = base_dir.join(&parent_ref);
        let mut parent = load_inner(&parent_path, on_stack)?;
        overlay_scenario(&mut parent, &scn);
        scn = parent;
    }

    // Per-entity reusable leek files: the file supplies defaults, the inline
    // entity overrides them.
    for entity in &mut scn.entities {
        if let Some(leek_ref) = entity.leek.take() {
            let leek_path = base_dir.join(&leek_ref);
            let mut merged = parse_entity_file(&leek_path)?;
            overlay_entity(&mut merged, entity);
            merged.leek = None;
            *entity = merged;
        }
    }

    on_stack.remove(&key);
    Ok(scn)
}

/// Parse a scenario file by extension without resolving references.
fn parse_scenario_file(path: &Path) -> Result<Scenario> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading scenario {}", path.display()))?;
    if is_json(path) {
        serde_json::from_str(&text)
            .with_context(|| format!("parsing scenario JSON {}", path.display()))
    } else {
        toml::from_str(&text).with_context(|| format!("parsing scenario TOML {}", path.display()))
    }
}

/// Parse a reusable leek-settings file (an [`EntitySpec`]) by extension.
fn parse_entity_file(path: &Path) -> Result<EntitySpec> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading leek file {}", path.display()))?;
    if is_json(path) {
        serde_json::from_str(&text).with_context(|| format!("parsing leek JSON {}", path.display()))
    } else {
        toml::from_str(&text).with_context(|| format!("parsing leek TOML {}", path.display()))
    }
    .map_err(|e: anyhow::Error| anyhow!("{e}"))
}

fn is_json(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}
