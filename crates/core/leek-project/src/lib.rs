//! Project discovery and helpers shared by `miku`, the LSP, and drivers.

mod index;

pub use index::{LoadedProjectFile, ProjectError, ProjectIndex, walk_leek_files};

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use leek_manifest::{Manifest, ManifestLoad, ManifestWarning};
use leek_span::SourceId;

/// Pipeline-ready source metadata (convert to [`leek_pipeline::Input`] at the driver).
#[derive(Debug, Clone)]
pub struct SourceInput {
    pub source: SourceId,
    pub text: String,
    pub version_byte: u8,
    pub strict: bool,
}

/// Loaded `Miku.toml` plus filesystem layout and a source-file index.
pub struct Project {
    pub manifest: Manifest,
    pub root: PathBuf,
    pub warnings: Vec<ManifestWarning>,
    index: ProjectIndex,
}

impl Project {
    pub fn discover(manifest_path: Option<&Path>) -> Result<Self> {
        let load = if let Some(p) = manifest_path {
            leek_manifest::load_from(p)
        } else {
            let cwd = std::env::current_dir().context("determining current directory")?;
            leek_manifest::discover(&cwd)
        }
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Self::from_load(load))
    }

    pub fn from_load(load: ManifestLoad) -> Self {
        let index = ProjectIndex::from_manifest(load.root.clone(), &load.manifest);
        Self {
            manifest: load.manifest,
            root: load.root,
            warnings: load.warnings,
            index,
        }
    }

    pub fn index(&self) -> &ProjectIndex {
        &self.index
    }

    pub fn index_mut(&mut self) -> &mut ProjectIndex {
        &mut self.index
    }

    pub fn entry_path(&self) -> PathBuf {
        self.root.join(&self.manifest.project.entry)
    }

    pub fn src_dir(&self) -> PathBuf {
        self.root.join(&self.manifest.paths.src)
    }

    pub fn tests_dir(&self) -> PathBuf {
        self.root.join(&self.manifest.paths.tests)
    }

    pub fn build_dir(&self) -> PathBuf {
        self.root.join("build")
    }

    pub fn walk_sources(&self) -> Vec<PathBuf> {
        walk_leek_files(&self.src_dir())
    }

    pub fn walk_tests(&self) -> Vec<PathBuf> {
        walk_leek_files(&self.tests_dir())
    }

    pub fn pipeline_input(
        &self,
        source_id: SourceId,
        path: &Path,
    ) -> Result<(SourceInput, String)> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let version_byte = index::peek_version_byte(&text, self.index.default_version_byte);
        let strict = index::peek_strict_flag(&text) || self.index.default_strict;
        Ok((
            SourceInput {
                source: source_id,
                text: text.clone(),
                version_byte,
                strict,
            },
            text,
        ))
    }

    pub fn load_file(&mut self, path: &Path) -> Result<LoadedProjectFile, ProjectError> {
        self.index.load_file(path)
    }
}
