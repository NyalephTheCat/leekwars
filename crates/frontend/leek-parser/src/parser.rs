//! Parser driver: token cursor + green-tree builder + error sink.

use leek_diagnostics::{Diagnostic, Severity};
use leek_lexer::lex;
use leek_span::{SourceId, Span};
use leek_syntax::{SyntaxKind, Token, Version};
use rowan::{Checkpoint, GreenNode, GreenNodeBuilder, Language as _};

use leek_syntax::LeekLanguage;

/// Result of a parse: the green tree plus any diagnostics. Diagnostics
/// from both pragma parsing and lexing flow through here too.
#[derive(Debug)]
pub struct ParseResult {
    pub green: GreenNode,
    pub diagnostics: Vec<Diagnostic>,
}

/// **Experimental.** Opt-in grammar relaxations, intended for parsing
/// *signature files* that declare builtins. Off by default; the
/// pipeline reads them from environment variables so they never affect
/// normal code or the corpus baseline.
#[derive(Debug, Clone, Copy, Default)]
pub struct ParseFeatures {
    /// Allow bodiless function declarations — `function name(params) -> T;`
    /// — so a signature can be stated without an implementation.
    pub function_signatures: bool,
    /// Allow generic type parameters — `function f<T>(…)`, `class Box<T>`,
    /// `T m<U>(…)` — declared after a function/class/method name.
    pub generics: bool,
    /// Allow `type Name = T` alias declarations and tuple-shaped array
    /// types (`Array[integer, boolean]`).
    pub types: bool,
    /// Allow `interface Name { … }` declarations and the
    /// `implements I1, I2` clause on classes.
    pub interfaces: bool,
    /// Allow `enum Name { A, B = 10 }` declarations.
    pub enums: bool,
}

impl ParseFeatures {
    /// Read the experimental toggles from the environment. Prefer
    /// [`From<FeatureFlags>`] with flags threaded through the pipeline; this
    /// remains for direct callers (tests, ad-hoc tools) without a flag source.
    pub fn from_env() -> Self {
        leek_span::FeatureFlags::from_env().into()
    }
}

impl From<leek_span::FeatureFlags> for ParseFeatures {
    fn from(f: leek_span::FeatureFlags) -> Self {
        Self {
            function_signatures: f.function_signatures,
            generics: f.generic_syntax,
            types: f.types,
            interfaces: f.interfaces,
            enums: f.enums,
        }
    }
}

/// Public entry: lex `text`, then parse it. The `version` selects
/// keyword gating; in practice you'd derive it from
/// [`parse_pragmas`](leek_syntax::parse_pragmas) first.
///
/// When a caller already has a lexed `Vec<Token>` (e.g. the
/// pipeline's `Lex` step produced one), prefer [`parse_tokens`] to
/// avoid re-lexing.
pub fn parse(text: &str, source: SourceId, version: Version) -> ParseResult {
    parse_with_features(text, source, version, ParseFeatures::from_env())
}

/// Like [`parse`] but with explicit experimental [`ParseFeatures`]
/// (lexes internally, then parses with the given features).
pub fn parse_with_features(
    text: &str,
    source: SourceId,
    version: Version,
    features: ParseFeatures,
) -> ParseResult {
    let lex_result = lex(text, source, version);
    let mut result = parse_tokens_with(text, source, &lex_result.tokens, version, features);
    // `parse_tokens` doesn't know about lex diagnostics; prepend
    // them so callers that go through `parse()` see the same
    // ordering as before.
    let mut diags = lex_result.diagnostics;
    diags.extend(result.diagnostics);
    result.diagnostics = diags;
    result
}

/// Parse a pre-lexed token stream. The caller is responsible for
/// any lex diagnostics — they're not folded in here. Matches
/// [`parse`]'s output otherwise.
pub fn parse_tokens(
    text: &str,
    source: SourceId,
    tokens: &[Token],
    version: Version,
) -> ParseResult {
    parse_tokens_with(text, source, tokens, version, ParseFeatures::from_env())
}

/// Like [`parse_tokens`] but with explicit experimental [`ParseFeatures`]
/// (tests pass these directly rather than via the environment).
pub fn parse_tokens_with(
    text: &str,
    source: SourceId,
    tokens: &[Token],
    version: Version,
    features: ParseFeatures,
) -> ParseResult {
    let mut p = Parser::new(text, source, tokens, version, features);
    crate::grammar::source_file(&mut p);
    ParseResult {
        green: p.builder.finish(),
        diagnostics: p.diagnostics,
    }
}

