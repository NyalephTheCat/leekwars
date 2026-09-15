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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use leek_complexity::Complexity;
use leek_complexity::pipeline::ComplexityArtifact;
use leek_diagnostics::{Diagnostic, Reporter, Severity, Sources};
use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_mir::MirProgram;
use leek_mir::pipeline::MirArtifact;
use leek_pipeline::{Artifact, Run};
use leek_project::{Input, Project};
use leek_span::SourceId;

use crate::Target;
use crate::driver::{DriverConfig, PathInterner, SourceInterner, reporter_for, run_sources};
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
    /// The one database every file of this invocation is compiled
    /// through.
    ///
    /// It lived on [`Compilation`] before, which meant a `miku test` over
    /// N files built N databases and shared nothing between them: the
    /// stdlib headers were re-parsed per file, and two test files that
    /// include the same helper each lowered it. A database per
    /// *invocation* is what makes a tracked query worth calling from a
    /// CLI at all (#129).
    db: leek_db::LeekDb,
    /// Which files an `include("…")` can resolve against, for the
    /// whole-program queries. Every file the project index knows, plus
    /// whatever their includes reach.
    files: leek_db::WorkspaceFiles,
    /// The input for each registered file, by canonical path, so
    /// compiling a file the session already knows reuses its input
    /// rather than minting a second one for the same bytes.
    inputs: BTreeMap<String, leek_db::SourceFile>,
}

impl<'p> Session<'p> {
    /// Open a session on `project`.
    ///
    /// Fails when the manifest's `[lint]` table names a code the catalog
    /// does not know: there is no reporter to render anything through, and
    /// every command would otherwise discover that separately.
    pub fn new(project: &'p Project, config: DriverConfig) -> Result<Self, SessionError> {
        let reporter = reporter_for(project, config.color, config.format)?;
        let interner: Arc<dyn SourceInterner> = Arc::new(PathInterner::new());
        let mut db = leek_db::LeekDb::default();
        let inputs = register_project_files(&mut db, project, interner.as_ref());
        let files = leek_db::WorkspaceFiles::empty(&db);
        files.set_all(&mut db, inputs.clone().into_iter().collect());
        Ok(Self {
            project,
            config,
            reporter,
            interner,
            db,
            files,
            inputs,
        })
    }

    /// The session's database, and the file set its whole-program queries
    /// resolve includes against.
    #[must_use]
    pub fn db(&self) -> (&leek_db::LeekDb, leek_db::WorkspaceFiles) {
        (&self.db, self.files)
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
        self.compile(path, source_id)
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
        // Intern first: the entry's id is what this file's `Input` — and so
        // every span it raises — has to carry. For a project file
        // `Session::input_for` already holds that id; this covers the rest.
        let source_id = self.interner.intern(path);
        self.compile(path, source_id)
    }

    /// Read `path`, build its [`Input`] and drive `pipeline` over it.
    ///
    /// The text is read exactly once per compiled file and then shared: the
    /// `Input`, the `Sources` map and [`Compilation::text`] are all the same
    /// buffer.
    fn compile(&self, path: &Path, source_id: SourceId) -> Result<Compilation<'_>, SessionError> {
        let text = std::fs::read_to_string(path).map_err(|source| SessionError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let lang = self.project.index().language_settings(&text);
        let text: Arc<str> = Arc::from(text);
        let file = self.input_for(path, source_id, &text, lang);
        let input = Input {
            // The session's id, not the caller's. See `input_for`.
            source: file.source(&self.db),
            text,
            version_byte: lang.version,
            strict: lang.strict,
            // The project's flags, not `Input::from`'s per-conversion
            // `FeatureFlags::from_env`: `[experimental]` is part of the
            // project and has to reach the pipeline (leekwars#206).
            flags: self.project.feature_flags(),
        };
        let label = path.display().to_string();
        let shape = self.shape();

        Ok(Compilation::in_session(
            input,
            label,
            &self.reporter,
            &self.db,
            file,
            self.files,
            shape,
            self.config.timing.clone(),
        ))
    }

    /// What this session's target and params mean for a run's diagnostic
    /// stream.
    fn shape(&self) -> RunShape {
        use leek_db::queries::Stage;
        let stage = match self.config.target {
            Target::Tokens => Stage::Tokens,
            Target::Parsed => Stage::Parsed,
            Target::Resolved => Stage::Resolved,
            Target::TypeChecked => Stage::TypeChecked,
            // `Complexity` and `Mir` plan past HIR but add no diagnostics
            // the program queries can answer for: complexity reports none,
            // and there is no program-level MIR lowering to ask.
            Target::Hir | Target::Complexity | Target::Mir | Target::Linted => Stage::Hir,
        };
        // Through the same merge the planner applies, not
        // `self.config.params` raw: `merge_manifest_lints` ORs the
        // manifest's `[lint] pedantic/nursery` into the CLI's, and it
        // returns a *copy*, so the session's own config never sees them. A
        // project that switches a group on in `Miku.toml` would otherwise
        // have it silently ignored — which is what
        // `test_honors_manifest_lint_groups_like_check` caught.
        let merged = crate::driver::merge_manifest_lints(self.project, &self.config);
        RunShape {
            stage,
            scope: merged.scope,
            lints: (merged.target == Target::Linted).then_some(merged.params.lints),
            stop_at: merged.params.stop_on_diagnostics,
            opt: merged.params.opt,
        }
    }

    /// This file's salsa input: the one [`register_project_files`] made
    /// when the index knows the file, otherwise a fresh one carrying
    /// `fallback_id`.
    ///
    /// **The session's interner is the single id authority.** A registered
    /// input already carries an id, and [`compile`](Self::compile) builds
    /// its [`Input`] from *that* rather than from the caller's
    /// `source_id`. Two authorities would mean the pipeline's diagnostics
    /// and the queries' disagree about which file a span belongs to, which
    /// `Sources` and
    /// [`for_source`](leek_db::queries::for_source) both key on — so a
    /// diagnostic would render against the wrong file or vanish.
    ///
    /// It also generalizes what [`compile_shared`](Self::compile_shared)
    /// was added for. Numbering an entry `1, 2, 3, …` while its includes
    /// take ids from the interner hands one file the id another already
    /// has (#191); taking both from the interner cannot.
    ///
    /// `fallback_id` is therefore used only for a file the session does
    /// not know — one outside the project index. Such a file deliberately
    /// does not join [`files`](Self::files) either: a file the index never
    /// saw is not part of the program, and adding it to the set every
    /// include resolves against would let a stray file outside the tree
    /// satisfy an `include`.
    fn input_for(
        &self,
        path: &Path,
        fallback_id: SourceId,
        text: &Arc<str>,
        lang: leek_span::pragma::LanguageSettings,
    ) -> leek_db::SourceFile {
        let canonical = leek_span::paths::canonical_or_normalized(path)
            .display()
            .to_string();
        if let Some(known) = self.inputs.get(&canonical) {
            return *known;
        }
        leek_db::SourceFile::new(
            &self.db,
            canonical,
            fallback_id.get(),
            Arc::clone(text),
            lang.version,
            lang.strict,
            leek_types::seed_library_enabled(),
            self.project.feature_flags().to_bits(),
        )
    }
}

