//! Run recipe pipelines over project sources with shared diagnostic reporting.

use std::path::Path;
use std::sync::Arc;

use crate::recipes::{RecipeParams, Target};
use anyhow::Result;
use leek_diagnostics::{ColorWhen, MessageFormat, Reporter, Sources};
use leek_diagnostics::{LintLevelError, LintLevels};
use leek_pipeline::{Input, Pipeline, Run, TimingSink};
use leek_project::Project;

/// The include-id interner, re-exported so front-ends that own one for
/// a whole run (`miku test`) do not need a direct `leek-resolver`
/// dependency just to name its type.
pub use leek_resolver::interner::{PathInterner, SourceInterner};

/// Configuration for one driver invocation.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub target: Target,
    pub params: RecipeParams,
    pub color: ColorWhen,
    pub format: MessageFormat,
    /// When set, every step of the pipelines planned from this config
    /// records its duration into the sink (`miku build --verbose`).
    ///
    /// A field rather than a parallel set of `_timed` entry points: the
    /// timed and untimed paths were two copies of the same planning code,
    /// and only one of them remembered to merge the manifest's lint groups.
    pub timing: Option<TimingSink>,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            target: Target::Linted,
            params: RecipeParams::default(),
            color: ColorWhen::Auto,
            format: MessageFormat::Human,
            timing: None,
        }
    }
}

/// Result of running the pipeline on one source file.
pub struct DriverRun<'a> {
    pub run: Run<'a>,
    pub had_error: bool,
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
    let had_error = report(&run, source_text, file_label, reporter);
    DriverRun { run, had_error }
}

/// Render `run`'s diagnostics through `reporter` (manifest lint levels
/// applied); returns whether any error was emitted.
///
/// When the pipeline resolved includes, diagnostics raised in included
/// files render against those files' own text and path.
pub fn report(run: &Run<'_>, source_text: &str, file_label: &str, reporter: &Reporter) -> bool {
    let sources = run_sources(run, source_text, file_label);
    reporter.emit(run.diagnostics(), &sources)
}

/// Every file `run`'s diagnostics may point into: the entry, under its own
/// `SourceId` from the run's `Input`, plus each resolved include under the id
/// the resolver re-parsed it with.
///
/// One map for the whole run, so a *label* pointing into an include resolves
/// the same way a primary span does. Before this, the reporter picked one
/// file per diagnostic from the primary's span and drew every label against
/// it, which put a "previously declared here" caret on whatever line of the
/// entry file happened to share the include's offset.
pub fn run_sources(run: &Run<'_>, source_text: &str, file_label: &str) -> Sources {
    let mut sources = Sources::single(run.input().source, file_label, source_text);
    if let Some(graph) = run.get::<leek_resolver::pipeline::IncludeGraphArtifact>() {
        for inc in &graph.includes {
            sources.push(inc.source, inc.path.display().to_string(), &*inc.text);
        }
    }
    sources
}

/// The [`Reporter`] for `project`: the manifest's `[lint]` deny/warn/allow
/// levels over the catalog defaults. Every command that renders or acts on
/// diagnostics builds its reporter here, so `check`, `lint` and `fix` agree
/// on which diagnostics exist and at what severity.
pub fn reporter_for(
    project: &Project,
    color: ColorWhen,
    format: MessageFormat,
) -> std::result::Result<Reporter, LintLevelError> {
    let lint = LintLevels {
        deny: &project.manifest.lint.deny,
        warn: &project.manifest.lint.warn,
        allow: &project.manifest.lint.allow,
    };
    Reporter::new(color, format, lint)
}