/// Parser state: a cursor over the lexer's token slice and a green-tree
/// builder.
///
/// Trivia handling: `current` / `nth` skip trivia logically without
/// emitting it. `bump`, `expect`, and `checkpoint` first flush any
/// pending trivia at the current scope, then act. The net effect is
/// that trivia between tokens becomes a child of whichever node was
/// open when the trivia was encountered.
pub(crate) struct Parser<'t> {
    text: &'t str,
    source: SourceId,
    tokens: &'t [Token],
    pos: usize,
    builder: GreenNodeBuilder<'static>,
    diagnostics: Vec<Diagnostic>,
    /// When `false`, `>` is *not* treated as a binary operator. Set
    /// during the body of `<...>` set/map literals so the closing `>`
    /// isn't accidentally consumed as a greater-than comparison.
    pub(crate) gt_is_binary: bool,
    version: Version,
    features: ParseFeatures,
    /// Current recursion depth of the expression/type grammar. Guards
    /// against stack overflow on pathologically nested input (a real DoS
    /// vector for the LSP, which parses untrusted buffers on every
    /// keystroke). See [`Parser::enter_recursion`].
    depth: u32,
}

/// Maximum nesting depth for the recursive expression/type productions
/// before the parser stops descending and emits an error. Chosen well
/// below what the smallest worker stack (≈2 MB on a tokio runtime) can
/// hold so a `]]]]…` / `Array<Array<…>>` / `----…` chain degrades to an
/// error node instead of overflowing the stack (which `catch_unwind`
/// cannot recover). Real code nests far shallower than this.
pub(crate) const MAX_RECURSION_DEPTH: u32 = 256;

impl<'t> Parser<'t> {
    fn new(
        text: &'t str,
        source: SourceId,
        tokens: &'t [Token],
        version: Version,
        features: ParseFeatures,
    ) -> Self {
        Self {
            text,
            source,
            tokens,
            pos: 0,
            builder: GreenNodeBuilder::new(),
            diagnostics: Vec::new(),
            gt_is_binary: true,
            version,
            features,
            depth: 0,
        }
    }

    /// Enter a recursive grammar production. Returns `true` if the depth
    /// budget ([`MAX_RECURSION_DEPTH`]) is now exceeded — callers that get
    /// `true` must emit an error and stop descending instead of recursing.
    /// Always pair with [`Parser::leave_recursion`].
    pub(crate) fn enter_recursion(&mut self) -> bool {
        self.depth += 1;
        self.depth > MAX_RECURSION_DEPTH
    }

    /// Leave a recursive grammar production (decrements the depth counter).
    pub(crate) fn leave_recursion(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    pub(crate) fn version(&self) -> Version {
        self.version
    }

    /// Experimental grammar toggles active for this parse.
    pub(crate) fn features(&self) -> ParseFeatures {
        self.features
    }

    // ---- Cursor (trivia-aware) ----

    /// Kind of the current non-trivia token (`Eof` past the end).
    pub(crate) fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    /// Kind of the n-th upcoming non-trivia token (0-indexed).
    pub(crate) fn nth(&self, n: usize) -> SyntaxKind {
        let mut idx = self.pos;
        let mut remaining = n;
        loop {
            match self.tokens.get(idx) {
                None => return SyntaxKind::Eof,
                Some(t) if t.kind == SyntaxKind::Eof => return SyntaxKind::Eof,
                Some(t) if t.kind.is_trivia() => {
                    idx += 1;
                }
                Some(t) => {
                    if remaining == 0 {
                        return t.kind;
                    }
                    remaining -= 1;
                    idx += 1;
                }
            }
        }
    }

    pub(crate) fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    pub(crate) fn at_any(&self, kinds: &[SyntaxKind]) -> bool {
        kinds.contains(&self.current())
    }

    pub(crate) fn at_eof(&self) -> bool {
        self.current() == SyntaxKind::Eof
    }

    /// Opaque marker for progress-checking. Two calls return equal
    /// values iff the parser has not advanced the cursor between
    /// them; using `SyntaxKind` equality alone is wrong because two
    /// consecutive identical tokens look the same.
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// Flush any pending trivia at the current scope. Public to grammar
    /// so callers can ensure trailing trivia ends up inside a closing
    /// node (e.g. `SourceFile`) rather than after `finish_node`.
    pub(crate) fn finish_trivia(&mut self) {
        self.flush_trivia();
    }

    /// Span of the current non-trivia token, or an empty span at EOF.
    pub(crate) fn current_span(&self) -> Span {
        let mut idx = self.pos;
        while let Some(t) = self.tokens.get(idx) {
            if !t.kind.is_trivia() {
                return t.span;
            }
            idx += 1;
        }
        // EOF — return zero-width span at end of input.
        let end = leek_span::offset(self.text.len());
        Span::new(self.source, end, end)
    }

    /// Source text of the current non-trivia token (empty at EOF).
    #[allow(dead_code)] // exposed for future grammar productions
    pub(crate) fn current_text(&self) -> &'t str {
        self.nth_text(0)
    }

