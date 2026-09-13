//! Compilation options for the native backend.

use std::path::PathBuf;

/// Default operation budget for a single CLI run (`miku run`, `miku test`
/// without a `timeout` annotation, `leekc --emit run`): 20M, the in-game
/// `OPERATIONS_LIMIT`.
pub const DEFAULT_OP_BUDGET: u64 = 20_000_000;

/// Cranelift optimization level — the debug/release switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

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
        }
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

/// Outcome of a native compile/run.
#[derive(Debug)]
pub enum NativeError {
    /// A MIR construct the backend doesn't lower yet. The corpus
    /// runner treats this as "skip", so the supported subset still
    /// gets exercised as coverage grows.
    Unsupported(String),
    /// MIR lowering or Cranelift compilation failed.
    Compile(String),
    /// The compiled program trapped at runtime.
    Runtime(String),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeError::Unsupported(m) => write!(f, "unsupported: {m}"),
            NativeError::Compile(m) => write!(f, "compile error: {m}"),
            NativeError::Runtime(m) => write!(f, "runtime error: {m}"),
        }
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
