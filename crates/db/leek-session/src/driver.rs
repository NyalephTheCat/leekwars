//! What a driver is configured with, and how it reports.

use crate::params::{CompileParams, Target};
use leek_diagnostics::{ColorWhen, MessageFormat, Reporter, Sources};
use leek_diagnostics::{LintLevelError, LintLevels};
use leek_project::Project;
use leek_query::TimingSink;

/// The include-id interner, re-exported so front-ends that own one for
/// a whole run (`miku test`) do not need a direct `leek-resolver`
/// dependency just to name its type.
pub use leek_resolver::interner::{PathInterner, SourceInterner};

/// How much of the program a compilation answers about.
///
/// Every `miku` subcommand is [`Program`](Scope::Program): it compiles a
/// file *and* what that file includes. `leekc` is too, except for the four
/// emits that are textual views of one file — `tokens`, `flat-cst`, `cst`
/// and `fmt`. Those describe the bytes in front of them, so they leave
/// `include(...)` unresolved, and the formatter has to: it must stay
/// byte-faithful to the file it was handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// The entry and its whole include closure. Parses under the
    /// program's class set, and reports what every file in the closure
    /// found.
    #[default]
    Program,
    /// The entry's own bytes. Parses under the empty class set, and
    /// reports only what the entry itself found.
    ///
    /// Valid up to [`Target::Parsed`]: the passes past it are
    /// whole-program by construction, so a file-scoped compilation has no
    /// HIR, MIR or complexity to hand out.
    File,
}

/// Configuration for one driver invocation.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    pub target: Target,
    pub params: CompileParams,
    pub color: ColorWhen,
    pub format: MessageFormat,
    /// Whether the entry's includes are part of the compilation.
    /// [`Scope::Program`] for everything but `leekc`'s textual emits.
    pub scope: Scope,
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
            params: CompileParams::default(),
            color: ColorWhen::Auto,
            format: MessageFormat::Human,
            scope: Scope::Program,
            timing: None,
        }
    }
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

/// One line naming the experimental features a run compiles with, or `None`
/// when every feature is off.
///
/// A feature switched on only by a `LEEK_EXPERIMENTAL_*` variable is marked
/// `(env)`: the environment is the half of the pair that leaves no trace in
/// the repository, and leekwars#206 is exactly that the same project can
/// compile differently in CI and locally with nothing to point at. Naming the
/// features is the fix; marking which of them the manifest does *not* account
/// for is what makes the line actionable.
#[must_use]
pub fn feature_flags_summary(
    manifest: leek_span::FeatureFlags,
    env: leek_span::FeatureFlags,
) -> Option<String> {
    let active = manifest.union(env);
    let listed: Vec<String> = leek_span::FeatureFlags::FIELDS
        .iter()
        .filter(|field| active.get(field))
        .map(|field| {
            if manifest.get(field) {
                field.name.to_string()
            } else {
                format!("{} (env)", field.name)
            }
        })
        .collect();
    if listed.is_empty() {
        return None;
    }
    Some(format!("experimental features: {}", listed.join(", ")))
}