    /// Source text of the n-th upcoming non-trivia token.
    pub(crate) fn nth_text(&self, n: usize) -> &'t str {
        let mut idx = self.pos;
        let mut remaining = n;
        loop {
            match self.tokens.get(idx) {
                None => return "",
                Some(t) if t.kind == SyntaxKind::Eof => return "",
                Some(t) if t.kind.is_trivia() => {
                    idx += 1;
                }
                Some(t) => {
                    if remaining == 0 {
                        return &self.text[t.span.range()];
                    }
                    remaining -= 1;
                    idx += 1;
                }
            }
        }
    }

    // ---- Builder integration ----

    /// Flush any pending trivia at the current scope into the builder.
    /// Called automatically before any structural change.
    fn flush_trivia(&mut self) {
        while let Some(t) = self.tokens.get(self.pos) {
            if t.kind.is_trivia() {
                self.emit_token(*t);
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn emit_token(&mut self, t: Token) {
        if t.kind == SyntaxKind::Eof {
            return;
        }
        let slice = &self.text[t.span.range()];
        self.builder.token(LeekLanguage::kind_to_raw(t.kind), slice);
    }

    /// Consume the current non-trivia token and emit it into the tree.
    /// No-op at EOF.
    pub(crate) fn bump(&mut self) {
        self.flush_trivia();
        if let Some(t) = self.tokens.get(self.pos) {
            if t.kind == SyntaxKind::Eof {
                return;
            }
            self.emit_token(*t);
            self.pos += 1;
        }
    }

    /// Emit a token tagged with a *remap* kind (useful when, e.g.,
    /// the lexer produces `KwAnd` but we want it to appear as `AmpAmp`
    /// in the tree — not used in this slice but here for future use).
    #[allow(dead_code)]
    pub(crate) fn bump_remap(&mut self, kind: SyntaxKind) {
        self.flush_trivia();
        if let Some(t) = self.tokens.get(self.pos) {
            if t.kind == SyntaxKind::Eof {
                return;
            }
            let slice = &self.text[t.span.range()];
            self.builder.token(LeekLanguage::kind_to_raw(kind), slice);
            self.pos += 1;
        }
    }

    /// Bump if current matches, else record an error and skip nothing.
    pub(crate) fn expect(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            self.error(format!("expected {kind:?}, found {:?}", self.current()));
            false
        }
    }

    /// Bump if current matches, returning whether we consumed.
    pub(crate) fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(crate) fn start_node(&mut self, kind: SyntaxKind) {
        // Don't flush trivia here — callers should peek first if they
        // want trivia placed before the node. The exception is
        // top-level node openings (handled in grammar/source_file).
        self.builder.start_node(LeekLanguage::kind_to_raw(kind));
    }

    pub(crate) fn finish_node(&mut self) {
        self.builder.finish_node();
    }

    /// Mark a checkpoint at the current builder position. Flushes
    /// pending trivia first so the checkpoint sits *after* leading
    /// trivia — which is what Pratt-style retroactive wrapping wants.
    pub(crate) fn checkpoint(&mut self) -> Checkpoint {
        self.flush_trivia();
        self.builder.checkpoint()
    }

    pub(crate) fn start_node_at(&mut self, cp: Checkpoint, kind: SyntaxKind) {
        self.builder
            .start_node_at(cp, LeekLanguage::kind_to_raw(kind));
    }

    // ---- Diagnostics ----

    pub(crate) fn error(&mut self, message: impl Into<String>) {
        let span = self.current_span();
        self.diagnostics.push(Diagnostic::new(
            UNEXPECTED_TOKEN,
            Severity::Error,
            span,
            message,
        ));
    }

    /// Consume the current non-trivia token into an `ErrorNode` and
    /// record a diagnostic. Used as the recovery primitive.
    pub(crate) fn err_and_bump(&mut self, message: impl Into<String>) {
        self.start_node(SyntaxKind::ErrorNode);
        self.error(message);
        self.bump();
        self.finish_node();
    }
}

pub(crate) use leek_diagnostics::codes::UNEXPECTED_TOKEN;
