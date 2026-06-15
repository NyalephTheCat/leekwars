//! Emission options for the LeekScript source backend.

use std::sync::Arc;

use leek_span::SourceId;
use leek_syntax::Version;

/// Output shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Human-readable: indentation, newlines, and (when source text is
    /// available) the original comments carried over.
    Pretty,
    /// Minified: no indentation/newlines beyond what the grammar
    /// requires, and comments dropped.
    Compact,
}

/// Configuration for [`emit`](crate::emit).
#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    /// Run the HIR→HIR optimization passes (constant folding, dead-code
    /// elimination) before emitting.
    pub optimize: bool,
    /// Language version the output targets (drives a few literal quirks).
    pub version: Version,
    /// The `SourceId` of the *user* file. Defs whose span comes from a
    /// different (non-synthetic) source are treated as prelude-origin.
    pub user_source: SourceId,
    /// Original source text, used to recover comments for pretty mode.
    /// `None` disables comment preservation.
    pub source_text: Option<Arc<str>>,
    /// Skip prelude-origin definitions (stdlib signatures merged into the
    /// HIR). On by default; their calls emit the bare builtin name.
    pub drop_prelude_defs: bool,
    /// Indentation unit for pretty mode (ignored in compact mode).
    pub indent: String,
}

impl Options {
    fn base(mode: Mode, version: Version) -> Self {
        Self {
            mode,
            optimize: false,
            version,
            user_source: SourceId::new(1).expect("source id 1 is valid"),
            source_text: None,
            drop_prelude_defs: true,
            indent: "\t".to_string(),
        }
    }

    /// Human-readable output.
    #[must_use]
    pub fn pretty(version: Version) -> Self {
        Self::base(Mode::Pretty, version)
    }

    /// Minified output.
    #[must_use]
    pub fn compact(version: Version) -> Self {
        Self::base(Mode::Compact, version)
    }

    #[must_use]
    pub fn with_optimize(mut self, on: bool) -> Self {
        self.optimize = on;
        self
    }

    #[must_use]
    pub fn with_source_text(mut self, text: impl Into<Arc<str>>) -> Self {
        self.source_text = Some(text.into());
        self
    }

    #[must_use]
    pub fn with_user_source(mut self, source: SourceId) -> Self {
        self.user_source = source;
        self
    }

    #[must_use]
    pub fn with_indent(mut self, indent: impl Into<String>) -> Self {
        self.indent = indent.into();
        self
    }

    #[must_use]
    pub fn keep_prelude_defs(mut self) -> Self {
        self.drop_prelude_defs = false;
        self
    }

    pub(crate) fn is_compact(&self) -> bool {
        self.mode == Mode::Compact
    }
}
