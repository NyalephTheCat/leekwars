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
use leek_hir::HirFile;
use leek_hir::pipeline::HirArtifact;
use leek_pipeline::Input;
use leek_recipes::Target;
use leek_span::SourceId;
use leek_span::pragma::LanguageSettings;

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
            Err(e) => {
                return Err(RunOutcome::failed(format!(
                    "cannot read {}: {e}",
                    path.display()
                )));
            }
        };

        let lang = settle_language(&source, &self.config);
        let src_id = SourceId::new(1).expect("source id 1 is non-zero");
        let input = Input {
            source: src_id,
            text: source.clone().into(),
            version_byte: lang.version,
            strict: lang.strict,
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
            version: lang.version,
            strict: lang.strict,
        })
    }
}

/// Settle the debugged program's language settings once, at the `Input`
/// boundary: the launch config's `version` > the file's `@version` pragma >
/// [`DEFAULT_VERSION`]; strict when the file has `@strict` or the launch
/// config asks for it. The same values drive lowering and the backend.
fn settle_language(source: &str, config: &LaunchConfig) -> LanguageSettings {
    LanguageSettings::resolve(source, config.version, DEFAULT_VERSION, config.strict)
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
        let lang = settle_language("// @version:1\n// @strict\n", &config(None, false));
        assert_eq!((lang.version, lang.strict), (1, true));
    }

    #[test]
    fn launch_config_version_overrides_pragma() {
        let lang = settle_language("// @version:1\n", &config(Some(3), true));
        assert_eq!((lang.version, lang.strict), (3, true));
    }

    #[test]
    fn pragma_less_file_defaults_to_latest() {
        let lang = settle_language("return 1\n", &config(None, false));
        assert_eq!((lang.version, lang.strict), (DEFAULT_VERSION, false));
    }
}
