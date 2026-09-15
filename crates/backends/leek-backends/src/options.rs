//! The option assembly both drivers share.
//!
//! `leekc` and `miku build` emit through the same two source backends, and
//! each wrote the same builder chain out by hand: pick a mode, set the
//! version, hand the backend the user's source id and text. Written twice,
//! the two could disagree about any of it — and about the optimization
//! level they did, which is what [`opt_level`](crate::opt_level) is for.
//!
//! What stays at the call sites is what genuinely differs: where the
//! output goes, and the flags only one driver has (`--ai-id`,
//! `--base-class`, the manifest's `emit_lines`).

use std::sync::Arc;

use leek_environment::EnvironmentCatalog;
use leek_project::Input;
use leek_syntax::Version;

/// The Java emitter's options for `input`.
///
/// `clean` picks the emission mode — see
/// [`java_clean_mode`](crate::java_clean_mode) for where a driver gets
/// it. `source_path` is the label the emitter records for the program it
/// translated, and `environment` the host library catalog that decides
/// how a game builtin is dispatched.
///
/// The version comes off `input` rather than from a parameter: a driver
/// settles it once at the input boundary, and a backend emitting under a
/// version the compilation did not use is a program that means something
/// else.
#[must_use]
pub fn java_options(
    input: &Input,
    ai_id: u64,
    clean: bool,
    source_path: impl Into<String>,
    environment: Option<&Arc<dyn EnvironmentCatalog>>,
) -> leek_backend_java::Options {
    let version = Version::from_byte(input.version_byte);
    let mut opts = if clean {
        leek_backend_java::Options::clean(version, ai_id)
    } else {
        leek_backend_java::Options::exact(version, ai_id)
    }
    .with_source_path(source_path);
    if let Some(env) = environment {
        opts = opts.with_environment(Arc::clone(env));
    }
    opts
}

/// The LeekScript source backend's options for `input`.
///
/// `compact` minifies; the pretty mode keeps the entry's own text so the
/// emitter can carry comments and literal spellings across. `optimize`
/// runs this backend's own semantics-preserving passes — it is the
/// `--optimize` flag, not the compilation's
/// [`OptLevel`](leek_query::OptLevel), which stays `O0` here so the
/// passes see the code as written.
///
/// The user source id and the text both come off `input`, because they
/// have to describe the same file: the emitter uses the id to tell the
/// author's declarations from the prelude's, and the text to render them.
#[must_use]
pub fn leekscript_options(
    input: &Input,
    compact: bool,
    optimize: bool,
) -> leek_backend_leekscript::Options {
    let version = Version::from_byte(input.version_byte);
    let opts = if compact {
        leek_backend_leekscript::Options::compact(version)
    } else {
        leek_backend_leekscript::Options::pretty(version).with_source_text(Arc::clone(&input.text))
    };
    opts.with_optimize(optimize).with_user_source(input.source)
}

#[cfg(test)]
mod tests {
    use super::{java_options, leekscript_options};
    use leek_project::Input;
    use leek_span::{FeatureFlags, SourceId};

    fn input(version_byte: u8) -> Input {
        Input {
            source: SourceId::new(7).unwrap(),
            text: "return 1\n".into(),
            version_byte,
            strict: false,
            flags: FeatureFlags::none(),
        }
    }

    /// The version is the compilation's, not a parameter a caller can get
    /// wrong: a backend emitting under a version the compilation did not
    /// use is a program that means something else.
    #[test]
    fn both_backends_emit_under_the_inputs_version() {
        for byte in [1u8, 4] {
            let expected = leek_syntax::Version::from_byte(byte);
            assert_eq!(
                java_options(&input(byte), 0, false, "x", None).version,
                expected
            );
            assert_eq!(
                leekscript_options(&input(byte), false, false).version,
                expected
            );
        }
    }

    /// Pretty mode carries the author's own text across; compact has
    /// nothing to carry.
    #[test]
    fn only_pretty_leekscript_keeps_the_source_text() {
        assert!(
            leekscript_options(&input(4), false, false)
                .source_text
                .is_some()
        );
        assert!(
            leekscript_options(&input(4), true, false)
                .source_text
                .is_none()
        );
    }

    /// The emitter tells the author's declarations from the prelude's by
    /// source id, so it has to be the entry's own.
    #[test]
    fn the_user_source_is_the_inputs_own() {
        let input = input(4);
        assert_eq!(
            leekscript_options(&input, false, false).user_source,
            input.source
        );
    }
}
