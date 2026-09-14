//! Compilation options for the native backend.

use std::path::PathBuf;

use leek_diagnostics::{Diagnostic, IntoDiagnostic, codes};
use leek_span::Span;

/// Default operation budget for a single CLI run (`miku run`, `miku test`
/// without a `timeout` annotation, `leekc --emit run`): 20M, the in-game
/// `OPERATIONS_LIMIT`.
pub const DEFAULT_OP_BUDGET: u64 = 20_000_000;

/// Cranelift optimization level — the debug/release switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptLevel {
    /// No optimization. Fastest compiles, best for debugging (maps
    /// closely to the source MIR). The "debug" profile.
    None,
    /// Optimize for speed. The "release" profile.
    Speed,
    /// Optimize for speed *and* small code size.
    SpeedAndSize,
}

impl OptLevel {
    /// The string Cranelift's `opt_level` setting expects.
    pub fn cranelift_str(self) -> &'static str {
        match self {
            OptLevel::None => "none",
            OptLevel::Speed => "speed",
            OptLevel::SpeedAndSize => "speed_and_size",
        }
    }
}

/// What the backend should produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEmit {
    /// JIT-compile and run in-process, returning the program's value.
    Jit,
    /// Emit the Cranelift IR (CLIF) text — the primary tool for
    /// inspecting/debugging what the backend generates.
    Clif,
    /// Emit the target disassembly of the compiled code.
    Disasm,
    /// Emit a relocatable object file (`.o`) to the given path.
    Object(PathBuf),
}

/// All knobs for a native compile. Construct via [`NativeOptions::debug`]
/// / [`NativeOptions::release`] and tweak, or build directly.
#[derive(Debug, Clone)]
pub struct NativeOptions {
    /// Optimization level (debug vs release).
    pub opt_level: OptLevel,
    /// Emit DWARF debug info (object output) so a debugger can map
    /// machine code back to functions. Honored for [`NativeEmit::Object`].
    pub debug_info: bool,
    /// Run Cranelift's IR verifier before lowering. On by default in
    /// debug builds; catches malformed IR early at a small compile cost.
    pub enable_verifier: bool,
    /// Keep frame pointers so native debuggers / profilers can unwind
    /// the stack. Recommended when `debug_info` is on.
    pub preserve_frame_pointers: bool,
    /// What to produce.
    pub emit: NativeEmit,
    /// Leekscript language version (1–4). Drives version-specific
    /// semantics the backend must honor — e.g. v1 real division by zero
    /// yields `null` (unrepresentable in the scalar subset → skipped),
    /// while v2+ follows IEEE (`±∞`).
    pub version: u8,
    /// Strict typing. In strict mode an untyped `var x = <int>` slot
    /// coerces every later write to the inferred type (so `var a = 10; a
    /// += 0.5` stays `10`), matching the interpreter's `write_place`.
    pub strict: bool,
    /// Emit a `leek_dbg_safepoint(offset)` call before every statement so
    /// a debugger can pause at source lines. Off by default (adds a call
    /// per statement); the debug adapter turns it on. See [`crate::debug`].
    pub debug_hooks: bool,
    /// Link the host game library: route otherwise-unknown builtins (the
    /// leek-wars fight functions like `getCell`) to the installed
    /// [`crate::game::GameRuntime`] instead of failing with `Unsupported`.
    /// See [`crate::game`].
    pub link_game: bool,
    /// Operation budget. The JIT'd code charges ops at the same MIR sites the
    /// interpreter does; exceeding this records `TOO_MUCH_OPERATIONS` (and loop
    /// back-edges poll the budget to stop). `u64::MAX` ≈ unlimited — use a high
    /// value to verify op *counts* (program must finish) and a low one to make
    /// a runaway loop fault. Read the charged total with
    /// [`crate::ops_used`] after [`crate::run`].
    pub op_limit: u64,
    /// Names of top-level zero-arg functions to force-compile as roots even
    /// when nothing in `main` references them, and register so a
    /// [`Function::User`](leek_runtime::value::Function::User) value can invoke
    /// them through [`crate::run_call`]. This is how the fight generator runs
    /// the `beforeFight()` / `afterFight()` lifecycle hooks: they're never
    /// called from the AI body, so reachability would otherwise prune them.
    pub hook_roots: Vec<String>,
    /// Maximum number of nested user-function calls. Entering a frame beyond
    /// it records `STACKOVERFLOW` (upstream's `Error.STACKOVERFLOW`) and the
    /// program winds down, instead of recursing until the native stack
    /// overflows and the OS kills the host process. Defaults to
    /// [`DEFAULT_MAX_CALL_DEPTH`]; lower it to exercise the error in tests.
    pub max_call_depth: u32,
    /// Native stack the program's user frames may use, in bytes, measured
    /// from where the run starts. Entering a frame below that also records
    /// `STACKOVERFLOW`. A backstop for [`max_call_depth`](Self::max_call_depth):
    /// recursion through a function value crosses the Rust dispatch shims and
    /// costs ~100x the stack of a direct call, so no single depth fits every
    /// call path. `usize::MAX` disables it. Defaults to
    /// [`DEFAULT_MAX_STACK_BYTES`]; the calling thread must have at least this
    /// much stack free (plus slack for the runtime) when the run starts.
    pub max_stack_bytes: usize,
}

