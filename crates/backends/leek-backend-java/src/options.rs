//! Emission options for the Java backend.
//!
//! Two modes — `Exact` mirrors the Java reference's emit shape
//! (`u_x` mangling, per-statement `ai.ops(1)`, runtime-comparison
//! switch lowering, etc.); `Clean` opts into the liberalizations
//! documented in `doc/java-backend.md` §2.2.

use std::sync::Arc;

use leek_environment::EnvironmentCatalog;
use leek_span::LineTable;
use leek_syntax::Version;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Faithful to the Java reference: identical mangling, lowering,
    /// and lexical shape (subject to the structural fidelity of this
    /// port — byte parity is a separate parity-harness milestone).
    Exact,
    /// Readable Java. Drops unnecessary mangling, folds per-block
    /// `charge(n)` calls, drops dead code after `return`, prefers
    /// native `switch` when the cases allow it.
    Clean,
}

/// Tunables for one emission run.
#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,

    /// Language version (drives v1 string-escape quirk, `g_x.get()` boxing,
    /// `legacyArrayClass` aliasing, etc.).
    pub version: Version,

    /// Stable id baked into the class name: `public class AI_<id> extends AI`.
    pub ai_id: u64,

    /// Emit per-statement `ai.ops(1)` ticks. Exact mode defaults true,
    /// clean mode false (and instead routes through `--with-charge`).
    pub emit_ops: bool,

    /// In clean mode, fold static op cost per block into a single
    /// `charge(n)` at the block's entry. No-op in exact mode.
    pub with_charge: bool,

    /// Drop unreachable statements after a definite `return` /
    /// `break` / `continue`. Clean mode only.
    pub dead_code_elim: bool,

    /// Use native Java `switch` when every case label is a constant.
    /// Clean mode only.
    pub native_switch: bool,

    /// Emit `LineMapping`-compatible `<javaLine> <fileIndex> <leekLine>`
    /// sidecar so JVM stack traces round-trip to Leek source.
    pub emit_lines: bool,

    /// Path string baked into `getAIString()` / `getErrorFiles()`.
    /// The reference uses the source file's absolute path; tests can
    /// thread a stable basename for reproducible captures.
    pub source_path: String,

    /// Optional line table for `.lines` sidecar emission (`add_line_at`).
    pub line_table: Option<LineTable>,

    /// Host-environment builtin catalog (combat/game functions like
    /// `getCell`, `moveToward`). When set, a call to one of its functions
    /// emits the generator-compatible dispatch (`EntityClass.getCell(ai,
    /// …)`) plus its `import`. `None` = language builtins only (a call to
    /// an unknown name falls back to a bare `name(...)` as before).
    pub environment: Option<Arc<dyn EnvironmentCatalog>>,

    /// Java base class the generated AI extends. Defaults to `"AI"` (the
    /// LeekScript runner base). For leek-wars-generator *fight* AIs set it
    /// to `"EntityAI"` (matching the generator's
    /// `LeekScript.compileFileContext(…, "…fight.entity.EntityAI", …)`), so
    /// the emitted class can be compiled and run inside a fight.
    pub base_class: String,
}

impl Options {
    pub fn exact(version: Version, ai_id: u64) -> Self {
        Self {
            mode: Mode::Exact,
            version,
            ai_id,
            emit_ops: true,
            with_charge: false,
            dead_code_elim: false,
            native_switch: false,
            emit_lines: true,
            source_path: String::new(),
            line_table: None,
            environment: None,
            base_class: "AI".to_string(),
        }
    }

    pub fn clean(version: Version, ai_id: u64) -> Self {
        Self {
            mode: Mode::Clean,
            version,
            ai_id,
            emit_ops: false,
            with_charge: true,
            dead_code_elim: true,
            native_switch: true,
            emit_lines: true,
            source_path: String::new(),
            line_table: None,
            environment: None,
            base_class: "AI".to_string(),
        }
    }

    /// Attach a line table built from the Leek source text.
    pub fn with_line_table(mut self, source: &str) -> Self {
        self.line_table = Some(LineTable::new(source));
        self
    }

    /// Builder: set the source-path baked into the AI metadata
    /// emitters. Chainable for ergonomics in tests.
    pub fn with_source_path(mut self, path: impl Into<String>) -> Self {
        self.source_path = path.into();
        self
    }

    /// Builder: attach a host-environment builtin catalog so combat/game
    /// functions (`getCell`, `moveToward`, …) emit generator-compatible
    /// dispatch. Pass `Arc::new(leek_environment::LeekWarsCatalog)` for the
    /// official generator.
    pub fn with_environment(mut self, env: Arc<dyn EnvironmentCatalog>) -> Self {
        self.environment = Some(env);
        self
    }

    /// Builder: set the Java base class the generated AI extends (default
    /// `"AI"`; use `"EntityAI"` for leek-wars-generator fight AIs).
    pub fn with_base_class(mut self, base: impl Into<String>) -> Self {
        self.base_class = base.into();
        self
    }

    pub fn is_clean(&self) -> bool {
        matches!(self.mode, Mode::Clean)
    }

    pub fn class_name(&self) -> String {
        format!("AI_{}", self.ai_id)
    }

    pub fn version_byte(&self) -> u8 {
        match self.version {
            Version::V1 => 1,
            Version::V2 => 2,
            Version::V3 => 3,
            Version::V4 => 4,
        }
    }
}