/// Render the manifest's own diagnostics — the warnings `leek-manifest`
/// raised while parsing `Miku.toml`, and a `[lint]` entry the catalog does not
/// know — and report whether any of them was error-level.
///
/// This is the one place `Miku.toml` problems reach the user, so they get
/// everything a `.leek` diagnostic gets: a caret under the offending key, the
/// manifest's own `[lint]` allow/deny levels (a project may silence `W0400`
/// or promote it to an error), and `--message-format json` for free.
///
/// Infallible by construction: when the `[lint]` table is itself broken there
/// are no levels to apply, so the bad entry renders at catalog defaults.
pub fn report_manifest(project: &Project, color: ColorWhen, format: MessageFormat) -> bool {
    let label = project.manifest_path.display().to_string();
    match reporter_for(project, color, format) {
        Ok(reporter) => {
            let diagnostics: Vec<leek_diagnostics::Diagnostic> = project
                .warnings
                .iter()
                .cloned()
                .map(leek_diagnostics::IntoDiagnostic::into_diagnostic)
                .collect();
            reporter.emit(&diagnostics, &manifest_sources(project, &label))
        }
        Err(err) => {
            let plain = Reporter::new(color, format, NO_LINT_LEVELS)
                .expect("the empty lint table resolves no codes");
            plain.emit(
                &[lint_level_diagnostic(project, &err)],
                &manifest_sources(project, &label),
            )
        }
    }
}

/// The manifest's text under [`Span::MANIFEST_SOURCE`](leek_span::Span::MANIFEST_SOURCE).
///
/// Named explicitly rather than left to the reporter's entry-file fallback:
/// the sentinel is what keeps a manifest diagnostic from drawing its caret in
/// `main.leek`, and a map that resolves it by accident would undo that.
fn manifest_sources(project: &Project, label: &str) -> Sources {
    Sources::single(
        leek_span::Span::MANIFEST_SOURCE,
        label,
        project.manifest_text.clone(),
    )
}

/// Severity overrides for a manifest whose `[lint]` table can't be trusted.
const NO_LINT_LEVELS: LintLevels<'static> = LintLevels {
    deny: &[],
    warn: &[],
    allow: &[],
};

/// The unresolvable `[lint]` entry as a diagnostic, pointing at the array
/// element that named it when the manifest recorded a span for it.
fn lint_level_diagnostic(project: &Project, err: &LintLevelError) -> leek_diagnostics::Diagnostic {
    let span = project
        .manifest
        .lint
        .span_for(err.raw())
        .unwrap_or_else(|| leek_span::Span::new(leek_span::Span::MANIFEST_SOURCE, 0, 0));
    leek_diagnostics::Diagnostic::at(
        leek_diagnostics::codes::MANIFEST_UNKNOWN_LINT_CODE,
        span,
        err.to_string(),
    )
}

/// The pipeline for one project file: `config`'s target and params merged
/// with the manifest's opt-in lint groups, with the file's includes resolved
/// from disk (included files get `SourceId`s following `source_id`).
///
/// Every `miku` subcommand that compiles one file per invocation plans it
/// here, so `check`, `lint`, `run`, `build`, `fix`, `analyze` and `doc` all
/// see the same include closure and the same lint groups. `test` compiles a
/// whole directory in one process and plans through
/// [`file_pipeline_shared`] instead, for one id space across the run.
pub fn file_pipeline(
    project: &Project,
    path: &Path,
    source_id: leek_span::SourceId,
    config: &DriverConfig,
) -> Result<Pipeline> {
    standalone_pipeline(path, source_id, &merge_manifest_lints(project, config))
}

/// [`file_pipeline`] for a command that compiles many files in one run:
/// the entry and its includes are numbered out of the caller's shared
/// `interner` rather than counting up from a per-file id, and the
/// entry's id comes back so the caller builds its `Input` with it.
///
/// `miku test` needs this — numbering each test file `1, 2, 3, …` while
/// its includes take the ids just above the entry's hands file 2 the id
/// file 1's first include already has (#191).
pub fn file_pipeline_shared(
    project: &Project,
    path: &Path,
    config: &DriverConfig,
    interner: &Arc<dyn SourceInterner>,
) -> Result<(Pipeline, leek_span::SourceId)> {
    let merged = merge_manifest_lints(project, config);
    let pipeline = build_with(&merged, includes_step(path, interner))?;
    Ok((pipeline, interner.intern(path)))
}

/// The pipeline for one file compiled outside any project — the
/// manifest-less half of [`file_pipeline`], for front-ends that have a
/// path but no `Miku.toml` (`leekc`).
///
/// There are no manifest lint groups to merge, but the file's includes
/// still resolve from disk and its included files are numbered exactly
/// the way the one-file-per-invocation `miku` commands number theirs:
/// from a fresh interner seeded at the entry's own `source_id`.
pub fn standalone_pipeline(
    path: &Path,
    source_id: leek_span::SourceId,
    config: &DriverConfig,
) -> Result<Pipeline> {
    build_with(config, includes_step_standalone(path, source_id))
}

