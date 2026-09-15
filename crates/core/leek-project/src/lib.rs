//! Project discovery and helpers shared by `miku`, the LSP, and drivers.

mod index;

pub use index::{LoadedProjectFile, ProjectError, ProjectIndex, walk_leek_files};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_manifest::{Manifest, ManifestLoad, ManifestWarning};
use leek_span::{FeatureFlags, SourceId};

/// Pipeline-ready source metadata (convert to [`Input`] at the driver).
#[derive(Debug, Clone)]
pub struct SourceInput {
    pub source: SourceId,
    pub text: String,
    pub version_byte: u8,
    pub strict: bool,
}

/// Configuration for a single pipeline run.
///
/// One source file, one version, one strict-mode setting. Multiple
/// files = multiple pipeline runs (the include graph layer composes
/// those itself).
///
/// The language version is a byte (1..=4) rather than `leek-syntax`'s
/// `Version` enum: `leek-syntax` sits well above this crate, and the passes
/// that want the enum convert on the way in.
#[derive(Debug, Clone)]
pub struct Input {
    pub source: SourceId,
    pub text: Arc<str>,
    pub version_byte: u8,
    pub strict: bool,
    /// Opt-in experimental language features for this run. Threaded through the
    /// pipeline (and the salsa input) instead of read from process-global env
    /// vars deep inside the passes. Construct with [`FeatureFlags::from_env`] at
    /// the entry boundary (or `FeatureFlags::none()` / explicit flags in tests).
    pub flags: FeatureFlags,
}

impl Input {
    /// Build an input from a loaded [`SourceInput`] with the flags the caller
    /// already settled.
    ///
    /// The `From` impls below read the environment once *per conversion*; a
    /// caller converting a whole project in a loop reads
    /// [`FeatureFlags::from_env`] once at its entry boundary and calls this
    /// for each file instead.
    #[must_use]
    pub fn from_source_with_flags(source: SourceInput, flags: FeatureFlags) -> Self {
        Self {
            source: source.source,
            text: source.text.into(),
            version_byte: source.version_byte,
            strict: source.strict,
            flags,
        }
    }
}

// Project-load → pipeline `Input`. The project model carries no experimental
// flags of its own, so this boundary reads them from the environment once (via
// `FeatureFlags::from_env`) and threads the result — rather than each pass
// re-reading env. A caller that wants explicit flags builds `Input` directly or
// goes through [`Input::from_source_with_flags`].

impl From<SourceInput> for Input {
    fn from(s: SourceInput) -> Self {
        Self::from_source_with_flags(s, FeatureFlags::from_env())
    }
}

impl From<&SourceInput> for Input {
    fn from(s: &SourceInput) -> Self {
        Self::from_source_with_flags(s.clone(), FeatureFlags::from_env())
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

/// Loaded `Miku.toml` plus filesystem layout and a source-file index.
pub struct Project {
    pub manifest: Manifest,
    pub root: PathBuf,
    /// The `Miku.toml` this project was loaded from — the label above a
    /// manifest diagnostic's snippet.
    pub manifest_path: PathBuf,
    /// The manifest's text, so manifest diagnostics can render against it.
    pub manifest_text: String,
    pub warnings: Vec<ManifestWarning>,
    index: ProjectIndex,
}

impl Project {
    /// Load the project's manifest, by path or by walking up from the cwd.
    ///
    /// The manifest failure comes back whole — spans included — so a caller
    /// with a reporter can render it against `Miku.toml`; `bins/` front-ends
    /// that only want a message `?` it into `anyhow` and get the `Display`.
    pub fn discover(manifest_path: Option<&Path>) -> Result<Self, ProjectError> {
        let load = if let Some(p) = manifest_path {
            leek_manifest::load_from(p)
        } else {
            let cwd = std::env::current_dir().map_err(|e| ProjectError::Cwd {
                message: e.to_string(),
            })?;
            leek_manifest::discover(&cwd)
        }?;
        Ok(Self::from_load(load))
    }

    pub fn from_load(load: ManifestLoad) -> Self {
        let index = ProjectIndex::from_manifest(load.root.clone(), &load.manifest);
        Self {
            manifest: load.manifest,
            root: load.root,
            manifest_path: load.path,
            manifest_text: load.text,
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

    /// The experimental language features this project compiles with: its
    /// `[experimental]` table, plus whatever the `LEEK_EXPERIMENTAL_*`
    /// variables switch on (leekwars#206).
    ///
    /// Every driver that compiles *this project's* files settles the flags
    /// here, instead of each `Input` conversion reading the environment on
    /// its own — which is what made `[experimental]` inert and left an
    /// env-driven build with nothing in the repository to point at.
    ///
    /// The two sources compose by union, not by replacement: see
    /// [`FeatureFlags::union`].
    #[must_use]
    pub fn feature_flags(&self) -> FeatureFlags {
        self.manifest.experimental.union(FeatureFlags::from_env())
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

    /// `path` written relative to the project root, or unchanged when it
    /// lies outside it.
    ///
    /// Every command that names a file on a progress or summary line prints
    /// it through here, so `miku fix`, `miku test`, `miku analyze` and
    /// `miku migrate` cannot disagree about whether a path is shown whole
    /// or relative (DRIVER-13). Display only: the result is not a path to
    /// open, which is why it is never joined back onto the root.
    #[must_use]
    pub fn relative(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.root)
            .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
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
    ) -> Result<(SourceInput, String), ProjectError> {
        let text = std::fs::read_to_string(path).map_err(|e| ProjectError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
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

#[cfg(test)]
mod tests {
    use super::{Input, SourceInput};
    use leek_span::{FeatureFlags, SourceId};

    #[test]
    fn from_source_with_flags_keeps_the_callers_flags_and_settings() {
        // The `From` impls read `LEEK_EXPERIMENTAL_*` once per conversion; this
        // entry point is what a driver uses to settle the flags once and thread
        // the same value through every file of a project.
        let flags = FeatureFlags {
            generic_syntax: true,
            ..FeatureFlags::none()
        };
        let input = Input::from_source_with_flags(
            SourceInput {
                source: SourceId::new(1).unwrap(),
                text: "var x = 1;".to_string(),
                version_byte: 4,
                strict: true,
            },
            flags,
        );

        assert_eq!(input.flags, flags);
        assert_eq!(input.source, SourceId::new(1).unwrap());
        assert_eq!(&*input.text, "var x = 1;");
        assert_eq!(input.version_byte, 4);
        assert!(input.strict);
    }
}
