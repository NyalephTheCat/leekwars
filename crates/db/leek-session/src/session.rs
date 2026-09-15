//! One compiler session over a project, and one compiled file.
//!
//! [`Session`] is what a front-end holds for a whole invocation: the
//! project, the driver configuration, the [`Reporter`] built from the
//! manifest's `[lint]` levels, and the include interner. [`Compilation`] is
//! what one compiled file *is*: the run, its text, its label, the
//! [`Sources`] map its diagnostics render against, and the reporter that
//! renders them.
//!
//! Before this, every `miku` subcommand assembled those five pieces itself
//! — and three of them re-read the entry file off disk a second time to do
//! it, because the driver returned a run and kept the text.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use leek_complexity::Complexity;
use leek_complexity::pipeline::ComplexityArtifact;
use leek_diagnostics::{Diagnostic, Reporter, Severity, Sources};
use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_mir::MirProgram;
use leek_mir::pipeline::MirArtifact;
use leek_pipeline::{Artifact, Pipeline, Run};
use leek_project::{Input, Project};
use leek_span::SourceId;

use crate::driver::{
    DriverConfig, PathInterner, SourceInterner, file_pipeline, file_pipeline_shared, reporter_for,
    run_sources,
};
use crate::error::SessionError;

/// The entry file's own `SourceId`; its includes get the ones after it.
const ENTRY_SOURCE: SourceId = match SourceId::new(1) {
    Some(id) => id,
    None => panic!("1 is not zero"),
};

/// One compiler invocation over one project.
///
/// Built once per command and borrowed by every file it compiles, so the
/// reporter is constructed once (and its `[lint]` failure reported once)
/// rather than per file, and every file of a run shares one include-id
/// space.
pub struct Session<'p> {
    project: &'p Project,
    config: DriverConfig,
    reporter: Reporter,
    interner: Arc<dyn SourceInterner>,
}

impl<'p> Session<'p> {
    /// Open a session on `project`.
    ///
    /// Fails when the manifest's `[lint]` table names a code the catalog
    /// does not know: there is no reporter to render anything through, and
    /// every command would otherwise discover that separately.
    pub fn new(project: &'p Project, config: DriverConfig) -> Result<Self, SessionError> {
        let reporter = reporter_for(project, config.color, config.format)?;
        Ok(Self {
            project,
            config,
            reporter,
            interner: Arc::new(PathInterner::new()),
        })
    }

    #[must_use]
    pub fn project(&self) -> &'p Project {
        self.project
    }

    #[must_use]
    pub fn config(&self) -> &DriverConfig {
        &self.config
    }

    /// The reporter every [`Compilation`] of this session renders through —
    /// the manifest's `[lint]` levels over the catalog defaults.
    #[must_use]
    pub fn reporter(&self) -> &Reporter {
        &self.reporter
    }

    /// Compile `path` with the entry numbered `source_id`, its includes
    /// resolved from disk and numbered after it.
    ///
    /// This is the one-file-per-id entry point: `check`, `lint`, `run`,
    /// `build`, `fix`, `analyze` and `doc` all compile through it, so they
    /// see the same include closure and the same lint groups.
    pub fn compile_file(
        &self,
        path: &Path,
        source_id: SourceId,
    ) -> Result<Compilation<'_>, SessionError> {
        let pipeline = file_pipeline(self.project, path, source_id, &self.config)?;
        self.compile(&pipeline, path, source_id)
    }

    /// Compile the project's entry file.
    pub fn compile_entry(&self) -> Result<Compilation<'_>, SessionError> {
        self.compile_file(&self.project.entry_path(), ENTRY_SOURCE)
    }

    /// [`compile_file`](Self::compile_file) for a command that compiles many
    /// entries in one invocation: the entry and its includes are numbered
    /// out of the session's own interner instead of counting up from a
    /// per-file id.
    ///
    /// `miku test` needs this — numbering each test file `1, 2, 3, …` while
    /// its includes take the ids just above the entry's hands file 2 the id
    /// file 1's first include already has (#191).
    pub fn compile_shared(&self, path: &Path) -> Result<Compilation<'_>, SessionError> {
        // Plan first: the pipeline interns the entry, and its id is what
        // this file's `Input` — and so every span it raises — has to carry.
        let (pipeline, source_id) =
            file_pipeline_shared(self.project, path, &self.config, &self.interner)?;
        self.compile(&pipeline, path, source_id)
    }

    /// Read `path`, build its [`Input`] and drive `pipeline` over it.
    ///
    /// The text is read exactly once per compiled file and then shared: the
    /// `Input`, the `Sources` map and [`Compilation::text`] are all the same
    /// buffer.
    fn compile(
        &self,
        pipeline: &Pipeline,
        path: &Path,
        source_id: SourceId,
    ) -> Result<Compilation<'_>, SessionError> {
        let text = std::fs::read_to_string(path).map_err(|source| SessionError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let lang = self.project.index().language_settings(&text);
        let input = Input {
            source: source_id,
            text: Arc::from(text),
            version_byte: lang.version,
            strict: lang.strict,
            // The project's flags, not `Input::from`'s per-conversion
            // `FeatureFlags::from_env`: `[experimental]` is part of the
            // project and has to reach the pipeline (leekwars#206).
            flags: self.project.feature_flags(),
        };
        Ok(Compilation::adopt(
            pipeline.run(input),
            path.display().to_string(),
            &self.reporter,
        ))
    }
}

