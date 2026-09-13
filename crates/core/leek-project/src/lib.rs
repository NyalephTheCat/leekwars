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

    /// The single output root — `[paths].build` (default `build`).
    /// Every command that writes artifacts (backends, `miku doc`,
    /// fight reports) stays under it, and `miku clean` removes it.
    pub fn build_dir(&self) -> PathBuf {
        self.root.join(&self.manifest.paths.build)
    }

    /// Where `miku doc` writes when given no `--out-dir` —
    /// `<build>/doc` (default `build/doc`).
    pub fn doc_dir(&self) -> PathBuf {
        self.build_dir().join("doc")
    }

    /// Where `miku fight --report` writes when given no path —
    /// `[fight].reports_dir` (default `build/fight-reports`).
    pub fn fight_reports_dir(&self) -> PathBuf {
        self.root.join(&self.manifest.fight.reports_dir)
    }

    /// Resolve a scenario argument against the project. Absolute paths and
    /// paths that already exist as given are kept as-is; anything else is
    /// looked up under `[fight].scenarios_dir` first, then under the
    /// project root.
    pub fn scenario_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() || path.exists() {
            return path.to_path_buf();
        }
        if let Some(dir) = &self.manifest.fight.scenarios_dir {
            let candidate = self.root.join(dir).join(path);
            if candidate.exists() {
                return candidate;
            }
        }
        self.root.join(path)
    }

    /// The scenario `miku fight` plays when given no path —
    /// `[fight].default_scenario`, resolved like any scenario argument.
    pub fn default_scenario(&self) -> Option<PathBuf> {
        self.manifest
            .fight
            .default_scenario
            .clone()
            .map(|p| self.scenario_path(&p))
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
        let lang = self.index.language_settings(&text);
        Ok((
            SourceInput {
                source: source_id,
                text: text.clone(),
                version_byte: lang.version,
                strict: lang.strict,
            },
            text,
        ))
    }

    pub fn load_file(&mut self, path: &Path) -> Result<LoadedProjectFile, ProjectError> {
        self.index.load_file(path)
    }
}
