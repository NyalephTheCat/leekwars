//! Native debug controller.
//!
//! Bridges the native backend's [`DebugHook`](leek_backend_native::DebugHook)
//! (called on the debuggee thread at function entry/exit and each statement)
//! to the DAP server (on the main thread). It maintains a shadow call stack
//! from the `enter`/`leave` callbacks and updates the top frame's line and
//! locals at each safepoint. When a stop is warranted — a breakpoint, a
//! completed step, or a forced stop (entry/pause) — it captures every live
//! frame's locals, fires `on_stop` (which sends a DAP `stopped` event), and
//! parks the debuggee thread on a condvar until [`resume`] is called.
//!
//! [`resume`]: NativeDebugSession::resume

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};

use leek_span::LineTable;

/// Why the debuggee paused — maps to a DAP `stopped` reason.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StopReason {
    Breakpoint,
    Entry,
    Pause,
    Step,
}

/// Reported to the DAP layer when the debuggee pauses.
pub(crate) struct StopInfo {
    pub line: u32,
    pub reason: StopReason,
}

/// One live call frame on the shadow stack.
struct Frame {
    /// `*const VarTable` for the function (its name + local descriptors).
    desc: usize,
    /// Pointer to the current local-value slots (updated each safepoint).
    values: usize,
    /// Current source line within the frame.
    line: u32,
}

/// A frame captured at a stop, ready to serve `stackTrace`/`variables`.
#[derive(Clone)]
pub(crate) struct FrameSnapshot {
    pub name: String,
    pub line: u32,
    pub vars: Vec<(String, String)>,
}

/// What the in-progress step is waiting for. `depth` is the call-stack depth
/// the step started at.
#[derive(Clone, Copy)]
enum Step {
    None,
    /// Stop at the next statement, even in a callee (step into).
    Into { line: u32, depth: usize },
    /// Stop at the next statement in this frame or a caller (step over).
    Over { line: u32, depth: usize },
    /// Stop when this frame returns (step out).
    Out { depth: usize },
}

struct Wait {
    resume: bool,
}

/// One native debug session. Implements [`leek_backend_native::DebugHook`];
/// install it with `leek_backend_native::set_debug_hook` before running an
/// instrumented program.
pub(crate) struct NativeDebugSession {
    line_table: LineTable,
    breakpoint_lines: HashSet<u32>,
    /// The shadow call stack (bottom .. top).
    stack: Mutex<Vec<Frame>>,
    /// Frames captured at the most recent stop, top-first.
    snapshot: Mutex<Vec<FrameSnapshot>>,
    /// Line of the previous safepoint (breakpoints fire on arrival from a
    /// different line — see the breakpoint check).
    prev_line: AtomicU32,
    /// Force a stop at the next safepoint (program entry or client `pause`).
    stop_next: AtomicBool,
    /// The pending forced stop is program entry.
    entry_stop: AtomicBool,
    /// The in-progress step, if any.
    step: Mutex<Step>,
    wait: Mutex<Wait>,
    cv: Condvar,
    /// Sends the DAP `stopped` event. Called on the debuggee thread.
    on_stop: Box<dyn Fn(StopInfo) + Send + Sync>,
}

impl NativeDebugSession {
    pub(crate) fn new(
        source: &str,
        breakpoint_lines: HashSet<u32>,
        stop_on_entry: bool,
        on_stop: Box<dyn Fn(StopInfo) + Send + Sync>,
    ) -> Self {
        Self {
            line_table: LineTable::new(source),
            breakpoint_lines,
            stack: Mutex::new(Vec::new()),
            snapshot: Mutex::new(Vec::new()),
            prev_line: AtomicU32::new(0),
            stop_next: AtomicBool::new(stop_on_entry),
            entry_stop: AtomicBool::new(stop_on_entry),
            step: Mutex::new(Step::None),
            wait: Mutex::new(Wait { resume: false }),
            cv: Condvar::new(),
            on_stop,
        }
    }

    /// Frames captured at the most recent stop, top-first.
    pub(crate) fn frames(&self) -> Vec<FrameSnapshot> {
        self.snapshot.lock().expect("snapshot lock poisoned").clone()
    }

    /// Release a parked debuggee so it continues running.
    pub(crate) fn resume(&self) {
        *self.step.lock().expect("step lock poisoned") = Step::None;
        self.wake();
    }

    /// Ask the debuggee to stop at the next statement (client `pause`).
    pub(crate) fn request_pause(&self) {
        self.stop_next.store(true, Ordering::SeqCst);
    }