/// One file compiled: the run, plus everything needed to say something
/// about it.
///
/// Borrowed from the [`Session`] that produced it, so a front-end holds one
/// of these for as long as it is still reporting on the file.
pub struct Compilation<'a> {
    run: Run<'a>,
    text: Arc<str>,
    label: String,
    reporter: &'a Reporter,
    /// Built on demand by [`sources`](Self::sources) — `analyze`, `doc` and
    /// `migrate` compile a whole tree and render nothing, and a source map
    /// costs a copy of every file's text plus a line table over it.
    sources: OnceLock<Sources>,
    /// Built on demand by [`db_handle`](Self::db_handle) — a front-end that
    /// never calls a tracked query never pays for a database.
    db: OnceLock<leek_db::LeekDb>,
    file: OnceLock<leek_db::SourceFile>,
}

impl<'a> Compilation<'a> {
    /// Wrap a run a front-end drove itself.
    ///
    /// The manifest-less half of [`Session::compile_file`], for `leekc`,
    /// which plans its own pipeline per `--emit` and has no `Miku.toml` to
    /// take a reporter from. The entry text comes off the run's own
    /// [`Input`], so there is nothing to keep in sync.
    #[must_use]
    pub fn adopt(run: Run<'a>, file_label: String, reporter: &'a Reporter) -> Self {
        Self {
            text: Arc::clone(&run.input().text),
            run,
            label: file_label,
            reporter,
            sources: OnceLock::new(),
            db: OnceLock::new(),
            file: OnceLock::new(),
        }
    }

    /// The compiled file's text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The label its diagnostics are headed with — the path as the caller
    /// spelled it.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Every file this compilation's diagnostics may point into: the entry
    /// plus each resolved include, each under its own `SourceId`.
    ///
    /// Assembled on the first call and kept, so the two commands that render
    /// both frontend and backend diagnostics build it once.
    #[must_use]
    pub fn sources(&self) -> &Sources {
        self.sources
            .get_or_init(|| run_sources(&self.run, &self.text, &self.label))
    }

    #[must_use]
    pub fn input(&self) -> &Input {
        self.run.input()
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        self.run.diagnostics()
    }

    /// Any artifact the planned pipeline produced. The escape hatch for the
    /// views with no named accessor below (tokens, the green tree, the
    /// formatted text); prefer [`hir`](Self::hir) and friends where they
    /// apply.
    #[must_use]
    pub fn get<A: Artifact>(&self) -> Option<&A> {
        self.run.get::<A>()
    }

    /// The lowered HIR, when the target reached it.
    #[must_use]
    pub fn hir(&self) -> Option<&HirFile> {
        self.run.get::<HirArtifact>().map(|a| a.0.as_ref())
    }

    /// The lowered MIR, when the target reached it.
    #[must_use]
    pub fn mir(&self) -> Option<&MirProgram> {
        self.run.get::<MirArtifact>().map(|a| a.0.as_ref())
    }