/// Plan `config`'s target with `includes` sequenced ahead of parsing, then
/// build it — timing every step when `config` carries a [`TimingSink`].
///
/// The single place a driver pipeline is built, so `--verbose` cannot end up
/// measuring a different plan from the one the same command runs without it.
fn build_with(config: &DriverConfig, includes: Box<dyn leek_pipeline::Step>) -> Result<Pipeline> {
    Ok(
        crate::recipes::plan_with_includes(config.target, includes, &config.params)?
            .build_with(config.timing.as_ref()),
    )
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

/// Build the `ResolveIncludes` step for a file: disk-folder I/O, with
/// the entry and every file it includes numbered out of `interner`.
///
/// Front-ends that compile several files in one process pass one
/// interner to every call, so a helper included by two entries keeps a
/// single `SourceId` instead of colliding with the next entry's
/// (#191). The entry is interned first, so `interner.intern(path)` is
/// the id the caller should build its `Input` with.
///
/// Public so other front-ends that drive the pipeline themselves (the debug
/// adapter, which reports diagnostics over DAP rather than rendering them)
/// resolve includes and number sources exactly as the `miku` commands here
/// do.
pub fn includes_step(
    path: &Path,
    interner: &Arc<dyn SourceInterner>,
) -> Box<dyn leek_pipeline::Step> {
    // The same key rule the include graph itself uses, so the entry
    // and its includes cannot land in the graph under two shapes (#181).
    let canonical = leek_span::paths::canonical_or_normalized(path);
    Box::new(leek_resolver::pipeline::ResolveIncludes::new(
        Arc::new(leek_resolver::folder::DiskFolder),
        canonical,
        Arc::clone(interner),
    ))
}

/// [`includes_step`] for a front-end that compiles exactly one entry:
/// a fresh interner whose first path — the entry — gets `source_id`,
/// so its includes follow on from the id the caller already put in its
/// `Input`.
pub fn includes_step_standalone(
    path: &Path,
    source_id: leek_span::SourceId,
) -> Box<dyn leek_pipeline::Step> {
    let interner: Arc<dyn SourceInterner> = Arc::new(
        leek_resolver::interner::PathInterner::starting_at(source_id.get()),
    );
    includes_step(path, &interner)
}

/// Convenience: discover project, build reporter, run one file.
pub fn run_file(
    project: &Project,
    path: &Path,
    source_id: leek_span::SourceId,
    config: &DriverConfig,
) -> Result<DriverRun<'static>> {
    let (src, text) = project.pipeline_input(source_id, path)?;
    let reporter = reporter_for(project, config.color, config.format)?;
    let pipeline = file_pipeline(project, path, source_id, config)?;
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

#[cfg(test)]
mod tests {
    use leek_diagnostics::{Diagnostic, Severity, codes};
    use leek_manifest::ManifestLoad;
    use leek_pipeline::LintGroups;
    use leek_span::{SourceId, Span};

    use super::*;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "leek-session-driver-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// The minimum `Miku.toml` the parser accepts; tests append the table
    /// they actually care about.
    const BASE_MANIFEST: &str = "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n";

    /// A project rooted at `root` whose manifest is `BASE_MANIFEST` + `extra`.
    fn project_at(root: std::path::PathBuf, extra: &str) -> Project {
        let toml = format!("{BASE_MANIFEST}{extra}");
        let (manifest, warnings) = leek_manifest::load_str(&toml).expect("parse manifest");
        let path = root.join("Miku.toml");
        Project::from_load(ManifestLoad {
            manifest,
            root,
            path,
            text: toml,
            warnings,
        })
    }

    fn project(extra: &str) -> Project {
        project_at(std::path::PathBuf::from("."), extra)
    }

    fn config(lints: LintGroups) -> DriverConfig {
        DriverConfig {
            params: RecipeParams::default().with_lints(lints),
            ..DriverConfig::default()
        }
    }

    fn diag(code: leek_diagnostics::Code, severity: Severity) -> Diagnostic {
        Diagnostic::new(
            code,
            severity,
            Span::new(SourceId::new(1).unwrap(), 0, 0),
            "synthetic",
        )
    }

    /// A diagnostic raised inside an included file must render against
    /// *that* file: its path in the `-->` header and its own line of
    /// source under the caret. The reporter used to pick one file per
    /// diagnostic and draw everything — labels included — against it.
    #[test]
    fn a_diagnostic_from_an_included_file_renders_against_that_file() {
        let dir = scratch("include-render");
        let main_path = dir.join("main.leek");
        std::fs::write(&main_path, "include(\"inc\")\nvar kept = 1;\n").unwrap();
        std::fs::write(
            dir.join("inc.leek"),
            "var helper = 1;\nvar broken = notDeclaredAnywhere;\n",
        )
        .unwrap();

        let project = project_at(dir.clone(), "");
        let source_id = SourceId::new(1).unwrap();
        let config = DriverConfig::default();
        let (src, text) = project.pipeline_input(source_id, &main_path).unwrap();
        let pipeline = file_pipeline(&project, &main_path, source_id, &config).unwrap();
        let run = pipeline.run(Input::from(src));
        let label = main_path.display().to_string();

        let sources = run_sources(&run, &text, &label);
        let reporter = reporter_for(&project, ColorWhen::Never, MessageFormat::Human).unwrap();
        let out = reporter.render_all(run.diagnostics(), &sources);

        // Both files are registered, each under its own id, from one map.
        let at = out
            .find("inc.leek:2:")
            .unwrap_or_else(|| panic!("no header pointing into the include:\n{out}"));
        // The include's snippet block runs until the next file header.
        let block = out[at..].split("  --> ").next().unwrap();
        assert!(
            block.contains("var broken = notDeclaredAnywhere;"),
            "expected the include's own source line:\n{block}"
        );
        assert!(
            !block.contains("var kept = 1;"),
            "the entry's line 2 leaked into the include's snippet:\n{block}"
        );
        assert!(
            out.contains("main.leek:2:"),
            "the entry's own diagnostics still render:\n{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The manifest's text is registered under the manifest sentinel, not
    /// under whatever id happens to be first — a `Miku.toml` diagnostic
    /// must never draw its caret in a `.leek` file.
    #[test]
    fn manifest_sources_uses_the_manifest_sentinel() {
        let project = project("");
        let sources = manifest_sources(&project, "Miku.toml");
        assert!(
            leek_diagnostics::SourceMap::get(&sources, leek_span::Span::MANIFEST_SOURCE).is_some()
        );
        assert!(leek_diagnostics::SourceMap::get(&sources, SourceId::new(1).unwrap()).is_none());
    }

    #[test]
    fn manifest_lint_groups_or_into_the_cli_flags() {
        // Either source can switch a group on and neither can switch one off:
        // `miku check --pedantic` must not be undone by an absent manifest
        // key, and `lint.pedantic = true` must not need the flag.
        for manifest_pedantic in [false, true] {
            for manifest_nursery in [false, true] {
                for cli_pedantic in [false, true] {
                    for cli_nursery in [false, true] {
                        let toml = format!(
                            "[lint]\npedantic = {manifest_pedantic}\nnursery = {manifest_nursery}\n"
                        );
                        let project = project(&toml);
                        let cli = config(LintGroups {
                            pedantic: cli_pedantic,
                            nursery: cli_nursery,
                        });
                        let merged = merge_manifest_lints(&project, &cli);
                        assert_eq!(
                            merged.params.lints,
                            LintGroups {
                                pedantic: manifest_pedantic || cli_pedantic,
                                nursery: manifest_nursery || cli_nursery,
                            },
                            "manifest ({manifest_pedantic}, {manifest_nursery}) \
                             + cli ({cli_pedantic}, {cli_nursery})"
                        );
                        // The caller's config is left untouched.
                        assert_eq!(cli.params.lints.pedantic, cli_pedantic);
                        assert_eq!(cli.params.lints.nursery, cli_nursery);
                    }
                }
            }
        }
    }

    #[test]
    fn merging_preserves_the_rest_of_the_config() {
        let project = project("[lint]\npedantic = true\n");
        let cli = DriverConfig {
            target: Target::Mir,
            params: RecipeParams::default().with_opt(crate::recipes::OptLevel::O1),
            color: ColorWhen::Never,
            format: MessageFormat::Json,
            timing: None,
        };
        let merged = merge_manifest_lints(&project, &cli);
        assert_eq!(merged.target, Target::Mir);
        assert_eq!(merged.params.opt, crate::recipes::OptLevel::O1);
        assert!(matches!(merged.format, MessageFormat::Json));
        assert!(merged.params.lints.pedantic);
    }

    #[test]
    fn reporter_applies_the_manifest_allow_list() {
        let project = project("[lint]\nallow = [\"E0240\"]\n");
        let reporter =
            reporter_for(&project, ColorWhen::Never, MessageFormat::Human).expect("reporter");
        let kept = reporter.apply_levels(&[
            diag(codes::PRIVATE_FIELD, Severity::Error),
            diag(codes::UNEXPECTED_TOKEN, Severity::Error),
        ]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].code, codes::UNEXPECTED_TOKEN);
    }

    #[test]
    fn reporter_applies_the_manifest_deny_and_warn_lists() {
        let project = project("[lint]\ndeny = [\"W0010\"]\nwarn = [\"E0240\"]\n");
        let reporter =
            reporter_for(&project, ColorWhen::Never, MessageFormat::Human).expect("reporter");
        let out = reporter.apply_levels(&[
            diag(codes::PRAGMA_UNKNOWN, Severity::Warning),
            diag(codes::PRIVATE_FIELD, Severity::Error),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].severity, Severity::Error, "deny promotes");
        assert_eq!(out[1].severity, Severity::Warning, "warn demotes");
    }

    #[test]
    fn reporter_accepts_canonical_names_as_well_as_ids() {
        let project = project("[lint]\nallow = [\"PrivateField\"]\n");
        let reporter =
            reporter_for(&project, ColorWhen::Never, MessageFormat::Human).expect("reporter");
        assert!(
            reporter
                .apply_levels(&[diag(codes::PRIVATE_FIELD, Severity::Error)])
                .is_empty()
        );
    }

    #[test]
    fn an_unknown_lint_code_in_the_manifest_is_an_error_not_a_silent_no_op() {
        let project = project("[lint]\ndeny = [\"NOPE9999\"]\n");
        let Err(err) = reporter_for(&project, ColorWhen::Never, MessageFormat::Human) else {
            panic!("an unknown lint code must fail the reporter build");
        };
        assert!(err.to_string().contains("NOPE9999"), "{err}");
        assert!(matches!(
            err,
            leek_diagnostics::LintLevelError::UnknownCode { .. }
        ));

        // …and it renders as a diagnostic pointing at the array element that
        // named it, rather than a bare line of prose.
        let diag = lint_level_diagnostic(&project, &err);
        assert_eq!(diag.code, codes::MANIFEST_UNKNOWN_LINT_CODE);
        assert_eq!(diag.severity, Severity::Error);
        assert_eq!(diag.span.source, Span::MANIFEST_SOURCE);
        let text = &project.manifest_text;
        assert_eq!(
            &text[diag.span.start as usize..diag.span.end as usize],
            "\"NOPE9999\""
        );
        // `report_manifest` reports it as an error even though no reporter
        // could be built from the broken `[lint]` table.
        assert!(report_manifest(
            &project,
            ColorWhen::Never,
            MessageFormat::Human
        ));
    }

    #[test]
    fn report_manifest_honours_the_manifest_lint_levels() {
        // `[fight] worker_count` is an unknown field: a W0400 warning.
        let unknown_field = "[fight]\nworker_count = 4\n";

        let plain = project(unknown_field);
        assert_eq!(plain.warnings.len(), 1, "{:?}", plain.warnings);
        assert!(
            !report_manifest(&plain, ColorWhen::Never, MessageFormat::Human),
            "a warning is not an error"
        );

        let allowed = project(&format!("{unknown_field}[lint]\nallow = [\"W0400\"]\n"));
        assert!(!report_manifest(
            &allowed,
            ColorWhen::Never,
            MessageFormat::Human
        ));
        let reporter =
            reporter_for(&allowed, ColorWhen::Never, MessageFormat::Human).expect("reporter");
        let diagnostics: Vec<Diagnostic> = allowed
            .warnings
            .iter()
            .cloned()
            .map(leek_diagnostics::IntoDiagnostic::into_diagnostic)
            .collect();
        assert!(
            reporter.apply_levels(&diagnostics).is_empty(),
            "`allow` must silence the manifest warning"
        );

        let denied = project(&format!("{unknown_field}[lint]\ndeny = [\"W0400\"]\n"));
        assert!(
            report_manifest(&denied, ColorWhen::Never, MessageFormat::Human),
            "`deny` must promote the manifest warning to an error"
        );
    }

    #[test]
    fn includes_step_survives_a_path_that_cannot_be_canonicalized() {
        // `canonicalize` fails for a file that does not exist; the step must
        // fall back to the path as given rather than panic.
        let step = includes_step_standalone(
            std::path::Path::new("/no/such/entry.leek"),
            SourceId::new(7).unwrap(),
        );
        assert_eq!(step.name(), "resolve_includes");
    }

    /// The reason `file_pipeline_shared` exists: two entry files planned
    /// from one interner get two ids, and a helper both of them include
    /// keeps a third — where the per-file numbering handed entry 2 the id
    /// entry 1's include already owned (#191).
    #[test]
    fn one_interner_numbers_several_entries_without_collisions() {
        let interner: Arc<dyn SourceInterner> = Arc::new(PathInterner::new());
        let first = interner.intern(std::path::Path::new("/tests/first.leek"));
        let helper = interner.intern(std::path::Path::new("/tests/helper.leek"));
        let second = interner.intern(std::path::Path::new("/tests/second.leek"));

        assert_eq!([first.get(), helper.get(), second.get()], [1, 2, 3]);
        assert_eq!(
            interner.intern(std::path::Path::new("/tests/helper.leek")),
            helper,
            "the second entry's include is the same file, so the same id"
        );
    }

    #[test]
    fn file_pipeline_resolves_includes_before_parsing() {
        let dir = scratch("file-pipeline");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        let project = project_at(dir.clone(), "");

        let pipeline = file_pipeline(
            &project,
            &dir.join("src/main.leek"),
            SourceId::new(1).unwrap(),
            &DriverConfig::default(),
        )
        .expect("pipeline");
        let names = pipeline.step_names();
        let at = |n: &str| {
            names
                .iter()
                .position(|s| *s == n)
                .unwrap_or_else(|| panic!("no `{n}` step in {names:?}"))
        };
        assert!(at("lex") < at("resolve_includes"), "{names:?}");
        assert!(at("resolve_includes") < at("parse"), "{names:?}");
        assert!(names.contains(&"lint"), "the default target is Linted");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_pipeline_resolves_includes_for_every_target_a_command_asks_for() {
        // DRIVER-02: `analyze` and `doc` ask for `Complexity`, `test` and
        // `fix` for `Linted`, `build` for `Mir`. Whichever a command wants,
        // it gets the same include-resolving front end — otherwise one
        // subcommand sees a symbol another one does not.
        let dir = scratch("every-target");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        let project = project_at(dir.clone(), "");

        for target in [
            Target::Resolved,
            Target::TypeChecked,
            Target::Hir,
            Target::Linted,
            Target::Mir,
            Target::Complexity,
        ] {
            let config = DriverConfig {
                target,
                ..DriverConfig::default()
            };
            let names = file_pipeline(
                &project,
                &dir.join("src/main.leek"),
                SourceId::new(1).unwrap(),
                &config,
            )
            .expect("pipeline")
            .step_names();
            let at = |n: &str| {
                names
                    .iter()
                    .position(|s| *s == n)
                    .unwrap_or_else(|| panic!("no `{n}` step for {target:?} in {names:?}"))
            };
            assert!(at("lex") < at("resolve_includes"), "{target:?}: {names:?}");
            assert!(
                at("resolve_includes") < at("parse"),
                "{target:?}: {names:?}"
            );
        }
        assert!(
            file_pipeline(
                &project,
                &dir.join("src/main.leek"),
                SourceId::new(1).unwrap(),
                &DriverConfig {
                    target: Target::Complexity,
                    ..DriverConfig::default()
                },
            )
            .expect("pipeline")
            .step_names()
            .contains(&"complexity"),
            "the Complexity target must still end at the complexity step"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_standalone_pipeline_differs_from_the_project_one_only_in_lint_groups() {
        // `leekc` has no manifest, so it plans through `standalone_pipeline`.
        // The pass sequence must be the one the `miku` commands get, or the
        // two front ends disagree about what `include(...)` means.
        let dir = scratch("standalone");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        let entry = dir.join("src/main.leek");
        let id = SourceId::new(1).unwrap();

        let project = project_at(dir.clone(), "");
        let from_project = file_pipeline(&project, &entry, id, &DriverConfig::default())
            .expect("project pipeline")
            .step_names();
        let standalone = standalone_pipeline(&entry, id, &DriverConfig::default())
            .expect("standalone pipeline")
            .step_names();
        assert_eq!(standalone, from_project);

        // The manifest's lint groups are the project-only half: they change
        // what the lint step reports, never the pass sequence.
        let loud = project_at(dir.clone(), "[lint]\npedantic = true\n");
        assert_eq!(
            file_pipeline(&loud, &entry, id, &DriverConfig::default())
                .expect("pipeline")
                .step_names(),
            standalone
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `miku build --verbose` must time the pipeline the plain build runs —
    /// same steps, one entry each — and must still get the manifest's lint
    /// groups. The old `run_file_timed` re-inlined the planning code, so
    /// either half could drift from `file_pipeline` unnoticed.
    #[test]
    fn a_timing_sink_records_the_same_plan_the_untimed_config_builds() {
        let dir = scratch("timed");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        let project = project_at(dir.clone(), "[lint]\npedantic = true\n");

        let untimed = DriverConfig {
            color: ColorWhen::Never,
            ..DriverConfig::default()
        };
        let plain = run_entry(&project, &untimed).expect("untimed run");
        let expected = file_pipeline(
            &project,
            &project.entry_path(),
            SourceId::new(1).unwrap(),
            &untimed,
        )
        .expect("pipeline")
        .step_names();

        let sink = TimingSink::new();
        let timed = run_entry(
            &project,
            &DriverConfig {
                timing: Some(sink.clone()),
                ..untimed
            },
        )
        .expect("timed run");

        let names: Vec<&str> = sink.entries().iter().map(|e| e.step).collect();
        assert_eq!(
            names, expected,
            "the sink must see every step of the untimed plan, once each"
        );
        assert_eq!(timed.had_error, plain.had_error);
        // The manifest's `pedantic = true` reached the timed plan too: the
        // lint step is planned, so the merge happened on this path as well.
        assert!(names.contains(&"lint"), "{names:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_pipeline_follows_the_configured_target() {
        let dir = scratch("target");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        let project = project_at(dir.clone(), "");

        let config = DriverConfig {
            target: Target::Mir,
            ..DriverConfig::default()
        };
        let names = file_pipeline(
            &project,
            &dir.join("src/main.leek"),
            SourceId::new(1).unwrap(),
            &config,
        )
        .expect("pipeline")
        .step_names();
        assert!(names.contains(&"lower-mir"), "{names:?}");
        assert!(!names.contains(&"lint"), "{names:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_entry_reports_a_resolver_error_from_the_project_entry() {
        let dir = scratch("run-entry");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        // A redeclared symbol (E0202) — a resolver error, so it also proves
        // the pipeline got past lexing and parsing.
        std::fs::write(
            dir.join("src/main.leek"),
            "var a = 1;\nvar a = 2;\nreturn a;\n",
        )
        .expect("entry");
        let project = project_at(dir.clone(), "");

        let config = DriverConfig {
            color: ColorWhen::Never,
            ..DriverConfig::default()
        };
        let run = run_entry(&project, &config).expect("run");
        assert!(run.had_error, "diagnostics: {:?}", run.run.diagnostics());
        assert!(
            run.run
                .diagnostics()
                .iter()
                .any(|d| d.code == codes::REDECLARED_SYMBOL),
            "diagnostics: {:?}",
            run.run.diagnostics()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_entry_is_clean_for_a_well_formed_entry() {
        let dir = scratch("run-entry-ok");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(dir.join("src/main.leek"), "return 1 + 1;\n").expect("entry");
        let project = project_at(dir.clone(), "");

        let config = DriverConfig {
            color: ColorWhen::Never,
            ..DriverConfig::default()
        };
        let run = run_entry(&project, &config).expect("run");
        assert!(!run.had_error, "diagnostics: {:?}", run.run.diagnostics());
        assert!(run.run.errors().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
