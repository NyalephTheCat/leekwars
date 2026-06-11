//! Run recipe pipelines over project sources with shared diagnostic reporting.

use std::path::Path;

use anyhow::Result;
use leek_diagnostics::LintLevels;
use leek_diagnostics::{ColorWhen, MessageFormat, Reporter};
use leek_pipeline::{Input, Pipeline, Run, TimingSink};
use leek_project::{Project, SourceInput};
use leek_recipes::{RecipeParams, Target};

/// Configuration for one driver invocation.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub target: Target,
    pub params: RecipeParams,
    pub color: ColorWhen,
    pub format: MessageFormat,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            target: Target::Linted,
            params: RecipeParams::default(),
            color: ColorWhen::Auto,
            format: MessageFormat::Human,
        }
    }
}

/// Result of running the pipeline on one source file.
pub struct DriverRun<'a> {
    pub run: Run<'a>,
    pub had_error: bool,
}

/// Build a [`Pipeline`] for `config`.
pub fn pipeline_for(config: &DriverConfig) -> Result<Pipeline, leek_recipes::RecipeError> {
    leek_recipes::pipeline(config.target, &config.params)
}

/// Run the pipeline on `input`, render diagnostics, return the [`Run`].
///
/// When the pipeline resolved includes, diagnostics raised in included
/// files render against those files' own text and path.
pub fn run_with_reporter(
    pipeline: &Pipeline,
    input: Input,
    source_text: &str,
    file_label: &str,
    reporter: &Reporter,
) -> DriverRun<'static> {
    let run = pipeline.run(input);
    let had_error = match run.get::<leek_resolver::pipeline::IncludeGraphArtifact>() {
        Some(graph) if !graph.includes.is_empty() => {
            let labels: Vec<String> = graph
                .includes
                .iter()
                .map(|inc| inc.path.display().to_string())
                .collect();
            let sources: Vec<leek_diagnostics::RunSource<'_>> = graph
                .includes
                .iter()
                .zip(&labels)
                .map(|(inc, label)| leek_diagnostics::RunSource {
                    source: inc.source,
                    text: &inc.text,
                    label,
                })
                .collect();
            reporter.emit_run_sources(run.diagnostics(), source_text, file_label, &sources)
        }
        _ => reporter.emit_run(run.diagnostics(), source_text, file_label),
    };
    DriverRun { run, had_error }
}

/// Merge the manifest's opt-in lint groups into `config`'s params.
/// OR semantics: a group runs if either the CLI flags or `Miku.toml`'s
/// `[lint]` table asks for it.
fn merge_manifest_lints(project: &Project, config: &DriverConfig) -> DriverConfig {
    let mut config = config.clone();
    config.params.lints.pedantic |= project.manifest.lint.pedantic;
    config.params.lints.nursery |= project.manifest.lint.nursery;
    config
}

/// Build the `ResolveIncludes` step for a file: disk-folder I/O,
/// `SourceId`s allocated sequentially from the entry's own id (the
/// graph walker seeds the entry first, so it keeps `source_id` and
/// each included file gets the next id).
fn includes_step(path: &Path, source_id: leek_span::SourceId) -> Box<dyn leek_pipeline::Step> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Box::new(leek_resolver::pipeline::ResolveIncludes::with_counter(
        std::sync::Arc::new(leek_resolver::folder::DiskFolder),
        canonical,
        source_id.get(),
    ))
}

/// Convenience: discover project, build reporter, run one file.
pub fn run_file(
    project: &Project,
    path: &Path,
    source_id: leek_span::SourceId,
    config: &DriverConfig,
) -> Result<DriverRun<'static>> {
    let (src, text) = project.pipeline_input(source_id, path)?;
    let lint = LintLevels {
        deny: &project.manifest.lint.deny,
        warn: &project.manifest.lint.warn,
        allow: &project.manifest.lint.allow,
    };
    let reporter =
        Reporter::new(config.color, config.format, lint).map_err(|e| anyhow::anyhow!("{e}"))?;
    let merged = merge_manifest_lints(project, config);
    let pipeline = leek_recipes::pipeline_with_includes(
        merged.target,
        includes_step(path, source_id),
        &merged.params,
    )?;
    let label = path.display().to_string();
    Ok(run_with_reporter(
        &pipeline,
        Input::from(src),
        &text,
        &label,
        &reporter,
    ))
}

/// Run the project entry file.
pub fn run_entry(project: &Project, config: &DriverConfig) -> Result<DriverRun<'static>> {
    let entry = project.entry_path();
    run_file(
        project,
        &entry,
        leek_span::SourceId::new(1).unwrap(),
        config,
    )
}

/// Like [`run_file`], but records per-step durations into `sink`.
pub fn run_file_timed(
    project: &Project,
    path: &Path,
    source_id: leek_span::SourceId,
    config: &DriverConfig,
    sink: &TimingSink,
) -> Result<DriverRun<'static>> {
    let (src, text) = project.pipeline_input(source_id, path)?;
    let lint = LintLevels {
        deny: &project.manifest.lint.deny,
        warn: &project.manifest.lint.warn,
        allow: &project.manifest.lint.allow,
    };
    let reporter =
        Reporter::new(config.color, config.format, lint).map_err(|e| anyhow::anyhow!("{e}"))?;
    let merged = merge_manifest_lints(project, config);
    let pipeline = leek_recipes::pipeline_with_includes_timed(
        merged.target,
        includes_step(path, source_id),
        &merged.params,
        sink,
    )?;
    let label = path.display().to_string();
    Ok(run_with_reporter(
        &pipeline,
        Input::from(src),
        &text,
        &label,
        &reporter,
    ))
}

/// Like [`run_entry`], but records per-step durations into `sink`.
pub fn run_entry_timed(
    project: &Project,
    config: &DriverConfig,
    sink: &TimingSink,
) -> Result<DriverRun<'static>> {
    let entry = project.entry_path();
    run_file_timed(
        project,
        &entry,
        leek_span::SourceId::new(1).unwrap(),
        config,
        sink,
    )
}

/// Build [`Input`] from [`SourceInput`].
pub fn input_from(src: SourceInput) -> Input {
    Input::from(src)
}
