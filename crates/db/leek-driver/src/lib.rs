//! Run recipe pipelines over project sources with shared diagnostic reporting.

use std::path::Path;

use anyhow::Result;
use leek_diagnostics::LintLevels;
use leek_diagnostics::{ColorWhen, MessageFormat, Reporter};
use leek_pipeline::{Input, Pipeline, Run};
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
pub fn run_with_reporter(
    pipeline: &Pipeline,
    input: Input,
    source_text: &str,
    file_label: &str,
    reporter: &Reporter,
) -> DriverRun<'static> {
    let run = pipeline.run(input);
    let had_error = reporter.emit_run(run.diagnostics(), source_text, file_label);
    DriverRun { run, had_error }
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
    let pipeline = pipeline_for(config)?;
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

/// Build [`Input`] from [`SourceInput`].
pub fn input_from(src: SourceInput) -> Input {
    Input::from(src)
}