/// Register every file the project index knows as a salsa input, keyed by
/// canonical path.
///
/// Version and strict mode are settled the same way
/// [`Session::compile`] settles the entry's — the index's
/// `language_settings` over the file's own text — so a query and the
/// pipeline agree about what language a file is written in.
///
/// A file the index cannot read is skipped rather than failing the
/// session: an unreadable file in the tree is the compile's problem to
/// report when something includes it, not a reason for every command to
/// refuse to start.
///
/// # Cost
///
/// This reads every project file up front, which `ProjectIndex` does not
/// — it carries paths, not text. So `Session::new` does I/O it did not do
/// before, and a command that compiles one file of a large project pays
/// for the whole tree.
///
/// Deliberate, and worth revisiting if a project ever gets big: a
/// LeekWars project is tens of files, every command that matters
/// (`check`, `test`, `build`) compiles all of them anyway, and the
/// alternative — building this lazily behind a `OnceLock` so only a
/// caller that asks a query pays — costs a second lifetime parameter on
/// [`Compilation`] and every signature that names it. That churn is
/// easier to justify once a front-end actually reads a query; until then
/// the simple shape is the honest one.
fn register_project_files(
    db: &mut leek_db::LeekDb,
    project: &Project,
    interner: &dyn SourceInterner,
) -> BTreeMap<String, leek_db::SourceFile> {
    let index = project.index();
    let flags = project.feature_flags().to_bits();
    let seed = leek_types::seed_library_enabled();
    let mut out = BTreeMap::new();
    let register = |db: &mut leek_db::LeekDb,
                    out: &mut BTreeMap<String, leek_db::SourceFile>,
                    path: &Path|
     -> Option<leek_db::SourceFile> {
        let text = std::fs::read_to_string(path).ok()?;
        let canonical = leek_span::paths::canonical_or_normalized(path)
            .display()
            .to_string();
        if let Some(known) = out.get(&canonical) {
            return Some(*known);
        }
        let lang = index.language_settings(&text);
        let file = leek_db::SourceFile::new(
            db,
            canonical.clone(),
            interner.intern(Path::new(&canonical)).get(),
            Arc::from(text),
            lang.version,
            lang.strict,
            seed,
            flags,
        );
        out.insert(canonical, file);
        Some(file)
    };

    let mut frontier = Vec::new();
    for path in index.files() {
        if let Some(file) = register(db, &mut out, path) {
            frontier.push(file);
        }
    }

    // Close the set under includes. `resolve_include` resolves a name
    // against `WorkspaceFiles` and nothing else — a tracked query that read
    // the filesystem would be impure — while the pipeline this replaced
    // resolved through a folder that fell back to `DiskFolder`. So an
    // `include("../shared/lib")` escaping the project root resolved before
    // and would report `E0272 IncludeNotFound` now.
    //
    // The same gap, and the same fix, as `leek_lsp::Workspace::resync`. It
    // is written twice because the two reach files differently — the server
    // has open buffers that shadow disk — but the rule is one rule: a file
    // an include names is part of the program whether or not the index
    // walked it.
    while let Some(file) = frontier.pop() {
        let includer = PathBuf::from(file.canonical_path(db).clone());
        if includer.as_os_str().is_empty() {
            continue;
        }
        for call in leek_db::queries::include_edges(db, file).includes {
            for candidate in leek_resolver::folder::include_candidates(&includer, &call.name) {
                let key = leek_span::paths::canonical_or_normalized(&candidate)
                    .display()
                    .to_string();
                if out.contains_key(&key) {
                    break;
                }
                if let Some(found) = register(db, &mut out, &candidate) {
                    frontier.push(found);
                    break;
                }
            }
        }
    }
    out
}

/// One file compiled: the run, plus everything needed to say something
/// about it.
///
/// Borrowed from the [`Session`] that produced it, so a front-end holds one
/// of these for as long as it is still reporting on the file.
pub struct Compilation<'a> {
    /// The pipeline run, for an adopted compilation. `None` for a session
    /// compilation, which answers from queries and never plans one.
    run: Option<Run<'a>>,
    text: Arc<str>,
    label: String,
    reporter: &'a Reporter,
    /// Built on demand by [`sources`](Self::sources) — `analyze`, `doc` and
    /// `migrate` compile a whole tree and render nothing, and a source map
    /// costs a copy of every file's text plus a line table over it.
    sources: OnceLock<Sources>,
    /// The session's database and this file's input in it, when the
    /// compilation came from a [`Session`]. `None` for an adopted run
    /// (`leekc`, which plans its own pipeline and has no session).
    db: Option<(&'a leek_db::LeekDb, leek_db::SourceFile)>,
    /// The session's file set, for the whole-program queries.
    files: Option<leek_db::WorkspaceFiles>,
    /// The entry's `Input`, kept rather than read off the run: a session
    /// compilation has no run to read it from.
    input: Input,
    /// Where to record how long each stage took, when the driver asked
    /// for timings.
    timing: Option<leek_pipeline::TimingSink>,
    /// Query answers behind the reference-returning accessors. Each holds
    /// an `Arc`, so the cache is a pointer rather than a copy of the tree.
    tokens_cache: OnceLock<Option<Arc<Vec<leek_syntax::Token>>>>,
    hir_cache: OnceLock<Option<Arc<HirFile>>>,
    mir_cache: OnceLock<Option<Arc<MirProgram>>>,
    complexity_cache: OnceLock<Option<Arc<Vec<Complexity>>>>,
    diagnostics_cache: OnceLock<Arc<Vec<Diagnostic>>>,
    /// How far the run's target reaches, and whether a parse error stops
    /// it — everything [`query_diagnostics`](Self::query_diagnostics)
    /// needs to reproduce this run's stream.
    shape: Option<RunShape>,
}

/// What a run's target and params mean for its diagnostic stream.
#[derive(Debug, Clone, Copy)]
struct RunShape {
    stage: leek_db::queries::Stage,
    /// Whether the entry's includes are part of the compilation.
    scope: crate::Scope,
    /// The lint groups the run planned with, when the target plans the
    /// lint step at all. `None` when it does not.
    lints: Option<leek_lint::LintGroups>,
    /// `RecipeParams::stop_on_diagnostics` — the severity at which the
    /// parse aborts the run.
    stop_at: Option<Severity>,
    /// The optimization level the recipe planned with.
    opt: leek_pipeline::OptLevel,
}

