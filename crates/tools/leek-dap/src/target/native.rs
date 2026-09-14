//! The native (Cranelift) debug target.
//!
//! Compiles the requested source to HIR through the standard recipe
//! pipeline — the *project* pipeline, with `include()` resolved off disk
//! exactly as `miku run` resolves it, so a program split across files
//! debugs the same way it runs — then JIT-compiles and runs it via
//! `leek-backend-native` with [`NativeOptions::debug`] — no optimization,
//! frame pointers kept, DWARF emitted — the configuration meant for
//! stepping.
//!
//! Breakpoints and stepping ride on the per-statement safepoints that
//! build emits (see [`crate::debug`]); [`Compiled::breakpoint_map`] answers
//! which file is which `SourceId` and which lines carry one, so a breakpoint
//! can be verified — or snapped to the next line that does — instead of being
//! reported armed on a line the debuggee never reaches.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_backend_native::NativeOptions;
use leek_diagnostics::Severity;
use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::Input;
use leek_recipes::Target;
use leek_resolver::pipeline::IncludeGraphArtifact;
use leek_span::paths::canonical_or_normalized;
use leek_span::pragma::{LATEST_VERSION, LanguageSettings};
use leek_span::{LineTable, SourceId, Span};

use crate::breakpoints::ProgramMap;
use crate::debug::DebugSource;

use super::{LaunchConfig, RunOutcome};

/// Latest Leekscript language version, used when neither the launch config,
/// the file's `@version` pragma, nor the manifest pins one.
const DEFAULT_VERSION: u8 = LATEST_VERSION;

pub(crate) struct NativeTarget {
    config: LaunchConfig,
}

/// One file the program was compiled from: the entry, or a file it
/// transitively `include()`s.
pub(crate) struct CompiledSource {
    /// The `SourceId` the file's spans carry.
    pub source: SourceId,
    pub path: PathBuf,
    pub text: String,
}

/// A compiled program ready to run (or debug). Owns its HIR via `Arc` so a
/// debug worker thread can hold it for the run's duration.
pub(crate) struct Compiled {
    pub hir: Arc<HirFile>,
    /// Every file the program was compiled from, entry first.
    pub sources: Vec<CompiledSource>,
    pub version: u8,
    pub strict: bool,
}

impl Compiled {
    /// Line tables and paths keyed by raw `SourceId`, so the debug controller
    /// can map a safepoint's `(source, offset)` to a file and line.
    pub(crate) fn debug_sources(&self) -> HashMap<u32, DebugSource> {
        self.sources
            .iter()
            .map(|file| {
                (
                    file.source.get(),
                    DebugSource {
                        path: file.path.display().to_string(),
                        line_table: LineTable::new(&file.text),
                    },
                )
            })
            .collect()
    }

    /// The breakpoint-resolution view of this program: which file is which
    /// `SourceId`, and which of its lines the debuggee can stop on.
    pub(crate) fn breakpoint_map(&self) -> ProgramMap {
        ProgramMap::new(
            self.sources
                .iter()
                .map(|file| (canonical_or_normalized(&file.path), file.source.get()))
                .collect(),
            self.safepoint_lines(),
        )
    }

    /// Lines that carry a debug safepoint, per raw `SourceId`.
    ///
    /// Mirrors what the native backend's debug build emits: one safepoint per
    /// MIR statement, plus one on the terminator line of a real `return` block
    /// so a bare `return x` — which lowers to no statement at all — still
    /// stops. Lowering the HIR a second time costs nothing next to the JIT and
    /// is what keeps breakpoint verification honest: a line with no safepoint
    /// is a line the debuggee cannot stop on, and saying so beats a marker
    /// that sits there armed and never fires.
    ///
    /// Deliberately generous in one spot: the backend skips the terminator
    /// safepoint inside a non-constant default argument's fill blocks, a
    /// translation detail with no MIR-level tell. Counting those lines can
    /// only leave a breakpoint verified that never fires — today's behavior —
    /// never mark a working one broken.
    fn safepoint_lines(&self) -> HashMap<u32, BTreeSet<u32>> {
        // Synthetic spans carry no file, so they simply find no line table.
        let tables: HashMap<u32, LineTable> = self
            .sources
            .iter()
            .map(|file| (file.source.get(), LineTable::new(&file.text)))
            .collect();
        let (program, _) = leek_mir::lower_file(&self.hir);

        let mut lines: HashMap<u32, BTreeSet<u32>> = HashMap::new();
        let mut note = |span: Span| {
            let source = span.source.get();
            if let Some(table) = tables.get(&source) {
                lines
                    .entry(source)
                    .or_default()
                    .insert(table.line_col(span.start).line);
            }
        };
        for function in &program.functions {
            for block in &function.blocks {
                for span in &block.statement_spans {
                    note(*span);
                }
                if matches!(block.terminator, leek_mir::ir::Terminator::Return(_)) {
                    note(block.terminator_span);
                }
            }
        }
        lines
    }
}