/// Print [`feature_flags_summary`] to stderr under `--verbose`.
///
/// Called once per invocation, beside the manifest warnings, rather than once
/// per compiled file: the flags are settled for the whole run.
pub fn report_feature_flags(
    manifest: leek_span::FeatureFlags,
    env: leek_span::FeatureFlags,
    verbose: bool,
) {
    if !verbose {
        return;
    }
    if let Some(line) = feature_flags_summary(manifest, env) {
        eprintln!("{line}");
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

/// Merge the manifest's opt-in lint groups into `config`'s params.
/// OR semantics: a group runs if either the CLI flags or `Miku.toml`'s
/// `[lint]` table asks for it.
pub(crate) fn merge_manifest_lints(project: &Project, config: &DriverConfig) -> DriverConfig {
    let mut config = config.clone();
    config.params.lints.pedantic |= project.manifest.lint.pedantic;
    config.params.lints.nursery |= project.manifest.lint.nursery;
    config
}

#[cfg(test)]
mod tests {
    use leek_diagnostics::{Diagnostic, Severity, codes};
    use leek_manifest::ManifestLoad;
    use leek_query::LintGroups;
    use leek_span::{SourceId, Span};

    use super::*;
    use crate::session::Session;

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
            params: CompileParams::default().with_lints(lints),
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
        // Under `src/`, where a project's sources live: the session's file
        // set is the project's, so a fixture that scatters `.leek` files
        // at the root is not a project the include graph can see.
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let main_path = src.join("main.leek");
        std::fs::write(&main_path, "include(\"inc\")\nvar kept = 1;\n").unwrap();
        std::fs::write(
            src.join("inc.leek"),
            "var helper = 1;\nvar broken = notDeclaredAnywhere;\n",
        )
        .unwrap();

        let project = project_at(dir.clone(), "");
        let source_id = SourceId::new(1).unwrap();
        let session = Session::new(&project, DriverConfig::default()).unwrap();
        let compiled = session.compile_file(&main_path, source_id).unwrap();

        let reporter = reporter_for(&project, ColorWhen::Never, MessageFormat::Human).unwrap();
        let out = reporter.render_all(compiled.diagnostics(), compiled.sources());

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
            params: CompileParams::default().with_opt(crate::OptLevel::O1),
            color: ColorWhen::Never,
            format: MessageFormat::Json,
            scope: Scope::File,
            timing: None,
        };
        let merged = merge_manifest_lints(&project, &cli);
        assert_eq!(merged.target, Target::Mir);
        assert_eq!(merged.scope, Scope::File);
        assert_eq!(merged.params.opt, crate::OptLevel::O1);
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
    fn a_project_compiles_with_its_experimental_table() {
        let opted_in = project("[experimental]\nenums = true\ntypes = true\n");
        let flags = opted_in.feature_flags();
        assert!(flags.enums && flags.types);
        assert!(!flags.interfaces);
        // The default project asks for nothing, and this test process sets no
        // `LEEK_EXPERIMENTAL_*` variable.
        assert_eq!(project("").feature_flags(), leek_span::FeatureFlags::none());
    }

    #[test]
    fn the_feature_summary_names_the_features_and_marks_the_env_only_ones() {
        let manifest = leek_span::FeatureFlags {
            enums: true,
            ..leek_span::FeatureFlags::none()
        };
        let env = leek_span::FeatureFlags {
            types: true,
            ..leek_span::FeatureFlags::none()
        };
        assert_eq!(
            feature_flags_summary(manifest, env).as_deref(),
            Some("experimental features: types (env), enums"),
        );
        // A feature the manifest already asks for is not an env surprise,
        // even when the variable is set as well.
        assert_eq!(
            feature_flags_summary(manifest, manifest).as_deref(),
            Some("experimental features: enums"),
        );
        // Nothing to say about a project that opts into nothing — no line at
        // all, rather than an empty list.
        let none = leek_span::FeatureFlags::none();
        assert_eq!(feature_flags_summary(none, none), None);
    }

    /// The reason a session interns its own ids: two entry files
    /// numbered out of one interner get two ids, and a helper both of
    /// them include keeps a third — where the per-file numbering handed
    /// entry 2 the id entry 1's include already owned (#191).
    #[test]
    fn one_interner_numbers_several_entries_without_collisions() {
        let interner: std::sync::Arc<dyn SourceInterner> = std::sync::Arc::new(PathInterner::new());
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

    /// `miku build --verbose` times the *stages* a compilation asks the
    /// database for, and the numbers describe the same work the plain
    /// build does.
    ///
    /// This used to assert the sink saw every planned pipeline step, once
    /// each. There are no steps to see: a session compilation answers from
    /// queries. The entries are stage names now — `diagnostics`, `hir` —
    /// which is a visible change to what `miku build --verbose` and
    /// `miku dev` print.
    ///
    /// Salsa cannot give back what was lost. It fires `WillExecute` before
    /// a query body runs and nothing on completion, so its event stream
    /// says which queries recomputed and never how long any took. The
    /// epic's "timing becomes a salsa event hook" is therefore a different
    /// measurement, not a port of this one; timing the calls keeps
    /// durations, which is what these three consumers print.
    ///
    /// Recorded lazily, when an accessor is first called, so the list
    /// reflects what the command actually asked for rather than what a
    /// recipe would have planned regardless.
    #[test]
    fn a_timing_sink_records_the_stages_the_compilation_asked_for() {
        let dir = scratch("timed");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        let project = project_at(dir.clone(), "[lint]\npedantic = true\n");

        let untimed = DriverConfig {
            color: ColorWhen::Never,
            ..DriverConfig::default()
        };
        let plain_session = Session::new(&project, untimed.clone()).expect("session");
        let plain = plain_session.compile_entry().expect("untimed run");

        let sink = TimingSink::new();
        let timed_session = Session::new(
            &project,
            DriverConfig {
                timing: Some(sink.clone()),
                ..untimed
            },
        )
        .expect("session");
        let timed = timed_session.compile_entry().expect("timed run");

        assert!(
            sink.entries().is_empty(),
            "nothing is computed until something is asked for"
        );

        assert_eq!(timed.had_error(), plain.had_error());
        let names: Vec<&str> = sink.entries().iter().map(|e| e.step).collect();
        assert_eq!(
            names,
            ["diagnostics"],
            "asking whether it failed costs exactly the diagnostic stream"
        );

        let _ = timed.hir();
        let names: Vec<&str> = sink.entries().iter().map(|e| e.step).collect();
        assert_eq!(names, ["diagnostics", "hir"], "and lowering adds one entry");

        let _ = timed.hir();
        assert_eq!(
            sink.entries().len(),
            2,
            "a second ask is a cache hit, not a second entry"
        );

        // The manifest's `pedantic = true` still reaches this path: the
        // timed and plain compilations agree about what counts as an error,
        // which is what the merge decides.
        assert_eq!(
            timed.diagnostics().len(),
            plain.diagnostics().len(),
            "timing changes what is measured, never what is reported"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compile_entry_reports_a_resolver_error_from_the_project_entry() {
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
        let session = Session::new(&project, config).expect("session");
        let run = session.compile_entry().expect("run");
        assert!(run.had_error(), "diagnostics: {:?}", run.diagnostics());
        assert!(
            run.diagnostics()
                .iter()
                .any(|d| d.code == codes::REDECLARED_SYMBOL),
            "diagnostics: {:?}",
            run.diagnostics()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compile_entry_is_clean_for_a_well_formed_entry() {
        let dir = scratch("run-entry-ok");
        std::fs::create_dir_all(dir.join("src")).expect("src dir");
        std::fs::write(dir.join("src/main.leek"), "return 1 + 1;\n").expect("entry");
        let project = project_at(dir.clone(), "");

        let config = DriverConfig {
            color: ColorWhen::Never,
            ..DriverConfig::default()
        };
        let session = Session::new(&project, config).expect("session");
        let run = session.compile_entry().expect("run");
        assert!(!run.had_error(), "diagnostics: {:?}", run.diagnostics());
        // Nothing aborted the pipeline either: the target was reached.
        assert!(run.hir().is_some());

        std::fs::remove_dir_all(&dir).ok();
    }
}