/// Default [`NativeOptions::max_call_depth`].
///
/// Upstream has no explicit counter: the JVM throws `StackOverflowError`
/// when the thread stack runs out, so its depth depends on the stack size.
/// This default admits every recursion the upstream corpus runs to completion
/// (the deepest is `rec(1000)`; `rec(10000)` is only run under an op budget it
/// exhausts first).
pub const DEFAULT_MAX_CALL_DEPTH: u32 = 5_000;

/// Default [`NativeOptions::max_stack_bytes`]: 1 MiB, half the 2 MiB stack a
/// spawned Rust thread (test, DAP or fight worker) gets by default.
pub const DEFAULT_MAX_STACK_BYTES: usize = 1 << 20;

impl Default for NativeOptions {
    fn default() -> Self {
        Self::debug()
    }
}

impl NativeOptions {
    /// Debug profile: no optimization, verifier on, frame pointers
    /// kept, debug info on. Best for stepping through generated code.
    pub fn debug() -> Self {
        Self {
            opt_level: OptLevel::None,
            debug_info: true,
            enable_verifier: true,
            preserve_frame_pointers: true,
            emit: NativeEmit::Jit,
            version: 4,
            strict: false,
            debug_hooks: false,
            link_game: false,
            op_limit: u64::MAX,
            hook_roots: Vec::new(),
            max_call_depth: DEFAULT_MAX_CALL_DEPTH,
            max_stack_bytes: DEFAULT_MAX_STACK_BYTES,
        }
    }

    /// Release profile: optimize for speed, verifier off, no debug info.
    pub fn release() -> Self {
        Self {
            opt_level: OptLevel::Speed,
            debug_info: false,
            enable_verifier: false,
            preserve_frame_pointers: false,
            emit: NativeEmit::Jit,
            version: 4,
            strict: false,
            debug_hooks: false,
            link_game: false,
            op_limit: u64::MAX,
            hook_roots: Vec::new(),
            max_call_depth: DEFAULT_MAX_CALL_DEPTH,
            max_stack_bytes: DEFAULT_MAX_STACK_BYTES,
        }
    }

    /// Set the call-depth limit (see [`max_call_depth`](Self::max_call_depth)).
    pub fn with_max_call_depth(mut self, depth: u32) -> Self {
        self.max_call_depth = depth;
        self
    }

    /// Set the native stack budget (see [`max_stack_bytes`](Self::max_stack_bytes)).
    pub fn with_max_stack_bytes(mut self, bytes: usize) -> Self {
        self.max_stack_bytes = bytes;
        self
    }

    /// Enable per-statement debug safepoints (see [`NativeOptions::debug_hooks`]).
    pub fn with_debug_hooks(mut self, on: bool) -> Self {
        self.debug_hooks = on;
        self
    }

    /// Link the host game library (see [`NativeOptions::link_game`]).
    pub fn with_link_game(mut self, on: bool) -> Self {
        self.link_game = on;
        self
    }

    pub fn with_emit(mut self, emit: NativeEmit) -> Self {
        self.emit = emit;
        self
    }

    /// Debug-profile JIT options for running a pipeline result: always at
    /// the input's settled language version **and** strict mode (so `miku
    /// run`, `miku test` and `leekc --emit run` can't drift from each other or
    /// from `miku build --backend native`), with the given op budget.
    pub fn jit_for_input(input: &leek_pipeline::Input, op_limit: u64) -> Self {
        Self::debug()
            .with_emit(NativeEmit::Jit)
            .with_lang(input.version_byte, input.strict)
            .with_op_limit(op_limit)
    }