    /// The per-function complexity rows, when the target asked for them.
    #[must_use]
    pub fn complexity(&self) -> Option<&[Complexity]> {
        self.run.get::<ComplexityArtifact>().map(|a| a.0.as_slice())
    }

    /// Render this compilation's diagnostics through the session's reporter
    /// and report whether any survived at error level.
    ///
    /// Rendering is the side effect: call it once per file. The same answer
    /// without printing anything is [`had_error`](Self::had_error).
    pub fn report(&self) -> bool {
        self.reporter.emit(self.diagnostics(), self.sources())
    }

    /// Whether [`report`](Self::report) would report an error, without
    /// rendering anything — the manifest's `[lint]` levels applied, so an
    /// `allow`ed error does not count and a `deny`ed warning does.
    #[must_use]
    pub fn had_error(&self) -> bool {
        self.reporter
            .apply_levels(self.diagnostics())
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Render a *backend's* diagnostics against this compilation's sources,
    /// and report whether any was error-level.
    ///
    /// Same codes, carets and `[lint]` levels a frontend diagnostic gets, and
    /// the same source map — so a complaint raised inside an included file
    /// points at that file. Takes a slice rather than an error because both
    /// source-emitting backends produce output *and* complaints: whether a
    /// diagnosed file is fatal is the reporter's call, not the backend's.
    pub fn report_backend(&self, diagnostics: &[Diagnostic]) -> bool {
        if diagnostics.is_empty() {
            return false;
        }
        self.reporter.emit(diagnostics, self.sources())
    }

    /// This compilation as a salsa input: the database and the
    /// [`SourceFile`](leek_db::SourceFile) to call a tracked query with.
    ///
    /// What a front-end that wants `leek_fmt::format_query` — or any other
    /// query written over [`leek_db::Db`] — hands it. The pair is created on
    /// the first call and kept, so two queries asked of the same compilation
    /// share one memo table.
    ///
    /// The file is a faithful copy of the run's own `Input`, under the same
    /// canonical-path key the include graph uses (#181), so a query answers
    /// about the bytes the pipeline actually compiled.
    pub fn db_handle(&self) -> (&dyn leek_db::Db, leek_db::SourceFile) {
        let db = self.db.get_or_init(leek_db::LeekDb::default);
        let file = *self.file.get_or_init(|| {
            let input = self.run.input();
            leek_db::SourceFile::new(
                db,
                leek_span::paths::canonical_or_normalized(Path::new(&self.label))
                    .display()
                    .to_string(),
                input.source.get(),
                Arc::clone(&input.text),
                input.version_byte,
                input.strict,
                leek_types::seed_library_enabled(),
                input.flags.to_bits(),
            )
        });
        (db, file)
    }
}

#[cfg(test)]
mod tests {
    use leek_diagnostics::{ColorWhen, MessageFormat};
    use leek_manifest::ManifestLoad;

    use super::*;
    use crate::Target;

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "leek-session-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("create scratch dir");
        dir
    }