impl NativeTarget {
    /// Read + compile the program to HIR. Returns the compiled program or a
    /// failed [`RunOutcome`] carrying the diagnostic.
    pub(crate) fn compile(&self) -> Result<Compiled, RunOutcome> {
        let path = &self.config.program;
        let source = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                return Err(RunOutcome::failed(format!(
                    "cannot read {}: {e}",
                    path.display()
                )));
            }
        };

        let lang = settle_language(&source, &self.config, manifest_defaults(path));
        // The entry keeps id 1 and the include walker hands out 2, 3, … —
        // the same numbering `leek-driver` uses, so spans line up with the
        // rest of the toolchain.
        let src_id = SourceId::new(1).expect("source id 1 is non-zero");
        let input = Input {
            source: src_id,
            text: source.clone().into(),
            version_byte: lang.version,
            strict: lang.strict,
            flags: leek_pipeline::FeatureFlags::from_env(),
        };

        let pipeline = match leek_recipes::pipeline_with_includes(
            Target::Hir,
            leek_driver::includes_step(path, src_id),
            &leek_recipes::driver_params(),
        ) {
            Ok(pipeline) => pipeline,
            Err(e) => return Err(RunOutcome::failed(format!("building pipeline: {e}"))),
        };
        let run = pipeline.run(input);

        let errors: Vec<&str> = run
            .diagnostics()
            .iter()
            .filter(|d| matches!(d.severity, Severity::Error))
            .map(|d| d.message.as_str())
            .collect();
        if !errors.is_empty() {
            return Err(RunOutcome::failed(format!(
                "compilation failed:\n{}",
                errors.join("\n")
            )));
        }

        let Some(hir) = run.get::<HirArtifact>() else {
            return Err(RunOutcome::failed("pipeline produced no HIR"));
        };
        let mut sources = vec![CompiledSource {
            source: src_id,
            path: canonical_or_normalized(path),
            text: source,
        }];
        if let Some(graph) = run.get::<IncludeGraphArtifact>() {
            sources.extend(graph.includes.iter().map(|file| CompiledSource {
                source: file.source,
                path: file.path.clone(),
                text: file.text.clone(),
            }));
        }
        Ok(Compiled {
            hir: hir.0.clone(),
            sources,
            version: lang.version,
            strict: lang.strict,
        })
    }
}

/// The `[project]` language defaults of the nearest `Miku.toml` at or above
/// the debugged file, or [`DEFAULT_VERSION`] / non-strict when the file is
/// standalone. Same source of truth `miku run` settles its inputs from.
fn manifest_defaults(program: &Path) -> (u8, bool) {
    let program = canonical_or_normalized(program);
    let dir = program.parent().unwrap_or_else(|| Path::new("."));
    leek_manifest::discover(dir).map_or((DEFAULT_VERSION, false), |load| {
        (load.manifest.project.language, load.manifest.project.strict)
    })
}

/// Settle the debugged program's language settings once, at the `Input`
/// boundary: the launch config's `version` > the file's `@version` pragma >
/// the manifest's `[project].language` > [`DEFAULT_VERSION`]; strict when the
/// file has `@strict`, the manifest asks for it, or the launch config does.
/// The same values drive lowering and the backend.
fn settle_language(
    source: &str,
    config: &LaunchConfig,
    (default_version, default_strict): (u8, bool),
) -> LanguageSettings {
    LanguageSettings::resolve(
        source,
        config.version,
        default_version,
        default_strict || config.strict,
    )
}

