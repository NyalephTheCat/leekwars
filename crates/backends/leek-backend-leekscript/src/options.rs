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
    /// The `SourceId` of the *user* (entry) file. Only used for comment
    /// attribution: `source_text` holds the entry's text, so comments are
    /// flushed for spans coming from it. It does **not** decide what is
    /// emitted — see [`Options::prelude_sources`].
    pub user_source: SourceId,
    /// Original source text, used to recover comments for pretty mode.
    /// `None` disables comment preservation.
    pub source_text: Option<Arc<str>>,
    /// Skip prelude-origin definitions (stdlib signatures merged into the
    /// HIR). On by default; their calls emit the bare builtin name.
    pub drop_prelude_defs: bool,
    /// `SourceId`s that hold merged library/prelude headers. Origin is
    /// recorded here rather than inferred from `user_source`, so defs from
    /// *included* files — which carry their own ids — are emitted like the
    /// entry's own. Defaults to [`leek_prelude::source_id()`], the id every
    /// in-tree pipeline gives the merged header.
    pub prelude_sources: Vec<SourceId>,
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
            prelude_sources: vec![leek_prelude::source_id()],
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

    /// Override the set of library/prelude source ids (for embedders that
    /// merge a header under an id of their own).
    #[must_use]
    pub fn with_prelude_sources(mut self, ids: impl IntoIterator<Item = SourceId>) -> Self {
        self.prelude_sources = ids.into_iter().collect();
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