    fn project_at(root: std::path::PathBuf, extra: &str) -> Project {
        let toml = format!("[project]\nname = \"demo\"\nversion = \"0.1.0\"\n{extra}");
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

    fn quiet(target: Target) -> DriverConfig {
        DriverConfig {
            target,
            color: ColorWhen::Never,
            format: MessageFormat::Human,
            ..DriverConfig::default()
        }
    }

    #[test]
    fn the_entry_compiles_and_its_text_comes_back_without_a_second_read() {
        let dir = scratch("entry");
        std::fs::write(dir.join("src/main.leek"), "return 1 + 1;\n").expect("entry");
        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");

        let compiled = session.compile_entry().expect("compile");
        assert_eq!(compiled.text(), "return 1 + 1;\n");
        assert!(
            compiled.label().ends_with("main.leek"),
            "{}",
            compiled.label()
        );
        assert!(!compiled.had_error(), "{:?}", compiled.diagnostics());
        assert!(compiled.hir().is_some());
        assert_eq!(compiled.input().source, ENTRY_SOURCE);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_resolver_error_in_the_entry_is_an_error_without_rendering_it() {
        let dir = scratch("entry-error");
        std::fs::write(
            dir.join("src/main.leek"),
            "var a = 1;\nvar a = 2;\nreturn a;\n",
        )
        .expect("entry");
        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");

        let compiled = session.compile_entry().expect("compile");
        assert!(compiled.had_error(), "{:?}", compiled.diagnostics());
        assert!(
            compiled
                .diagnostics()
                .iter()
                .any(|d| d.code == leek_diagnostics::codes::REDECLARED_SYMBOL),
            "{:?}",
            compiled.diagnostics()
        );
        // …and `report` agrees with the predicate that does not print.
        assert!(compiled.report());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The manifest's `[lint] allow` reaches the session's reporter, so a
    /// file whose only error is allowed is not an error at all.
    #[test]
    fn the_manifest_lint_levels_decide_what_counts_as_an_error() {
        let dir = scratch("allowed");
        std::fs::write(
            dir.join("src/main.leek"),
            "var a = 1;\nvar a = 2;\nreturn a;\n",
        )
        .expect("entry");
        let project = project_at(dir.clone(), "[lint]\nallow = [\"E0202\"]\n");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");
        assert!(!session.compile_entry().expect("compile").had_error());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_lint_code_fails_the_session_instead_of_every_file() {
        let dir = scratch("bad-lint");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        let project = project_at(dir.clone(), "[lint]\ndeny = [\"NOPE9999\"]\n");
        let Err(err) = Session::new(&project, quiet(Target::Linted)) else {
            panic!("an unknown `[lint]` code must fail the session");
        };
        assert!(matches!(err, SessionError::LintLevel(_)), "{err:?}");
        assert!(err.to_string().contains("NOPE9999"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_is_an_io_error_naming_the_path() {
        let dir = scratch("missing");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");

        let missing = dir.join("src/gone.leek");
        let Err(err) = session.compile_file(&missing, ENTRY_SOURCE) else {
            panic!("compiling a file that does not exist must fail");
        };
        assert!(matches!(err, SessionError::Io { .. }), "{err:?}");
        assert!(err.to_string().contains("gone.leek"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The reason [`Session::compile_shared`] exists: two entries compiled
    /// in one invocation get two ids, and a helper both of them include
    /// keeps a third (#191).
    #[test]
    fn one_session_numbers_several_entries_without_collisions() {
        let dir = scratch("shared-ids");
        std::fs::create_dir_all(dir.join("tests")).expect("tests dir");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        std::fs::write(
            dir.join("src/helper.leek"),
            "function helper() { return 1; }\n",
        )
        .expect("helper");
        for name in ["first", "second"] {
            std::fs::write(
                dir.join(format!("tests/{name}.leek")),
                "include(\"helper\")\nreturn helper();\n",
            )
            .expect("test file");
        }
        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");

        let first = session
            .compile_shared(&dir.join("tests/first.leek"))
            .expect("first");
        let second = session
            .compile_shared(&dir.join("tests/second.leek"))
            .expect("second");
        assert_ne!(
            first.input().source,
            second.input().source,
            "two entries must not share an id"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `db_handle` is what a front-end hands a tracked query. The file it
    /// returns must describe the bytes the pipeline compiled, and asking
    /// twice must hand back the same input rather than a second one.
    #[test]
    fn the_db_handle_describes_the_compiled_file_and_is_built_once() {
        let dir = scratch("db-handle");
        std::fs::write(dir.join("src/main.leek"), "return 1 + 1;\n").expect("entry");
        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");
        let compiled = session.compile_entry().expect("compile");

        let (db, file) = compiled.db_handle();
        assert_eq!(&**file.text(db), "return 1 + 1;\n");
        assert_eq!(file.source(db), ENTRY_SOURCE);
        assert_eq!(file.version_byte(db), compiled.input().version_byte);
        assert!(
            file.path(db).is_some_and(|p| p.ends_with("main.leek")),
            "{:?}",
            file.path(db)
        );

        let (_, again) = compiled.db_handle();
        assert!(file == again, "the input is created once and kept");

        std::fs::remove_dir_all(&dir).ok();
    }
}
