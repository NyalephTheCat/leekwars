//! The native (Cranelift) debug target.
//!
//! Compiles the requested source to HIR through the standard recipe
//! pipeline, then JIT-compiles and runs it via `leek-backend-native`
//! with [`NativeOptions::debug`] — no optimization, frame pointers
//! kept, DWARF emitted — the configuration meant for stepping.
//!
//! This runs to completion; honoring breakpoints requires driving the
//! JIT with the emitted DWARF (or an interpreter hook), which is the
//! next phase. See the module-level note in [`crate::target`].

use std::sync::Arc;

use leek_backend_native::NativeOptions;
use leek_diagnostics::Severity;
use leek_hir::pipeline::HirArtifact;
use leek_hir::HirFile;
use leek_pipeline::Input;
use leek_recipes::Target;
use leek_span::SourceId;

use super::{LaunchConfig, RunOutcome};

/// Latest Leekscript language version, used when the launch config
/// doesn't pin one.
const DEFAULT_VERSION: u8 = 4;

pub(crate) struct NativeTarget {
    config: LaunchConfig,
}

/// A compiled program ready to run (or debug). Owns its HIR via `Arc` so a
/// debug worker thread can hold it for the run's duration.
pub(crate) struct Compiled {
    pub hir: Arc<HirFile>,
    pub source: String,
    pub version: u8,
    pub strict: bool,
}

impl NativeTarget {
    /// Read + compile the program to HIR. Returns the compiled program or a
    /// failed [`RunOutcome`] carrying the diagnostic.
    pub(crate) fn compile(&self) -> Result<Compiled, RunOutcome> {
        let path = &self.config.program;
        let source = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => return Err(RunOutcome::failed(format!("cannot read {}: {e}", path.display()))),
        };

        let version = self.config.version.unwrap_or(DEFAULT_VERSION);
        let src_id = SourceId::new(1).expect("source id 1 is non-zero");
        let input = Input {
            source: src_id,
            text: source.clone().into(),
            version_byte: version,
            strict: self.config.strict,
            flags: leek_pipeline::FeatureFlags::from_env(),
        };

        let pipeline = match leek_recipes::pipeline(Target::Hir, &leek_recipes::driver_params()) {
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
        Ok(Compiled {
            hir: hir.0.clone(),
            source,
            version,
            strict: self.config.strict,
        })
    }
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