/// Time `f` into `sink` under `name`, or just run it when there is none.
fn timed<T>(
    sink: Option<&leek_pipeline::TimingSink>,
    name: &'static str,
    f: impl FnOnce() -> T,
) -> T {
    match sink {
        Some(sink) => sink.time(name, f),
        None => f(),
    }
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
            input: run.input().clone(),
            run: Some(run),
            label: file_label,
            reporter,
            sources: OnceLock::new(),
            db: None,
            files: None,
            shape: None,
            timing: None,
            tokens_cache: OnceLock::new(),
            hir_cache: OnceLock::new(),
            mir_cache: OnceLock::new(),
            complexity_cache: OnceLock::new(),
            diagnostics_cache: OnceLock::new(),
        }
    }

    /// A compilation a [`Session`] produced: no run at all, every answer
    /// from the session's database.
    ///
    /// The pipeline is not planned and not driven. Serving one accessor
    /// from a query while another read a run would pay for both — a run
    /// planned for a late target has already computed everything on the
    /// way there — which is why the move is all-or-nothing rather than
    /// consumer by consumer.
    #[must_use]
    fn in_session(
        input: Input,
        file_label: String,
        reporter: &'a Reporter,
        db: &'a leek_db::LeekDb,
        file: leek_db::SourceFile,
        files: leek_db::WorkspaceFiles,
        shape: RunShape,
        timing: Option<leek_pipeline::TimingSink>,
    ) -> Self {
        Self {
            text: Arc::clone(&input.text),
            input,
            run: None,
            label: file_label,
            reporter,
            sources: OnceLock::new(),
            db: Some((db, file)),
            files: Some(files),
            shape: Some(shape),
            timing,
            tokens_cache: OnceLock::new(),
            hir_cache: OnceLock::new(),
            mir_cache: OnceLock::new(),
            complexity_cache: OnceLock::new(),
            diagnostics_cache: OnceLock::new(),
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
        self.sources.get_or_init(|| {
            if let Some(run) = &self.run {
                return run_sources(run, &self.text, &self.label);
            }
            let mut sources = Sources::single(self.input.source, &self.label, &*self.text);
            // A file-scoped compilation resolves no include, so nothing it
            // reports can point into one — and building the graph to find
            // that out would read the very files this scope exists to
            // leave alone.
            if self.scope() == crate::Scope::File {
                return sources;
            }
            let graph = self.with_program(|db, files, file, version| {
                leek_db::queries::include_graph(db, files, file, version)
            });
            if let (Some((db, _)), Some(graph)) = (self.db, graph) {
                for inc in graph.includes() {
                    sources.push(
                        inc.source,
                        inc.path.display().to_string(),
                        &**inc.file.text(db),
                    );
                }
            }
            sources
        })
    }

    #[must_use]
    pub fn input(&self) -> &Input {
        &self.input
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        if let Some(run) = &self.run {
            return run.diagnostics();
        }
        self.diagnostics_cache.get_or_init(|| {
            timed(self.timing.as_ref(), "diagnostics", || {
                self.query_diagnostics().unwrap_or_default()
            })
        })
    }

    /// Any artifact the planned pipeline produced. The escape hatch for the
    /// views with no named accessor below (tokens, the green tree, the
    /// formatted text); prefer [`hir`](Self::hir) and friends where they
    /// apply.
    #[must_use]
    pub fn get<A: Artifact>(&self) -> Option<&A> {
        self.run.as_ref()?.get::<A>()
    }

    /// The entry's green tree.
    ///
    /// A named accessor because [`get`](Self::get) reads the run's
    /// artifact bag, and a session compilation has no run. Parsed under
    /// the *program's* class set, which is what the include-aware pipeline
    /// parsed the entry under.
    #[must_use]
    pub fn green_tree(&self) -> Option<leek_syntax::language::GreenNode> {
        if let Some(run) = &self.run {
            return run
                .get::<leek_parser::pipeline::GreenTreeArtifact>()
                .map(|g| g.0.clone());
        }
        self.with_program(|db, files, file, version| {
            let classes = self.classes(db, files, file, version);
            leek_db::queries::parse_query(db, file, classes).green
        })
    }

    /// The entry's tokens.
    ///
    /// Lexing is per file, so this is the one accessor a
    /// [`Scope::File`](crate::Scope::File) compilation answers exactly as
    /// a program-scoped one does.
    #[must_use]
    pub fn tokens(&self) -> Option<&[leek_syntax::Token]> {
        if let Some(run) = &self.run {
            return run
                .get::<leek_lexer::pipeline::TokensArtifact>()
                .map(|t| t.0.tokens.as_slice());
        }
        self.tokens_cache
            .get_or_init(|| {
                timed(self.timing.as_ref(), "tokens", || {
                    self.with_program(|db, _, file, _| {
                        Arc::new(leek_db::queries::lex_query(db, file).tokens)
                    })
                })
            })
            .as_deref()
            .map(Vec::as_slice)
    }

    /// The entry, formatted under `options`.
    ///
    /// Unverified, exactly as the pipeline's `Fmt` step left it: whether
    /// the formatted text still parses to the same program is the
    /// caller's check to make (`leek_fmt::check_equivalence`), because
    /// only the caller knows whether it is about to print the result or
    /// write it over the original.
    ///
    /// The options are an argument rather than part of the session
    /// because they are interned into the query key: two callers that
    /// assemble the same settings share one memo, and a caller that
    /// changes them does not invalidate anything else about the file.
    #[must_use]
    pub fn formatted(&self, options: &leek_fmt::FormatOptions) -> Option<Arc<String>> {
        if let Some(run) = &self.run {
            return run
                .get::<leek_fmt::pipeline::FormattedArtifact>()
                .map(|f| Arc::clone(&f.0));
        }
        let (db, file) = self.db?;
        let db: &dyn leek_db::Db = db;
        let config = leek_fmt::pipeline::FormatConfig::new(db, options.clone());
        Some(
            timed(self.timing.as_ref(), "fmt", || {
                leek_fmt::pipeline::format_query(db, file, config)
            })
            .text,
        )
    }

    /// The lowered HIR, when the target reached it.
    ///
    /// From `lower_program` for a session compilation, from the run for an
    /// adopted one. Cached because the signature hands out a reference and
    /// a tracked query returns a value; the `Arc` inside makes the cache a
    /// pointer, not a copy of the tree.
    #[must_use]
    pub fn hir(&self) -> Option<&HirFile> {
        if let Some(run) = &self.run {
            return run.get::<HirArtifact>().map(|a| a.0.as_ref());
        }
        self.hir_cache
            .get_or_init(|| {
                timed(self.timing.as_ref(), "hir", || {
                    self.whole_program(|db, files, file, version| {
                        leek_db::queries::lower_program(db, files, file, version, self.opt()).hir
                    })
                })
            })
            .as_deref()
    }

    /// The lowered MIR, when the target reached it.
    #[must_use]
    pub fn mir(&self) -> Option<&MirProgram> {
        if let Some(run) = &self.run {
            return run.get::<MirArtifact>().map(|a| a.0.as_ref());
        }
        self.mir_cache
            .get_or_init(|| {
                timed(self.timing.as_ref(), "mir", || {
                    self.whole_program(|db, files, file, version| {
                        leek_db::queries::lower_program_mir(db, files, file, version, self.opt())
                            .program
                    })
                })
            })
            .as_deref()
    }

    /// The per-function complexity rows, when the target asked for them.
    #[must_use]
    pub fn complexity(&self) -> Option<&[Complexity]> {
        if let Some(run) = &self.run {
            return run.get::<ComplexityArtifact>().map(|a| a.0.as_slice());
        }
        self.complexity_cache
            .get_or_init(|| {
                timed(self.timing.as_ref(), "complexity", || {
                    self.whole_program(|db, files, file, version| {
                        leek_db::queries::program_complexity(db, files, file, version).0
                    })
                })
            })
            .as_deref()
            .map(Vec::as_slice)
    }

    /// The optimization level this compilation's target asked for.
    fn opt(&self) -> leek_pipeline::OptLevel {
        self.shape.map_or(leek_pipeline::OptLevel::O0, |s| s.opt)
    }

    /// Whether this compilation's includes are part of it.
    fn scope(&self) -> crate::Scope {
        self.shape.map_or(crate::Scope::Program, |s| s.scope)
    }

    /// [`with_program`](Self::with_program), but `None` under
    /// [`Scope::File`](crate::Scope::File).
    ///
    /// HIR, MIR and complexity are whole-program answers: they are built
    /// over the merged tree of the include closure, and there is no
    /// file-scoped query that means the same thing at the
    /// [`OptLevel`](leek_pipeline::OptLevel) the program ones are keyed
    /// on. A file-scoped compilation never plans those passes, so `None`
    /// is the honest answer rather than a program-wide one it did not
    /// ask for.
    fn whole_program<T>(
        &self,
        f: impl FnOnce(
            &leek_db::LeekDb,
            leek_db::WorkspaceFiles,
            leek_db::SourceFile,
            leek_syntax::Version,
        ) -> T,
    ) -> Option<T> {
        match self.scope() {
            crate::Scope::Program => self.with_program(f),
            crate::Scope::File => None,
        }
    }

    /// The class set this compilation's entry parses under: the
    /// program's, or the empty one when the includes are not part of it.
    fn classes<'db>(
        &self,
        db: &'db leek_db::LeekDb,
        files: leek_db::WorkspaceFiles,
        file: leek_db::SourceFile,
        version: leek_syntax::Version,
    ) -> leek_db::ProgramClasses<'db> {
        match self.scope() {
            crate::Scope::Program => leek_db::queries::program_classes(db, files, file, version),
            crate::Scope::File => leek_db::ProgramClasses::none(db),
        }
    }

    /// Run `f` against this compilation's program key, when it has one.
    ///
    /// Available under either [`Scope`](crate::Scope) — `files` is the
    /// session's file set either way. What a file-scoped compilation must
    /// not do is *use* it to reach past the entry, which is why the
    /// accessors above take their class set from
    /// [`classes`](Self::classes) and the whole-program ones below refuse
    /// outright.
    fn with_program<T>(
        &self,
        f: impl FnOnce(
            &leek_db::LeekDb,
            leek_db::WorkspaceFiles,
            leek_db::SourceFile,
            leek_syntax::Version,
        ) -> T,
    ) -> Option<T> {
        let (db, file) = self.db?;
        let files = self.files?;
        let version = leek_syntax::pipeline::version_from_byte(file.version_byte(db));
        Some(f(db, files, file, version))
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
    pub fn db_handle(&self) -> Option<(&dyn leek_db::Db, leek_db::SourceFile)> {
        self.db.map(|(db, file)| (db as &dyn leek_db::Db, file))
    }

    /// This compilation's diagnostics, computed from tracked queries
    /// instead of read off the run.
    ///
    /// `None` for an adopted run, which has no session and so no database
    /// to ask.
    ///
    /// Reproducing a `Run`'s stream takes two things beyond calling a
    /// query, both established by the tests below:
    ///
    /// 1. **The stage.** A run reports what the steps it *ran* reported,
    ///    so the stream is sliced to the target's
    ///    [`Stage`](leek_db::queries::Stage) rather than always reporting
    ///    the whole frontend.
    /// 2. **The abort.** `leek_parser::pipeline::Parse` is the one
    ///    production step implementing `RecipeStepStopOnError`, so when
    ///    `RecipeParams::stop_on_diagnostics` is set and the parse itself
    ///    reports at or above that severity, `Pipeline::drive` stops
    ///    before any later step. The check below is that rule: the
    ///    *entry's own* parse, because the closure's parses happen in
    ///    `ResolveIncludes`, which runs before `Parse` and so is not part
    ///    of what the wrapper counts as new.
    ///
    /// `Severity` orders `Error < Warning`, so "at or above `min`" is
    /// `severity <= min` — the same comparison `StopOnDiagnostics` makes.
    #[must_use]
    pub fn query_diagnostics(&self) -> Option<Arc<Vec<Diagnostic>>> {
        use leek_db::queries;

        let (db, file) = self.db?;
        let files = self.files?;
        let shape = self.shape?;
        let version = leek_syntax::pipeline::version_from_byte(file.version_byte(db));

        // Only once the target actually reaches parsing. A `Target::Tokens`
        // run never plans `Parse`, so there is nothing to abort and no
        // parse diagnostics to report — checking anyway would hand a
        // tokens-only run the parser's findings.
        let upto = |stage| match shape.scope {
            crate::Scope::Program => {
                queries::program_diagnostics_upto(db, files, file, version, stage)
            }
            crate::Scope::File => queries::file_diagnostics_upto(db, file, stage),
        };

        if let Some(min) = shape.stop_at
            && shape.stage >= queries::Stage::Parsed
        {
            let classes = self.classes(db, files, file, version);
            let parsed = queries::parse_query(db, file, classes);
            if parsed.diagnostics.iter().any(|d| d.severity <= min) {
                return Some(upto(queries::Stage::Parsed));
            }
        }

        Some(match shape.lints {
            Some(groups) => leek_lint::pipeline::program_diagnostics_with_lints(
                db, files, file, version, groups,
            ),
            None => upto(shape.stage),
        })
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

        let (db, file) = compiled
            .db_handle()
            .expect("a session compilation has a database");
        assert_eq!(&**file.text(db), "return 1 + 1;\n");
        assert_eq!(file.version_byte(db), compiled.input().version_byte);
        assert_eq!(
            file.source(db),
            compiled.input().source,
            "the input and the query answer about the same SourceId"
        );
        assert!(
            file.path(db)
                .is_some_and(|p: &str| p.ends_with("main.leek")),
            "{:?}",
            file.path(db)
        );

        let (_, again) = compiled.db_handle().expect("still there");
        assert!(file == again, "the input is created once and kept");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every target's `Run` stream is exactly its stage's query stream.
    ///
    /// The claim `the_program_stream_reports_what_the_pipeline_reported`
    /// makes at `Target::Linted`, made at every target below it — which is
    /// what `the_runs_diagnostics_grow_with_the_target` showed was
    /// missing, and what `diagnostics()` needs before it can move off the
    /// run.
    ///
    /// The fixture parses cleanly and earns its complaints after parsing,
    /// which is the case the slicing is for. A file that fails to parse is
    /// a different story — see
    /// `a_failed_parse_stops_the_run_but_not_the_queries`.
    #[test]
    fn every_targets_run_matches_its_stage() {
        let dir = scratch("stage-parity");
        std::fs::write(
            dir.join("src/main.leek"),
            "var a = 1;\nvar a = 2;\nvar z = 1 / 0;\nreturn a;\n",
        )
        .expect("entry");
        let project = project_at(dir.clone(), "");

        let cases = [
            (Target::Tokens, leek_db::queries::Stage::Tokens),
            (Target::Parsed, leek_db::queries::Stage::Parsed),
            (Target::Resolved, leek_db::queries::Stage::Resolved),
            (Target::TypeChecked, leek_db::queries::Stage::TypeChecked),
            (Target::Hir, leek_db::queries::Stage::Hir),
        ];

        let mut widths = Vec::new();
        for (target, stage) in cases {
            let session = Session::new(&project, quiet(target)).expect("session");
            let compiled = session.compile_entry().expect("compile");
            let from_run: Vec<&str> = compiled.diagnostics().iter().map(|d| d.code.id()).collect();

            let (db, files) = session.db();
            let (_, file) = compiled.db_handle().expect("session database");
            let sliced = leek_db::queries::program_diagnostics_upto(
                db,
                files,
                file,
                leek_syntax::pipeline::version_from_byte(file.version_byte(db)),
                stage,
            );
            let from_query: Vec<&str> = sliced.iter().map(|d| d.code.id()).collect();

            assert_eq!(
                from_run, from_query,
                "{target:?} and {stage:?} must report the same stream"
            );
            widths.push((target, from_run.len()));
        }

        std::fs::remove_dir_all(&dir).ok();

        // Non-trivial: a later stage reports strictly more than an
        // earlier one. Without this the assertions above would hold just
        // as well for five empty lists.
        let at = |t: Target| widths.iter().find(|(x, _)| *x == t).expect("case").1;
        assert_eq!(at(Target::Parsed), 0, "this fixture parses: {widths:?}");
        assert!(
            at(Target::Resolved) > at(Target::Parsed),
            "resolution adds the redeclaration: {widths:?}"
        );
    }

    /// The program-level MIR and complexity queries answer what the
    /// pipeline's `Target::Mir` and `Target::Complexity` runs produce, and
    /// — the point of them — they see the whole closure.
    ///
    /// `leek_mir::pipeline`'s `run_lower_mir` notes (#428) that its salsa
    /// branch lowers the entry alone, so a memoized include-aware
    /// `Target::Mir` run would silently drop every included function. It is
    /// dormant because nothing asks a memoized run for that target, but it
    /// is exactly the trap `Compilation` would fall into on being moved to
    /// queries. The assertions below are on the *included* function, so a
    /// per-file answer fails them.
    #[test]
    fn the_program_mir_and_complexity_queries_see_the_closure() {
        let dir = scratch("program-mir");
        std::fs::write(
            dir.join("src/main.leek"),
            "include(\"util\")\nreturn helper(3);\n",
        )
        .expect("entry");
        std::fs::write(
            dir.join("src/util.leek"),
            "function helper(n) {\n\tvar t = 0;\n\tfor (var i = 0; i < n; i++) { t = t + i; }\n\treturn t;\n}\n",
        )
        .expect("include");
        let project = project_at(dir.clone(), "");

        let mir_session = Session::new(&project, quiet(Target::Mir)).expect("session");
        let mir_compiled = mir_session.compile_entry().expect("compile");
        let run_mir = mir_compiled
            .mir()
            .expect("the pipeline lowered MIR")
            .clone();
        let (db, files) = mir_session.db();
        let (_, file) = mir_compiled.db_handle().expect("session database");
        let version = leek_syntax::pipeline::version_from_byte(file.version_byte(db));
        let query_mir = leek_db::queries::lower_program_mir(
            db,
            files,
            file,
            version,
            leek_pipeline::OptLevel::O0,
        );

        let cx_session = Session::new(&project, quiet(Target::Complexity)).expect("session");
        let cx_compiled = cx_session.compile_entry().expect("compile");
        let run_cx: Vec<String> = cx_compiled
            .complexity()
            .expect("the pipeline analyzed")
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let (cx_db, cx_files) = cx_session.db();
        let (_, cx_file) = cx_compiled.db_handle().expect("session database");
        let query_cx = leek_db::queries::program_complexity(
            cx_db,
            cx_files,
            cx_file,
            leek_syntax::pipeline::version_from_byte(cx_file.version_byte(cx_db)),
        );
        let query_cx_names: Vec<String> = query_cx.0.iter().map(|c| c.name.clone()).collect();

        std::fs::remove_dir_all(&dir).ok();

        // The closure's function is the discriminator: a per-file answer
        // would not have it.
        assert!(
            run_cx.iter().any(|n| n == "helper"),
            "the pipeline measured the included function: {run_cx:?}"
        );
        assert_eq!(
            run_cx, query_cx_names,
            "complexity: the query measured the same functions"
        );
        assert_eq!(
            run_mir.functions.len(),
            query_mir.program.functions.len(),
            "MIR: the query lowered the same functions"
        );
        assert_eq!(run_mir, *query_mir.program, "MIR: and the same program");
    }

    /// `query_diagnostics` reproduces the run's stream across every
    /// target, both stop-on-error settings, and a file that parses and one
    /// that does not.
    ///
    /// The whole matrix in one test on purpose. Each of the three axes hid
    /// a divergence in turn — the target hid one until
    /// `the_runs_diagnostics_grow_with_the_target`, the parse failure hid
    /// one until a fixture was strengthened, and the threshold is what
    /// turned out to explain the second. A test that fixed any axis would
    /// have gone green over the bug that axis conceals.
    #[test]
    fn query_diagnostics_reproduces_the_run_across_the_matrix() {
        let clean = "var a = 1;\nvar a = 2;\nvar z = 1 / 0;\nreturn a;\n";
        let broken = "var a = 1;\nvar a = 2;\nvar bad = \u{a3};\n";

        let targets = [
            Target::Tokens,
            Target::Parsed,
            Target::Resolved,
            Target::TypeChecked,
            Target::Hir,
            Target::Linted,
        ];

        let mut compared = 0;
        for (label, src) in [("clean", clean), ("broken", broken)] {
            for stop in [Some(Severity::Error), None] {
                for target in targets {
                    let dir = scratch(&format!("matrix-{label}-{}-{target:?}", stop.is_some()));
                    std::fs::write(dir.join("src/main.leek"), src).expect("entry");
                    let project = project_at(dir.clone(), "");

                    let mut params = leek_pipeline::RecipeParams::default();
                    params.stop_on_diagnostics = stop;
                    let session = Session::new(
                        &project,
                        DriverConfig {
                            target,
                            color: ColorWhen::Never,
                            format: MessageFormat::Human,
                            params,
                            ..DriverConfig::default()
                        },
                    )
                    .expect("session");
                    let compiled = session.compile_entry().expect("compile");

                    // Compared on `(code, span)`, not code alone. The span
                    // carries the `SourceId`, which is what `Sources` and
                    // `for_source` key on to attribute a diagnostic to a
                    // file — two streams with the same codes and different
                    // ids render against the wrong file or vanish, and a
                    // code-only comparison cannot see it.
                    let from_run: Vec<_> = compiled
                        .diagnostics()
                        .iter()
                        .map(|d| (d.code.id(), d.span))
                        .collect();
                    let queried = compiled.query_diagnostics().expect("session compilation");
                    let from_query: Vec<_> =
                        queried.iter().map(|d| (d.code.id(), d.span)).collect();

                    std::fs::remove_dir_all(&dir).ok();

                    assert_eq!(
                        from_run, from_query,
                        "{label} source, stop={stop:?}, {target:?}"
                    );
                    compared += 1;
                }
            }
        }

        assert_eq!(compared, 24, "every combination was actually compared");
    }

    /// A file that fails to parse stops the run's later steps, and does
    /// not stop the queries.
    ///
    /// The second divergence between a `Run`'s stream and a query's, and
    /// the one still open. Slicing to a stage handles the first —
    /// `every_targets_run_matches_its_stage` — but not this.
    ///
    /// The mechanism, precisely (an earlier version of this comment, and
    /// the commit that introduced it, blamed a missing `AstArtifact`;
    /// that was wrong — `Parse` always inserts one, because the root cast
    /// cannot fail). `leek_parser::pipeline::Parse` is the single
    /// production step implementing `RecipeStepStopOnError`, so when
    /// `RecipeParams::stop_on_diagnostics` is set it is wrapped in
    /// `StopOnDiagnostics::abort`. That records the diagnostic count
    /// before the step, and if the *parse itself* adds one at or above
    /// the threshold it calls `Context::abort`, which makes
    /// `Pipeline::drive` break before any later step runs. Nothing
    /// no-ops; the pipeline simply stops.
    ///
    /// The tracked passes have no such notion. They work off the green
    /// tree, which always exists, so they carry on and report against a
    /// tree the parser has already given up on.
    ///
    /// `a_permissive_run_does_not_stop_and_so_agrees` is the other half
    /// of that claim: drop the threshold and the run keeps going, and the
    /// two streams line up again. That is what makes this a statement
    /// about the abort rather than about parse errors in general.
    ///
    /// Not obviously the wrong answer — more is arguably better than
    /// silence — but it is a *different* answer, and deciding which one
    /// `miku` should print is a behaviour call, not a refactor. So this
    /// records the gap rather than papering over it, and `diagnostics()`
    /// stays on the run until it is closed.
    #[test]
    fn a_failed_parse_stops_the_run_but_not_the_queries() {
        let dir = scratch("failed-parse");
        std::fs::write(
            dir.join("src/main.leek"),
            "var a = 1;\nvar a = 2;\nvar bad = \u{a3};\n",
        )
        .expect("entry");
        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Resolved)).expect("session");
        let compiled = session.compile_entry().expect("compile");

        let from_run: Vec<&str> = compiled.diagnostics().iter().map(|d| d.code.id()).collect();
        let (db, files) = session.db();
        let (_, file) = compiled.db_handle().expect("session database");
        let sliced = leek_db::queries::program_diagnostics_upto(
            db,
            files,
            file,
            leek_syntax::pipeline::version_from_byte(file.version_byte(db)),
            leek_db::queries::Stage::Resolved,
        );
        let from_query: Vec<&str> = sliced.iter().map(|d| d.code.id()).collect();

        std::fs::remove_dir_all(&dir).ok();

        assert!(
            !from_run.contains(&"E0202"),
            "the run gave up before resolution: {from_run:?}"
        );
        assert!(
            from_query.contains(&"E0202"),
            "the query resolved anyway: {from_query:?}"
        );
        assert!(
            from_query.len() > from_run.len(),
            "and so reports strictly more: run={from_run:?} query={from_query:?}"
        );
    }

    /// With no stop-on-error threshold the run does not abort, and its
    /// stream matches the query's again — on the same fixture that
    /// diverges under the default params.
    ///
    /// The discriminating half of `a_failed_parse_stops_the_run_but_not_the_queries`.
    /// If the divergence were about the parse failing, it would persist
    /// here; it does not, which places the cause in the `StopOnDiagnostics`
    /// wrapper and nowhere else. It also explains why the LSP never hit
    /// this: `lsp_params` is `RecipeParams::permissive`, so its runs have
    /// always behaved the way the queries do.
    #[test]
    fn a_permissive_run_does_not_stop_and_so_agrees() {
        let dir = scratch("permissive-parse");
        std::fs::write(
            dir.join("src/main.leek"),
            "var a = 1;\nvar a = 2;\nvar bad = \u{a3};\n",
        )
        .expect("entry");
        let project = project_at(dir.clone(), "");
        let config = DriverConfig {
            target: Target::Resolved,
            color: ColorWhen::Never,
            format: MessageFormat::Human,
            params: leek_pipeline::RecipeParams::permissive(),
            ..DriverConfig::default()
        };
        let session = Session::new(&project, config).expect("session");
        let compiled = session.compile_entry().expect("compile");

        let from_run: Vec<&str> = compiled.diagnostics().iter().map(|d| d.code.id()).collect();
        let (db, files) = session.db();
        let (_, file) = compiled.db_handle().expect("session database");
        let sliced = leek_db::queries::program_diagnostics_upto(
            db,
            files,
            file,
            leek_syntax::pipeline::version_from_byte(file.version_byte(db)),
            leek_db::queries::Stage::Resolved,
        );
        let from_query: Vec<&str> = sliced.iter().map(|d| d.code.id()).collect();

        std::fs::remove_dir_all(&dir).ok();

        assert!(
            from_run.contains(&"E0202"),
            "no threshold, so resolution still ran: {from_run:?}"
        );
        assert_eq!(
            from_run, from_query,
            "and the streams agree again once nothing aborts"
        );
    }

    /// A `Run`'s diagnostics are whatever the steps it *ran* reported, so
    /// the stream grows with the target. The tracked program stream has no
    /// such notion: it always reports the whole frontend, and
    /// `program_diagnostics_with_lints` always appends lints.
    ///
    /// This is the constraint on replacing [`Compilation::diagnostics`],
    /// and it is easy to miss because
    /// `the_program_stream_reports_what_the_pipeline_reported` passes:
    /// that test compiles at `Target::Linted`, the one target where the
    /// two agree. Below it they diverge, and in the direction that hurts
    /// — the query reports *more*. Swapping it in unconditionally would
    /// make `miku build` (`Target::Hir`) start emitting lint findings it
    /// has never emitted, and `leekc --emit ast` (`Target::Parsed`) start
    /// reporting type errors, which is a behaviour change wearing a
    /// refactor's clothes.
    ///
    /// So a replacement needs the stream sliced to the target. Until that
    /// exists, this test is the reason `diagnostics()` still comes from
    /// the run.
    #[test]
    fn the_runs_diagnostics_grow_with_the_target() {
        let dir = scratch("target-dependence");
        std::fs::write(
            dir.join("src/main.leek"),
            "var a = 1;\nvar a = 2;\nvar z = 1 / 0;\nreturn a;\n",
        )
        .expect("entry");
        let project = project_at(dir.clone(), "");

        let codes_at = |target| {
            let session = Session::new(&project, quiet(target)).expect("session");
            let compiled = session.compile_entry().expect("compile");
            compiled
                .diagnostics()
                .iter()
                .map(|d| d.code.id())
                .collect::<Vec<_>>()
        };

        let parsed = codes_at(Target::Parsed);
        let typed = codes_at(Target::TypeChecked);
        let linted = codes_at(Target::Linted);

        std::fs::remove_dir_all(&dir).ok();

        // Parsing alone finds nothing here: the redeclaration is the
        // resolver's, the division the linter's.
        assert!(parsed.is_empty(), "nothing before resolution: {parsed:?}");
        assert_eq!(typed, ["E0202"], "the resolver's finding, and no lint");
        assert!(
            linted.len() > typed.len() && linted.starts_with(&["E0202"]),
            "linting adds to it rather than replacing it: {linted:?}"
        );
    }

    /// The tracked whole-program lowering answers exactly what the
    /// planned pipeline put in `HirArtifact`.
    ///
    /// This is the gate on moving [`Compilation::hir`] — and with it
    /// `check`, `run`, `build`, `test` and `leekc` — off the `Run`. Every
    /// one of those reads the HIR and nothing else of the pipeline's, so
    /// once the two agree the switch is mechanical; until they do, it is
    /// a guess. The same check is what made the LSP's accessor rewrites
    /// provably behaviour-preserving rather than hopefully so.
    ///
    /// Compared on the lowered tree itself, not a summary of it: two
    /// lowerings that agree about function names and disagree about a
    /// body would pass any count-based assertion and break every
    /// backend.
    #[test]
    fn the_program_query_lowers_what_the_pipeline_lowered() {
        let dir = scratch("hir-parity");
        std::fs::write(
            dir.join("src/main.leek"),
            "include(\"util\")\nvar x = helper() + 1;\nreturn x;\n",
        )
        .expect("entry");
        std::fs::write(
            dir.join("src/util.leek"),
            "function helper() {\n\tvar a = [1, 2, 3];\n\treturn a[0] * 2;\n}\n",
        )
        .expect("include");

        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Hir)).expect("session");
        let compiled = session.compile_entry().expect("compile");

        let from_pipeline = compiled.hir().expect("the pipeline lowered").clone();
        let (db, files) = session.db();
        let (_, file) = compiled.db_handle().expect("session database");
        let from_query = leek_db::queries::lower_program(
            db,
            files,
            file,
            leek_syntax::pipeline::version_from_byte(file.version_byte(db)),
            leek_pipeline::OptLevel::O0,
        );

        std::fs::remove_dir_all(&dir).ok();

        // Not vacuous: two empty trees are equal, so the fixture has to
        // have lowered something, and specifically something from the
        // *include* — that is the part a per-file query would miss.
        assert!(
            from_pipeline
                .defs
                .iter()
                .any(|def| matches!(def, leek_hir::Def::Function(f) if f.name == "helper")),
            "the closure's function reached the pipeline's HIR: {:?}",
            from_pipeline.defs.len()
        );
        assert_eq!(
            from_pipeline.defs.len(),
            from_query.hir.defs.len(),
            "same number of definitions"
        );
        assert_eq!(
            from_pipeline, *from_query.hir,
            "the query and the pipeline lower the same tree"
        );
    }

    /// The tracked program stream is byte-for-byte what the planned
    /// pipeline reported, lints included and in the same order.
    ///
    /// The other half of the gate on moving [`Compilation`] off the
    /// `Run`. `hir` alone is not enough: a `Run` planned for a late
    /// target has already computed and cloned everything on the way
    /// there, so serving `hir` from a query while `diagnostics` still
    /// came from the run would pay for both. A consumer moves wholly or
    /// not at all, which means both halves have to agree first.
    ///
    /// **Only at this target.** The two streams agree at
    /// `Target::Linted` and nowhere below it — see
    /// `the_runs_diagnostics_grow_with_the_target`, which is the
    /// constraint this test does *not* discharge.
    ///
    /// Order is part of the claim, not incidental. `leek-db` argues its
    /// stream is *indistinguishable* from the pipeline's after
    /// `for_source` filtering rather than identical to it — the include
    /// failures are one memoized block instead of interleaved per file.
    /// This fixture is where that argument is checked against the real
    /// thing rather than reasoned about: an error in the entry, lints in
    /// the entry, and a lint that exists only inside the include.
    #[test]
    fn the_program_stream_reports_what_the_pipeline_reported() {
        let dir = scratch("diagnostic-parity");
        std::fs::write(
            dir.join("src/main.leek"),
            "include(\"util\")\nvar a = 1;\nvar a = 2;\nreturn helper();\n",
        )
        .expect("entry");
        std::fs::write(
            dir.join("src/util.leek"),
            "function helper() {\n\treturn 1 / 0;\n}\n",
        )
        .expect("include");

        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");
        let compiled = session.compile_entry().expect("compile");

        let from_pipeline: Vec<&str> = compiled.diagnostics().iter().map(|d| d.code.id()).collect();
        let (db, files) = session.db();
        let (_, file) = compiled.db_handle().expect("session database");
        let from_query = leek_lint::pipeline::program_diagnostics_with_lints(
            db,
            files,
            file,
            leek_syntax::pipeline::version_from_byte(file.version_byte(db)),
            leek_lint::LintGroups::default(),
        );
        let from_query_codes: Vec<&str> = from_query.iter().map(|d| d.code.id()).collect();

        std::fs::remove_dir_all(&dir).ok();

        // Not vacuous, and not only the entry's: the redeclaration is the
        // entry's, `L0016` exists only inside the include.
        assert!(
            from_pipeline.contains(&"E0202") && from_pipeline.contains(&"L0016"),
            "fixture reports across the closure: {from_pipeline:?}"
        );
        assert_eq!(
            from_pipeline, from_query_codes,
            "the query and the pipeline report the same stream, in the same order"
        );
        assert_eq!(
            compiled.diagnostics().len(),
            from_query.len(),
            "and nothing is dropped or duplicated"
        );
    }

    /// An `include` that escapes the project root still resolves.
    ///
    /// The regression this exists for, introduced when `Compilation` moved
    /// off the run and caught only by going looking: the pipeline resolved
    /// includes through a folder that fell back to `DiskFolder`, while
    /// `resolve_include` resolves against `WorkspaceFiles` and nothing
    /// else. A project whose entry includes `../shared/lib` therefore
    /// started reporting `E0272 IncludeNotFound` for a file that is right
    /// there on disk, and every symbol it provided became undefined.
    ///
    /// Nothing caught it: the corpus is single-file, and no `miku` test
    /// had an include pointing outside the project. It is the same gap
    /// `leek_lsp`'s `resync` closes, which is the uncomfortable part —
    /// the analysis was already written down before the session repeated
    /// it.
    #[test]
    fn an_include_escaping_the_project_still_resolves() {
        let base = scratch("escaping-include");
        let shared = base.join("shared");
        std::fs::create_dir_all(&shared).expect("shared dir");
        std::fs::write(
            base.join("src/main.leek"),
            "include(\"../shared/lib\")\nreturn helper();\n",
        )
        .expect("entry");
        std::fs::write(
            shared.join("lib.leek"),
            "function helper() {\n\treturn 1 / 0;\n}\n",
        )
        .expect("include");

        let project = project_at(base.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");
        let compiled = session.compile_entry().expect("compile");
        let codes: Vec<&str> = compiled.diagnostics().iter().map(|d| d.code.id()).collect();

        std::fs::remove_dir_all(&base).ok();

        assert!(
            !codes.contains(&"E0272"),
            "the include is on disk and resolves: {codes:?}"
        );
        // And its contents are really in the program: the lint fires on a
        // construct that exists only inside the escaping file.
        assert!(
            codes.contains(&"L0016"),
            "the escaping file's own findings reach the stream: {codes:?}"
        );
    }

    /// A caller's `source_id` does not override the session's.
    ///
    /// The regression this exists for: `Session` interns every project
    /// file when it opens, so a file the index knows already has an id.
    /// Building its `Input` from the caller's argument instead gave the
    /// pipeline one id and the queries another — `SourceId(9)` against
    /// `SourceId(2)` for the fixture below — so a diagnostic from one path
    /// named a different file than the same diagnostic from the other.
    ///
    /// It survived the 24-combination matrix because that compared
    /// diagnostic *codes*, and the codes were identical; only the spans
    /// disagreed. The matrix compares spans now.
    ///
    /// The caller's id is still honoured for a file the session does not
    /// know, which is the only case where there is no id to conflict with.
    #[test]
    fn the_sessions_id_wins_over_the_callers_for_a_project_file() {
        let dir = scratch("id-authority");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        std::fs::write(dir.join("src/util.leek"), "return 2;\n").expect("other");
        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");

        let compiled = session
            .compile_file(&dir.join("src/util.leek"), SourceId::new(9).expect("id"))
            .expect("compile");
        let (db, file) = compiled.db_handle().expect("session database");

        assert_eq!(
            compiled.input().source,
            file.source(db),
            "one id, whatever the caller asked for"
        );
        assert_ne!(
            compiled.input().source,
            SourceId::new(9).expect("id"),
            "and it is the session's, not the caller's"
        );

        // A file outside the project index has no session id to conflict
        // with, so the caller's stands.
        let outside = dir.join("loose.leek");
        std::fs::write(&outside, "return 3;\n").expect("loose");
        let loose = session
            .compile_file(&outside, SourceId::new(9).expect("id"))
            .expect("compile");
        assert_eq!(
            loose.input().source,
            SourceId::new(9).expect("id"),
            "the caller's id is honoured where there is nothing to clash with"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two files compiled in one session share one database, and a file
    /// the project index already knows keeps the input registered for it
    /// rather than getting a second one for the same bytes.
    ///
    /// This is the whole point of hanging the database off the session:
    /// per-`Compilation` databases meant a `miku test` over N files built
    /// N of them and shared no memo between any two, so every file
    /// re-parsed the stdlib headers from scratch.
    #[test]
    fn one_database_is_shared_by_every_file_of_a_session() {
        let dir = scratch("shared-db");
        std::fs::write(dir.join("src/main.leek"), "return 1;\n").expect("entry");
        std::fs::write(dir.join("src/other.leek"), "return 2;\n").expect("other");
        let project = project_at(dir.clone(), "");
        let session = Session::new(&project, quiet(Target::Linted)).expect("session");

        let first = session.compile_entry().expect("compile entry");
        let second = session
            .compile_file(&dir.join("src/other.leek"), SourceId::new(9).expect("id"))
            .expect("compile other");

        let (db_a, file_a) = first.db_handle().expect("entry database");
        let (db_b, file_b) = second.db_handle().expect("other database");

        assert!(
            std::ptr::eq(db_a, db_b),
            "both compilations answer out of the session's one database"
        );
        assert!(file_a != file_b, "each file is its own input");
        assert_eq!(&**file_b.text(db_b), "return 2;\n");

        // The index walked both files, so compiling one reuses the input
        // registered for it instead of minting a second.
        let again = session.compile_entry().expect("recompile entry");
        assert!(
            again.db_handle().expect("database").1 == file_a,
            "a project file keeps one input across compilations"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
