//! Compilation options for the native backend.

use std::path::PathBuf;

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
