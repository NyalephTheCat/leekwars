//! Debug Adapter Protocol server for Leekscript.
//!
//! Mirrors the shape of `leek-lsp`: a thin stdio launcher
//! (`bins/leek-dap`) calls [`run_stdio`], which speaks DAP over
//! Content-Length-framed stdio using the [`dap`] crate's message
//! types. Per-request handling lives in [`handlers`], split by area
//! the same way the LSP splits its `handlers/` module.
//!
//! # Debug target
//!
//! The debuggee is the **native (Cranelift) backend**. A `launch`
//! request names a `.leek` program; at `configurationDone` the
//! adapter compiles it with [`leek_backend_native::NativeOptions::debug`]
//! (no optimization, frame pointers kept, DWARF emitted) and runs it.
//!
//! # Status: skeleton
//!
//! Working: the full DAP handshake, native execution, line breakpoints,
//! `stopOnEntry`, step in/over/out (depth-aware), multi-frame stack traces,
//! and per-frame local-variable inspection — all driven by per-statement
//! safepoints plus function enter/leave hooks the native backend emits in
//! debug builds (see [`debug::NativeDebugSession`]).
//!
//! Known gap: a breakpoint on a line that lowers to only a terminator (a
//! bare `return x`) won't fire, since safepoints are per-statement; a line
//! with any computation does.

mod capabilities;
mod debug;
mod event;
mod handlers;
mod server;
mod session;
mod target;

pub use server::run_stdio;
