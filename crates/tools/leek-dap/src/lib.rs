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
//! Compilation goes through the same project front-end as `miku run`:
//! `include("…")` is resolved off disk with matching `SourceId`s, and the
//! language version / strict mode are settled from the launch config, then
//! the file's `@version` / `@strict` pragmas, then `Miku.toml`'s
//! `[project]` defaults. Breakpoints and stack frames therefore point at
//! included files by their own paths and lines.
//!
//! The debuggee always runs on a worker thread — including a `noDebug`
//! launch, which is the same run with the instrumentation off — so the
//! request loop keeps answering `terminate`/`disconnect` while a long
//! program (or a whole fight) is running.
//!
//! # What works
//!
//! Working: the full DAP handshake, native execution, line breakpoints,
//! `stopOnEntry`, step in/over/out (depth-aware), multi-frame stack traces,
//! and per-frame local-variable inspection — all driven by per-statement
//! safepoints plus function enter/leave hooks the native backend emits in
//! debug builds (see [`debug::NativeDebugSession`]).
//!
//! Breakpoints are a live model: [`breakpoints::BreakpointStore`] holds what
//! the client asked for, keyed by canonical path, and every `setBreakpoints`
//! pushes the resolved set into a running controller — so a breakpoint set
//! mid-run takes effect on that run. Each is verified against the lines the
//! backend actually emits a safepoint for, moved down to the next such line
//! when the requested one has no code, and reported with an id the `stopped`
//! event names back.
//!
//! Known gap: conditional breakpoints, hit counts and logpoints are not
//! implemented, and the matching capabilities stay off rather than advertise
//! them.

mod breakpoints;
mod capabilities;
mod debug;
mod event;
mod handlers;
mod server;
mod session;
mod target;
#[cfg(test)]
mod testing;

pub use server::run_stdio;