impl NativeTarget {
    pub(crate) fn launch(config: &LaunchConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }
}

/// Run a compiled program with the debug profile (frame pointers + DWARF).
/// `debug_hooks` turns on per-statement safepoints for breakpoint support.
pub(crate) fn run_compiled(program: &Compiled, debug_hooks: bool) -> RunOutcome {
    let opts = NativeOptions::debug()
        .with_lang(program.version, program.strict)
        .with_debug_hooks(debug_hooks);
    match leek_backend_native::run(program.hir.as_ref(), &opts) {
        Ok(value) => RunOutcome {
            output: format!("=> {value:?}\n"),
            exit_code: 0,
        },
        Err(e) => RunOutcome::failed(format!("native execution error: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Language defaults of a file that belongs to no project.
    const STANDALONE: (u8, bool) = (DEFAULT_VERSION, false);

    fn config(version: Option<u8>, strict: bool) -> LaunchConfig {
        serde_json::from_value(serde_json::json!({
            "program": "main.leek",
            "version": version,
            "strict": strict,
        }))
        .expect("launch config")
    }

    #[test]
    fn pragma_selects_version_when_launch_config_does_not() {
        let lang = settle_language(
            "// @version:1\n// @strict\n",
            &config(None, false),
            STANDALONE,
        );
        assert_eq!((lang.version, lang.strict), (1, true));
    }

    #[test]
    fn launch_config_version_overrides_pragma() {
        let lang = settle_language("// @version:1\n", &config(Some(3), true), STANDALONE);
        assert_eq!((lang.version, lang.strict), (3, true));
    }

    #[test]
    fn pragma_less_file_defaults_to_latest() {
        let lang = settle_language("return 1\n", &config(None, false), STANDALONE);
        assert_eq!((lang.version, lang.strict), (DEFAULT_VERSION, false));
    }

    #[test]
    fn manifest_backs_a_pragma_less_file() {
        let lang = settle_language("return 1\n", &config(None, false), (2, true));
        assert_eq!((lang.version, lang.strict), (2, true));
    }

    #[test]
    fn pragma_overrides_the_manifest_default() {
        let lang = settle_language("// @version:3\n", &config(None, false), (2, false));
        assert_eq!(lang.version, 3);
    }

    #[test]
    fn safepoint_lines_cover_a_bare_return_and_skip_a_comment() {
        let dir = std::env::temp_dir().join(format!("leek-dap-safepoints-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let program = dir.join("main.leek");
        // Line 1 is a comment, line 2 a statement, line 3 a bare `return`
        // that lowers to a terminator and no statement at all.
        std::fs::write(&program, "// a comment\nvar a = 1\nreturn a\n").expect("program");

        let compiled = NativeTarget::launch(
            &serde_json::from_value(serde_json::json!({
                "program": program.display().to_string(),
            }))
            .expect("launch config"),
        )
        .compile()
        .unwrap_or_else(|outcome| panic!("compile failed: {}", outcome.output));
        let entry = compiled.sources[0].source.get();
        let map = compiled.breakpoint_map();

        assert_eq!(map.source_of(&program), Some(entry));
        assert_eq!(map.place(entry, 2), Some(2), "the statement line");
        assert_eq!(
            map.place(entry, 3),
            Some(3),
            "a bare `return` line carries a safepoint of its own"
        );
        assert_eq!(
            map.place(entry, 1),
            Some(2),
            "a comment line slides down to the next line with code"
        );
        assert_eq!(map.place(entry, 4), None, "nothing executable follows");

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn manifest_defaults_follow_the_program_out_of_the_cwd() {
        let dir = std::env::temp_dir().join(format!(
            "leek-dap-manifest-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).expect("temp project");
        std::fs::write(
            dir.join("Miku.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\nlanguage = 2\nstrict = true\n",
        )
        .expect("manifest");
        let program = src.join("main.leek");
        std::fs::write(&program, "return 1\n").expect("program");

        assert_eq!(manifest_defaults(&program), (2, true));
        // A file outside any project keeps the built-in defaults.
        assert_eq!(
            manifest_defaults(&std::env::temp_dir().join("no-such-project-file.leek")).0,
            DEFAULT_VERSION
        );

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
