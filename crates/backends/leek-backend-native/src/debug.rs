//! Debug hook seam for the native backend.
//!
//! When a program is compiled with [`crate::NativeOptions::debug_hooks`],
//! the generated code calls `leek_dbg_safepoint(offset, desc, values)`
//! before every statement (see `translate`). That shim forwards to the
//! process-global [`DebugHook`] installed via [`set_debug_hook`], if any.
//!
//! The hook is the boundary the debug adapter plugs into. It receives the
//! source byte offset of the statement about to run plus a pointer pair
//! describing the current frame's local variables, and decides whether to
//! pause (block the calling thread). Variable rendering is done here, in the
//! native crate, so the adapter never touches raw pointers — see
//! [`render_frame_vars`].
//!
//! Single debuggee at a time: the hook is global, matching one debug
//! session per process (a debug adapter drives exactly one).

use std::sync::{Arc, RwLock};

use leek_runtime::Value;

/// Receives a callback before each statement executes.
pub trait DebugHook: Send + Sync {
    /// Called on the executing (debuggee) thread before the statement at
    /// `offset` (a byte offset into the source) runs.
    ///
    /// `frame_desc` / `frame_values` describe the current function's locals:
    /// `frame_desc` is a `*const VarTable` and `frame_values` points at one
    /// `i64` slot per descriptor (both `0` when there are no named locals).
    /// Pass them to [`render_frame_vars`] to get displayable values.
    ///
    /// Implementations may block this thread to pause execution; they must
    /// eventually return for the program to make progress.
    fn safepoint(&self, offset: u32, frame_desc: usize, frame_values: usize);

    /// Called on function entry (debuggee thread), pushing a call frame.
    /// `frame_desc` is the entered function's `*const VarTable`.
    fn enter_frame(&self, frame_desc: usize) {
        let _ = frame_desc;
    }

    /// Called just before a function returns (debuggee thread), popping the
    /// top call frame.
    fn leave_frame(&self) {}
}

/// A compiled function's debug descriptor: its display name plus its named
/// locals. Built (and leaked to a `'static`) by the backend at compile time;
/// its address is baked into the generated `leek_dbg_*` calls.
pub struct VarTable {
    pub func_name: String,
    pub vars: Vec<VarDesc>,
}

/// One named local: its display name and storage kind.
pub struct VarDesc {
    pub name: String,
    /// `0` = int, `1` = real, `2` = bool, `3` = boxed `Value` handle.
    pub kind: u8,
}

static HOOK: RwLock<Option<Arc<dyn DebugHook>>> = RwLock::new(None);

/// Install (or clear, with `None`) the global debug hook. The adapter sets
/// this before running an instrumented program and clears it afterward.
///
/// # Panics
/// Panics only if the lock is poisoned (a prior holder panicked).
pub fn set_debug_hook(hook: Option<Arc<dyn DebugHook>>) {
    *HOOK.write().expect("debug-hook lock poisoned") = hook;
}

/// Render a frame's locals to `(name, value)` string pairs. Safe wrapper the
/// adapter calls while the debuggee is parked (so the frame is alive and the
/// values are stable). `desc`/`values` are the pointers handed to
/// [`DebugHook::safepoint`].
#[must_use]
pub fn render_frame_vars(desc: usize, values: usize) -> Vec<(String, String)> {
    if desc == 0 || values == 0 {
        return Vec::new();
    }
    // SAFETY: `desc` is a `*const VarTable` leaked by the backend for the
    // duration of the process, and `values` points at `table.vars.len()`
    // i64 slots in the parked debuggee's live frame.
    let table = unsafe { &*(desc as *const VarTable) };
    let slots = values as *const i64;
    table
        .vars
        .iter()
        .enumerate()
        .map(|(i, desc)| {
            let raw = unsafe { *slots.add(i) };
            (desc.name.clone(), render_slot(desc.kind, raw))
        })
        .collect()
}

/// The display name of the function a descriptor belongs to. `desc` is a
/// `*const VarTable` handed to [`DebugHook::enter_frame`].
#[must_use]
pub fn frame_name(desc: usize) -> Option<String> {
    if desc == 0 {
        return None;
    }
    // SAFETY: `desc` is a `*const VarTable` leaked by the backend.
    let table = unsafe { &*(desc as *const VarTable) };
    Some(table.func_name.clone())
}

fn render_slot(kind: u8, raw: i64) -> String {
    match kind {
        0 => raw.to_string(),
        1 => f64::from_bits(raw as u64).to_string(),
        2 => if raw != 0 { "true" } else { "false" }.to_string(),
        3 => {
            if raw == 0 {
                "null".to_string()
            } else {
                // SAFETY: a kind-3 slot holds a live boxed-`Value` handle.
                let value = unsafe { &*(raw as *const Value) };
                value.to_string()
            }
        }
        _ => "<unknown>".to_string(),
    }
}

/// Forward a safepoint to the installed hook. Called from the
/// `leek_dbg_safepoint` runtime shim. The `Arc` is cloned out and the lock
/// released *before* calling `safepoint`, so a hook that blocks (to pause)
/// doesn't hold the lock.
pub(crate) fn fire_safepoint(offset: u32, desc: usize, values: usize) {
    let hook = HOOK.read().expect("debug-hook lock poisoned").clone();
    if let Some(hook) = hook {
        hook.safepoint(offset, desc, values);
    }
}

/// Forward a function entry to the installed hook (pushes a call frame).
pub(crate) fn fire_enter(desc: usize) {
    let hook = HOOK.read().expect("debug-hook lock poisoned").clone();
    if let Some(hook) = hook {
        hook.enter_frame(desc);
    }
}

/// Forward a function return to the installed hook (pops a call frame).
pub(crate) fn fire_leave() {
    let hook = HOOK.read().expect("debug-hook lock poisoned").clone();
    if let Some(hook) = hook {
        hook.leave_frame();
    }
}