    /// Step into: stop at the very next statement (including a callee).
    pub(crate) fn step_into(&self) {
        let (line, depth) = self.top();
        *self.step.lock().expect("step lock poisoned") = Step::Into { line, depth };
        self.wake();
    }

    /// Step over: stop at the next statement in this frame or a caller.
    pub(crate) fn step_over(&self) {
        let (line, depth) = self.top();
        *self.step.lock().expect("step lock poisoned") = Step::Over { line, depth };
        self.wake();
    }

    /// Step out: stop once this frame has returned.
    pub(crate) fn step_out(&self) {
        let (_, depth) = self.top();
        *self.step.lock().expect("step lock poisoned") = Step::Out { depth };
        self.wake();
    }

    /// Current (line, depth) of the top frame.
    fn top(&self) -> (u32, usize) {
        let stack = self.stack.lock().expect("stack lock poisoned");
        (stack.last().map_or(0, |f| f.line), stack.len())
    }

    fn wake(&self) {
        let mut wait = self.wait.lock().expect("debug wait lock poisoned");
        wait.resume = true;
        self.cv.notify_all();
    }

    /// Capture every live frame's name/line/locals (debuggee thread, frames
    /// alive), publish the snapshot, then park until resumed.
    fn stop(&self, line: u32, reason: StopReason) {
        // Clone the frame pointers out under the lock, then render outside it.
        let frames: Vec<(usize, usize, u32)> = {
            let stack = self.stack.lock().expect("stack lock poisoned");
            stack.iter().rev().map(|f| (f.desc, f.values, f.line)).collect()
        };
        let snapshot = frames
            .into_iter()
            .map(|(desc, values, fline)| FrameSnapshot {
                name: leek_backend_native::frame_name(desc).unwrap_or_else(|| "<unknown>".into()),
                line: fline,
                vars: leek_backend_native::render_frame_vars(desc, values),
            })
            .collect();
        *self.snapshot.lock().expect("snapshot lock poisoned") = snapshot;

        (self.on_stop)(StopInfo { line, reason });

        let mut wait = self.wait.lock().expect("debug wait lock poisoned");
        wait.resume = false;
        while !wait.resume {
            wait = self.cv.wait(wait).expect("debug wait lock poisoned");
        }
    }
}

impl leek_backend_native::DebugHook for NativeDebugSession {
    fn safepoint(&self, offset: u32, frame_desc: usize, frame_values: usize) {
        let line = self.line_table.line_col(offset).line;
        let prev = self.prev_line.swap(line, Ordering::SeqCst);

        // Update the top frame and read the current depth.
        let depth = {
            let mut stack = self.stack.lock().expect("stack lock poisoned");
            if let Some(top) = stack.last_mut() {
                top.line = line;
                top.values = frame_values;
                top.desc = frame_desc;
            }
            stack.len()
        };

        let reason = if self.stop_next.swap(false, Ordering::SeqCst) {
            if self.entry_stop.swap(false, Ordering::SeqCst) {
                StopReason::Entry
            } else {
                StopReason::Pause
            }
        } else if self.step_reached(line, depth) {
            StopReason::Step
        } else if line != prev && self.breakpoint_lines.contains(&line) {
            StopReason::Breakpoint
        } else {
            return;
        };

        self.stop(line, reason);
    }

    fn enter_frame(&self, frame_desc: usize) {
        self.stack.lock().expect("stack lock poisoned").push(Frame {
            desc: frame_desc,
            values: 0,
            line: 0,
        });
    }

    fn leave_frame(&self) {
        let len = {
            let mut stack = self.stack.lock().expect("stack lock poisoned");
            stack.pop();
            stack.len()
        };
        // A pending step-out completes when the target frame returns; arm a
        // forced stop at the next safepoint in the caller. (Copy the step out
        // first — `Step` is `Copy` — to avoid re-locking while borrowed.)
        let step = *self.step.lock().expect("step lock poisoned");
        if let Step::Out { depth } = step
            && len < depth
        {
            *self.step.lock().expect("step lock poisoned") = Step::Into { line: 0, depth: len };
        }
    }
}

impl NativeDebugSession {
    /// Whether the in-progress step is satisfied at this (line, depth).
    fn step_reached(&self, line: u32, depth: usize) -> bool {
        let mut step = self.step.lock().expect("step lock poisoned");
        let reached = match *step {
            Step::None => false,
            Step::Into { line: l, depth: d } => depth != d || line != l,
            Step::Over { line: l, depth: d } => depth < d || (depth == d && line != l),
            // Out is converted to Into on the matching `leave_frame`; reaching
            // here means we're still inside, so don't stop.
            Step::Out { .. } => false,
        };
        if reached {
            *step = Step::None;
        }
        reached
    }
}