    /// Set the language semantics (version + strict typing) the compiled
    /// code should honor.
    pub fn with_lang(mut self, version: u8, strict: bool) -> Self {
        self.version = version;
        self.strict = strict;
        self
    }

    /// Set the operation budget (see [`op_limit`](Self::op_limit)).
    pub fn with_op_limit(mut self, limit: u64) -> Self {
        self.op_limit = limit;
        self
    }

    /// Force the named top-level zero-arg functions to compile as roots and
    /// register for indirect invocation (see [`hook_roots`](Self::hook_roots)).
    pub fn with_hook_roots(mut self, roots: Vec<String>) -> Self {
        self.hook_roots = roots;
        self
    }
}

/// The subset of [`NativeOptions`] that changes the *generated code*, so two
/// runs whose keys are equal can share one compiled module.
///
/// Everything left out is applied at run time, after codegen, and therefore
/// costs nothing to vary per run: `op_limit` (armed by `reset_ops`, and polled
/// by a back-edge check that is emitted unconditionally), `max_call_depth` /
/// `max_stack_bytes` (armed by `arm_call_guard`), `debug_info` (object emit
/// only) and `emit` itself (only JIT modules are ever cached). That is what
/// lets a fight's hook runs, which add `HOOK_OPS_BONUS` to the budget, reuse a
/// module built for the same roots.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodegenKey {
    opt_level: OptLevel,
    enable_verifier: bool,
    preserve_frame_pointers: bool,
    version: u8,
    strict: bool,
    debug_hooks: bool,
    link_game: bool,
    hook_roots: Vec<String>,
}

impl NativeOptions {
    /// This options set's [`CodegenKey`] — its identity as far as codegen is
    /// concerned.
    #[must_use]
    pub fn codegen_key(&self) -> CodegenKey {
        CodegenKey {
            opt_level: self.opt_level,
            enable_verifier: self.enable_verifier,
            preserve_frame_pointers: self.preserve_frame_pointers,
            version: self.version,
            strict: self.strict,
            debug_hooks: self.debug_hooks,
            link_game: self.link_game,
            hook_roots: self.hook_roots.clone(),
        }
    }
}

/// What kind of native failure this is. The discriminant, not the message,
/// is what callers should branch on — `miku test` matching an expected
/// runtime error, the corpus runner bucketing a skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeErrorKind {
    /// A MIR construct the backend doesn't lower yet. The corpus
    /// runner treats this as "skip", so the supported subset still
    /// gets exercised as coverage grows.
    Unsupported,
    /// MIR lowering or Cranelift compilation failed.
    Compile,
    /// The compiled program trapped at runtime.
    ///
    /// This is the one kind with no source location and no catalog code, and
    /// that is not an omission: the trap happens inside generated code, and
    /// the value it reports is a *fight* error key (`TOO_MUCH_OPERATIONS`,
    /// `ARRAY_OUT_OF_BOUND`, …) that the generator logs verbatim. Its
    /// `message` is therefore exactly that key, unprefixed and with nothing
    /// appended.
    Runtime,
}

impl NativeErrorKind {
    /// The `Display` prefix — kept byte-identical to the three strings this
    /// error printed before it carried a location, because downstream code
    /// has bucketed on them.
    fn prefix(self) -> &'static str {
        match self {
            NativeErrorKind::Unsupported => "unsupported",
            NativeErrorKind::Compile => "compile error",
            NativeErrorKind::Runtime => "runtime error",
        }
    }
}

/// Outcome of a native compile/run.
///
/// Carries where it happened, not just what happened: the statement span the
/// translator was on, the function that statement belongs to, and — for a MIR
/// lowering failure — every `Diagnostic` the lowerer produced, so `miku` can
/// render the same caret-under-the-construct output a frontend error gets
/// instead of a bare line of text.
///
/// `Clone` because a compiled module is cached for the length of a fight (see
/// [`crate::CompiledProgram`]): an AI that fails to compile must still report
/// that same failure on *every* turn, as it did back when each turn recompiled
/// it, so the cached `Err` is handed out by clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeError {
    pub kind: NativeErrorKind,
    /// The bare reason, with no kind prefix and no location appended. For
    /// [`NativeErrorKind::Runtime`] this is the fight error key itself.
    pub message: String,
    /// Where in the source the failure is, when the backend knows.
    pub span: Option<Span>,
    /// The user function being translated, when the backend knows.
    pub function: Option<String>,
    /// For a lowering failure, every diagnostic the lowerer produced —
    /// not just the first, and each with its own span and catalog code.
    pub diagnostics: Vec<Diagnostic>,
}

