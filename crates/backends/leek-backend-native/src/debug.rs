//! Debug hook seam for the native backend.
//!
//! When a program is compiled with [`crate::NativeOptions::debug_hooks`],
//! the generated code calls `leek_dbg_safepoint(pos, desc, values)` before
//! every statement (see `translate`). That shim forwards to the
//! process-global [`DebugHook`] installed via [`set_debug_hook`], if any.
//!
//! The hook is the boundary the debug adapter plugs into. It receives the
//! statement's source *and* byte offset plus a pointer pair describing the
//! current frame's local variables, and decides whether to pause (block the
//! calling thread). Carrying the source matters once a program is spliced
//! from several files: an offset alone would be looked up in the wrong
//! file's line table. Variable rendering is done here, in the native crate,
//! so the adapter never touches raw pointers — see [`render_frame_vars`].
//!
//! Single debuggee at a time: the hook is global, matching one debug
//! session per process (a debug adapter drives exactly one).

use std::sync::{Arc, RwLock};

use leek_runtime::Value;
use leek_span::SourceId;

/// Pack a statement's `(source, byte offset)` into the single `i64` the
/// generated `leek_dbg_safepoint` call carries: source id in the high 32
/// bits, offset in the low 32. Keeping it one argument leaves the shim's
/// signature (and every existing call site) untouched.
pub(crate) fn pack_position(source: SourceId, offset: u32) -> i64 {
    ((u64::from(source.get()) << 32) | u64::from(offset)) as i64
}

/// Inverse of [`pack_position`]: `(source id, byte offset)`.
fn unpack_position(packed: i64) -> (u32, u32) {
    let bits = packed as u64;
    ((bits >> 32) as u32, bits as u32)
}

/// Receives a callback before each statement executes.
pub trait DebugHook: Send + Sync {
    /// Called on the executing (debuggee) thread before the statement at
    /// `offset` (a byte offset into the file `source` identifies) runs.
    /// `source` is the raw [`SourceId`] value the statement's span carries,
    /// so a program spliced from included files reports which file each
    /// safepoint belongs to.
    ///
    /// `frame_desc` / `frame_values` describe the current function's locals:
    /// `frame_desc` is a `*const VarTable` and `frame_values` points at one
    /// `i64` slot per descriptor (both `0` when there are no named locals).
    /// Pass them to [`render_frame_vars`] to get displayable values.
    ///
    /// Implementations may block this thread to pause execution; they must
    /// eventually return for the program to make progress.
    fn safepoint(&self, source: u32, offset: u32, frame_desc: usize, frame_values: usize);

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
/// this before running an instrumented program; to take one back out again,
/// reach for [`clear_debug_hook`], which removes only the hook it is handed.
///
/// # Panics
/// Panics only if the lock is poisoned (a prior holder panicked).
pub fn set_debug_hook(hook: Option<Arc<dyn DebugHook>>) {
    *HOOK.write().expect("debug-hook lock poisoned") = hook;
}

/// Remove `hook` from the global slot, and only `hook`: if something else is
/// installed by now, it is left exactly where it is. Reports whether `hook`
/// was the installed one.
///
/// The slot is process-global, so "I am done, clear it" is not a thing a
/// caller can safely say — a run that has finished may be tidying up long
/// after another debug session took the slot over, and `set_debug_hook(None)`
/// would tear that live session down: its parked debuggee would resume with
/// no hook at all and run past every breakpoint to the end. Whoever installs
/// a hook removes that hook, and a run that installed none removes nothing.
///
/// # Panics
/// Panics only if the lock is poisoned (a prior holder panicked).
pub fn clear_debug_hook(hook: &Arc<dyn DebugHook>) -> bool {
    let mut slot = HOOK.write().expect("debug-hook lock poisoned");
    // Compared as thin addresses: two `Arc<dyn DebugHook>` for one allocation
    // may carry different vtable pointers, and the allocation is the identity.
    let installed = slot
        .as_ref()
        .map(|installed| Arc::as_ptr(installed).cast::<()>());
    if installed != Some(Arc::as_ptr(hook).cast::<()>()) {
        return false;
    }
    *slot = None;
    true
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
            // SAFETY: `i < table.vars.len()`, and `values` points at that
            // many i64 slots (see the borrow of `table` above), so the
            // offset is in bounds.
            let slot = unsafe { slots.add(i) };
            // SAFETY: the parked debuggee's frame is alive and the slot is
            // an initialised i64.
            let raw = unsafe { *slot };
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
/// `leek_dbg_safepoint` runtime shim with the packed position built by
/// [`pack_position`]. The `Arc` is cloned out and the lock released *before*
/// calling `safepoint`, so a hook that blocks (to pause) doesn't hold the
/// lock.
pub(crate) fn fire_safepoint(packed: i64, desc: usize, values: usize) {
    let hook = HOOK.read().expect("debug-hook lock poisoned").clone();
    if let Some(hook) = hook {
        let (source, offset) = unpack_position(packed);
        hook.safepoint(source, offset, desc, values);
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

#[cfg(test)]
mod tests {
    use super::{DebugHook, HOOK, clear_debug_hook, set_debug_hook};
    use std::sync::Arc;

    /// A hook that records nothing: these tests are about which `Arc` sits in
    /// the global slot, not about what a safepoint does.
    struct Inert;

    impl DebugHook for Inert {
        fn safepoint(&self, _source: u32, _offset: u32, _desc: usize, _values: usize) {}
    }

    fn installed() -> Option<*const ()> {
        HOOK.read()
            .expect("debug-hook lock poisoned")
            .as_ref()
            .map(|hook| Arc::as_ptr(hook).cast::<()>())
    }

    /// One test, not three: the slot is process-global, so two tests taking
    /// turns with it would be racing each other rather than testing it.
    #[test]
    fn a_hook_is_cleared_by_its_owner_and_by_nobody_else() {
        let mine: Arc<dyn DebugHook> = Arc::new(Inert);
        let someone_elses: Arc<dyn DebugHook> = Arc::new(Inert);
        set_debug_hook(Some(mine.clone()));

        assert!(
            !clear_debug_hook(&someone_elses),
            "clearing a hook that was never installed reported a removal"
        );
        assert_eq!(
            installed(),
            Some(Arc::as_ptr(&mine).cast::<()>()),
            "a stranger's clear took down the installed hook"
        );

        assert!(clear_debug_hook(&mine), "the owner's clear did nothing");
        assert_eq!(installed(), None, "the hook outlived its own clear");
        // And clearing an already-empty slot is simply false, not a panic.
        assert!(!clear_debug_hook(&mine));
    }
}