impl NativeError {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(NativeErrorKind::Unsupported, message)
    }

    pub fn compile(message: impl Into<String>) -> Self {
        Self::new(NativeErrorKind::Compile, message)
    }

    /// A runtime trap. `code` is the bare fight error key; nothing is
    /// prepended or appended to it, because the generator logs it verbatim.
    pub fn runtime(code: impl Into<String>) -> Self {
        Self::new(NativeErrorKind::Runtime, code)
    }

    fn new(kind: NativeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            span: None,
            function: None,
            diagnostics: Vec::new(),
        }
    }

    /// The bare reason. Use this instead of parsing [`Display`](std::fmt::Display).
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.message
    }

    /// The fight error key, when this is a runtime trap — `None` otherwise.
    /// The key is the whole message, so a caller comparing against
    /// `"TOO_MUCH_OPERATIONS"` never has to strip a prefix.
    #[must_use]
    pub fn runtime_code(&self) -> Option<&str> {
        (self.kind == NativeErrorKind::Runtime).then_some(&*self.message)
    }

    /// Whether this is the "outside the native subset" outcome the corpus
    /// runner treats as a skip.
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        self.kind == NativeErrorKind::Unsupported
    }

    /// Pin the error to `span`, replacing any span already set.
    #[must_use]
    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Pin the error to `span` only if it has none — how an outer frame adds
    /// function-level attribution without overwriting a statement-level span.
    #[must_use]
    pub fn or_span(mut self, span: Span) -> Self {
        self.span.get_or_insert(span);
        self
    }

    /// Name the function being translated, if not already named.
    #[must_use]
    pub fn in_fn(mut self, name: &str) -> Self {
        if self.function.is_none() {
            self.function = Some(name.to_string());
        }
        self
    }

    /// Attach the lowerer's own diagnostics.
    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Every diagnostic this error should render as: the carried lowering
    /// diagnostics when there are any (each with its own code and span),
    /// otherwise one built from the error itself.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        if self.diagnostics.is_empty() {
            vec![self.clone().into_diagnostic()]
        } else {
            self.diagnostics.clone()
        }
    }
}

impl IntoDiagnostic for NativeError {
    fn into_diagnostic(self) -> Diagnostic {
        let code = match self.kind {
            NativeErrorKind::Unsupported => codes::NATIVE_UNSUPPORTED,
            // A runtime trap has no source location; it still needs *a* code
            // to render, and "the native backend failed" is the truthful one.
            NativeErrorKind::Compile | NativeErrorKind::Runtime => codes::NATIVE_COMPILE_FAILED,
        };
        let message = match self.kind {
            NativeErrorKind::Unsupported => {
                format!("the native backend does not support {}", self.message)
            }
            NativeErrorKind::Compile => format!("native compilation failed: {}", self.message),
            NativeErrorKind::Runtime => format!("the program trapped: {}", self.message),
        };
        let mut diag = Diagnostic::error(code, self.span.unwrap_or_else(Span::synthetic), message);
        if let Some(function) = self.function {
            diag = diag.with_note(format!("while compiling `{function}`"));
        }
        diag
    }
}

impl std::fmt::Display for NativeError {
    /// Byte-identical to what this error printed before it carried a
    /// location: `"unsupported: {message}"`, `"compile error: {message}"`,
    /// `"runtime error: {message}"`.
    ///
    /// The location deliberately stays out of it. This string is a fight-log
    /// parameter (`official::log_ai_error`) and a corpus skip bucket, both of
    /// which compare it; the location belongs in the rendered
    /// [`Diagnostic`](IntoDiagnostic::into_diagnostic), which is what a human
    /// reads.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.prefix(), self.message)
    }
}

impl std::error::Error for NativeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jit_for_input_carries_version_and_strict() {
        // Regression: `miku run` / `miku test` built `NativeOptions::debug()`
        // and set only the version, so a `@strict` file (or `strict = true`
        // manifest) ran non-strict under the JIT.
        let input = leek_pipeline::Input {
            source: leek_span::SourceId::new(1).unwrap(),
            text: "".into(),
            version_byte: 2,
            strict: true,
            flags: leek_pipeline::FeatureFlags::none(),
        };
        let opts = NativeOptions::jit_for_input(&input, 1234);
        assert_eq!(opts.version, 2);
        assert!(opts.strict);
        assert_eq!(opts.op_limit, 1234);
        assert_eq!(opts.emit, NativeEmit::Jit);
    }
}
