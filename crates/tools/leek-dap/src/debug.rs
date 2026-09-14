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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use leek_backend_native::DebugValue;
use leek_span::LineTable;

use crate::breakpoints::Trigger;
use crate::expr;
use crate::lock;

/// One file the debuggee was compiled from, keyed by the `SourceId` its spans
/// carry. A program built through the project include graph is spliced from
/// several files, so an offset only means something together with its source.
pub(crate) struct DebugSource {
    /// Path reported to the client in stack frames.
    pub path: String,
    pub line_table: LineTable,
}

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
    /// Ids of the breakpoints that caused the stop; empty for a step, a
    /// client `pause`, or program entry. DAP's `stopped` event carries these
    /// so the client can point at the breakpoint that fired.
    pub hit_breakpoint_ids: Vec<i64>,
}

/// What a breakpoint whose line has been reached does.
enum Fires {
    /// Stop the debuggee and announce it.
    Yes,
    /// Do nothing: the condition was false, or the hit count was not one the
    /// client asked for.
    No,
    /// Print this and keep running — a logpoint.
    Log(String),
}

/// One executed safepoint, for the arrival test in [`NativeDebugSession::arrived`].
#[derive(Clone, Copy)]
struct Site {
    /// Call-stack depth the safepoint ran at.
    depth: usize,
    /// Raw `SourceId` of the file it belongs to.
    source: u32,
    line: u32,
    /// Byte offset within the file — the only thing that separates two
    /// safepoints on the *same* line, and so the only thing that can tell a
    /// loop back-edge from the next statement along.
    offset: u32,
}

/// One live call frame on the shadow stack.
struct Frame {
    /// `*const VarTable` for the function (its name + local descriptors).
    desc: usize,
    /// Pointer to the current local-value slots (updated each safepoint).
    values: usize,
    /// Current source line within the frame.
    line: u32,
    /// Raw `SourceId` of the file that line belongs to.
    source: u32,
}

/// One local of a captured frame.
#[derive(Clone)]
pub(crate) struct FrameVar {
    pub name: String,
    /// The text `variables` reports. Rendered where it was read — on the
    /// debuggee thread, the only one where the runtime's display version is
    /// settled — rather than later, on the request loop.
    pub display: String,
    /// The same value typed, for `evaluate` to compute with.
    pub value: DebugValue,
}

/// A frame captured at a stop, ready to serve `stackTrace`/`variables`.
#[derive(Clone)]
pub(crate) struct FrameSnapshot {
    pub name: String,
    pub line: u32,
    /// Path of the file the frame is executing in — an included file when the
    /// program was compiled through the project include graph. `None` for a
    /// source the adapter has no text for (a synthetic span).
    pub path: Option<String>,
    pub vars: Vec<FrameVar>,
}

/// What the in-progress step is waiting for. `depth` is the call-stack depth
/// the step started at.
#[derive(Clone, Copy)]
enum Step {
    None,
    /// Stop at the next statement, even in a callee (step into).
    Into {
        line: u32,
        depth: usize,
    },
    /// Stop at the next statement in this frame or a caller (step over).
    Over {
        line: u32,
        depth: usize,
    },
    /// Stop when this frame returns (step out).
    Out {
        depth: usize,
    },
}

struct Wait {
    /// True from the moment the debuggee claims a stop until a [`wake`] frees
    /// it. The claim is taken *before* `on_stop` fires the DAP `stopped`
    /// event, so a `continue`/step arriving in that window clears the flag
    /// rather than being overwritten by a later reset — the debuggee then
    /// sails straight through the park instead of hanging forever.
    ///
    /// [`wake`]: NativeDebugSession::wake
    stopped: bool,
    /// Set once the session is being torn down — see [`detach`]. Lives under
    /// the same lock as `stopped` so a stop claimed in the instant before the
    /// detach is still seen, rather than parking a thread nobody will wake.
    ///
    /// [`detach`]: NativeDebugSession::detach
    detached: bool,
}

/// One native debug session. Implements [`leek_backend_native::DebugHook`];
/// install it with `leek_backend_native::set_debug_hook` before running an
/// instrumented program.
pub(crate) struct NativeDebugSession {
    /// Line tables and paths, keyed by raw `SourceId`.
    sources: HashMap<u32, DebugSource>,
    /// Breakpoint by line, per raw `SourceId`, so a breakpoint in an
    /// included file is matched against that file's own lines. Behind a lock
    /// and replaced wholesale by [`set_breakpoints`]: the client adds and
    /// removes breakpoints at any time, including while the debuggee is
    /// parked at a stop.
    ///
    /// [`set_breakpoints`]: NativeDebugSession::set_breakpoints
    breakpoints: RwLock<HashMap<u32, HashMap<u32, Trigger>>>,
    /// How often each breakpoint has been hit with its condition satisfied,
    /// by breakpoint id — what a `hitCondition` counts.
    hits: Mutex<HashMap<i64, u64>>,
    /// The shadow call stack (bottom .. top).
    stack: Mutex<Vec<Frame>>,
    /// Frames captured at the most recent stop, top-first.
    snapshot: Mutex<Vec<FrameSnapshot>>,
    /// The safepoint executed just before the current one, or `None` at the
    /// very first. Breakpoints fire on *arrival* — see [`Self::arrived`].
    prev: Mutex<Option<Site>>,
    /// Force a stop at the next safepoint (program entry or client `pause`).
    stop_next: AtomicBool,
    /// The pending forced stop is program entry.
    entry_stop: AtomicBool,
    /// The in-progress step, if any.
    step: Mutex<Step>,
    wait: Mutex<Wait>,
    cv: Condvar,
    /// Set the first time one of the locks above is taken poisoned — a
    /// thread panicked while holding it, so what is under it is whatever the
    /// panic left behind. See [`Self::poisoned`].
    poisoned: AtomicBool,
    /// Sends the DAP `stopped` event. Called on the debuggee thread.
    on_stop: Box<dyn Fn(StopInfo) + Send + Sync>,
    /// Sends a DAP `output` event: a logpoint's message, or the complaint of
    /// a condition that could not be evaluated. Also the debuggee thread.
    on_output: Box<dyn Fn(String) + Send + Sync>,
}

impl NativeDebugSession {
    pub(crate) fn new(
        sources: HashMap<u32, DebugSource>,
        breakpoints: HashMap<u32, HashMap<u32, Trigger>>,
        stop_on_entry: bool,
        on_stop: Box<dyn Fn(StopInfo) + Send + Sync>,
        on_output: Box<dyn Fn(String) + Send + Sync>,
    ) -> Self {
        Self {
            sources,
            breakpoints: RwLock::new(breakpoints),
            hits: Mutex::new(HashMap::new()),
            stack: Mutex::new(Vec::new()),
            snapshot: Mutex::new(Vec::new()),
            prev: Mutex::new(None),
            stop_next: AtomicBool::new(stop_on_entry),
            entry_stop: AtomicBool::new(stop_on_entry),
            step: Mutex::new(Step::None),
            wait: Mutex::new(Wait {
                stopped: false,
                detached: false,
            }),
            cv: Condvar::new(),
            poisoned: AtomicBool::new(false),
            on_stop,
            on_output,
        }
    }

    /// Take one of this session's own locks, recovering from poisoning and
    /// remembering that it happened (#176 — see [`crate::lock`] for why both
    /// halves are needed).
    fn locked<'a, T>(&self, mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
        lock::unpoisoned_seen(mutex.lock(), &self.poisoned)
    }

    /// [`Self::locked`] for the read half of an `RwLock`.
    fn read_locked<'a, T>(&self, rw: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
        lock::unpoisoned_seen(rw.read(), &self.poisoned)
    }

    /// [`Self::locked`] for the write half of an `RwLock`.
    fn write_locked<'a, T>(&self, rw: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
        lock::unpoisoned_seen(rw.write(), &self.poisoned)
    }

    /// Whether a panic has left one of this session's locks poisoned.
    ///
    /// Recovering the guard keeps the adapter alive, but the state recovered
    /// with it is whatever the panicking thread left: a shadow frame pushed
    /// with no line yet, a snapshot from the stop before this one, a hit
    /// count half-incremented. The request loop asks after every request and
    /// ends the session rather than go on answering `stackTrace`, `variables`
    /// and `evaluate` out of it (#176) — and [`Self::stop`] announces no
    /// further stops in the meantime.
    pub(crate) fn poisoned(&self) -> bool {
        self.poisoned.load(Ordering::SeqCst)
    }

    /// Replace the whole breakpoint set, keyed by raw `SourceId` then line.
    ///
    /// Whole-set replacement, not add/remove: the session owns the client's
    /// breakpoints and always hands over the complete picture, so the two
    /// cannot drift apart. Safe to call at any point in the run — while the
    /// debuggee is running, and while it is parked at a stop.
    pub(crate) fn set_breakpoints(&self, by_source: HashMap<u32, HashMap<u32, Trigger>>) {
        // Ids are never re-minted, so a stale hit count can never be charged
        // to a new breakpoint — but a breakpoint the client removed and set
        // again is a new id, and one it merely re-sent keeps its count, which
        // is what a user watching a `hitCondition` expects.
        let live: HashSet<i64> = by_source
            .values()
            .flat_map(|lines| lines.values().map(|trigger| trigger.id))
            .collect();
        self.locked(&self.hits).retain(|id, _| live.contains(id));
        *self.write_locked(&self.breakpoints) = by_source;
    }

    /// The breakpoint on this line, if the client set one.
    ///
    /// Takes the read lock and drops it before returning: the caller goes on
    /// to park the debuggee, and a lock held across that park would block the
    /// very `setBreakpoints` that could free it.
    fn breakpoint_at(&self, source: u32, line: u32) -> Option<Trigger> {
        self.read_locked(&self.breakpoints)
            .get(&source)?
            .get(&line)
            .cloned()
    }

    /// What a breakpoint whose line has been reached should do.
    ///
    /// Runs on the debuggee thread, before the stop is claimed, so it must
    /// not panic: a dead debuggee leaves the request loop waiting on a stop
    /// that never comes. Every step returns a `Result` instead.
    fn fires(&self, trigger: &Trigger, frame_desc: usize, frame_values: usize) -> Fires {
        // Only a breakpoint that has something to say reads the frame; a
        // plain one is the common case and pays nothing.
        let vars = if trigger.condition.is_some() || trigger.log.is_some() {
            leek_backend_native::read_frame_vars(frame_desc, frame_values)
        } else {
            Vec::new()
        };

        if let Some(condition) = &trigger.condition {
            match expr::eval(condition, &vars, trigger.version) {
                Ok(value) if !value.is_truthy() => return Fires::No,
                Ok(_) => {}
                // Fail open, loudly. A condition that cannot be evaluated
                // here — a name that is not in scope at this line — would
                // otherwise turn the breakpoint silently off, which is the
                // one failure a user cannot see. Stopping and saying why is
                // what every mainstream adapter does.
                Err(message) => {
                    (self.on_output)(format!("breakpoint condition: {message}\n"));
                    return Fires::Yes;
                }
            }
        }

        if let Some(hit) = &trigger.hit {
            let count = {
                let mut hits = self.locked(&self.hits);
                let count = hits.entry(trigger.id).or_insert(0);
                *count += 1;
                *count
            };
            if !hit.passes(count) {
                return Fires::No;
            }
        }

        match &trigger.log {
            Some(log) => Fires::Log(expr::interpolate(log, &vars, trigger.version)),
            None => Fires::Yes,
        }
    }

    /// Whether `now` counts as *arriving* at its line — the condition a
    /// breakpoint fires on — and record it as the new previous site.
    ///
    /// Consecutive safepoints on the same line of the same frame are one
    /// arrival, so `var a = 1 var b = 2` written on one line stops once. A
    /// safepoint at or before the previous offset is a backward jump, so the
    /// back-edge of `for (var i = 0; i < 3; i++) { sum += i }` written on one
    /// line re-arms the breakpoint and it fires every iteration; a different
    /// depth covers recursion re-entering the same line.
    ///
    /// The previous site is recorded whether or not this is an arrival: the
    /// test means nothing unless *every* safepoint updates it.
    fn arrived(&self, now: Site) -> bool {
        let mut prev = self.locked(&self.prev);
        let arrived = prev.is_none_or(|p| {
            p.depth != now.depth
                || p.source != now.source
                || p.line != now.line
                || now.offset <= p.offset
        });
        *prev = Some(now);
        arrived
    }

    /// Frames captured at the most recent stop, top-first.
    pub(crate) fn frames(&self) -> Vec<FrameSnapshot> {
        self.locked(&self.snapshot).clone()
    }

    /// Tear the session down: release a parked debuggee and let every later
    /// safepoint run straight through without announcing a stop.
    ///
    /// A `disconnect` sent while the debuggee is parked at a breakpoint would
    /// otherwise leave its thread on the condvar for the life of the process,
    /// with the process-global debug hook still installed. Harmless for the
    /// standalone binary, which exits; fatal for an embedder, and for a test
    /// binary that runs one session after another.
    pub(crate) fn detach(&self) {
        let mut wait = self.locked(&self.wait);
        wait.detached = true;
        wait.stopped = false;
        self.cv.notify_all();
    }

    /// Release a parked debuggee so it continues running.
    pub(crate) fn resume(&self) {
        *self.locked(&self.step) = Step::None;
        self.wake();
    }

    /// Ask the debuggee to stop at the next statement (client `pause`).
    pub(crate) fn request_pause(&self) {
        self.stop_next.store(true, Ordering::SeqCst);
    }

    /// Step into: stop at the very next statement (including a callee).
    pub(crate) fn step_into(&self) {
        let (line, depth) = self.top();
        *self.locked(&self.step) = Step::Into { line, depth };
        self.wake();
    }

    /// Step over: stop at the next statement in this frame or a caller.
    pub(crate) fn step_over(&self) {
        let (line, depth) = self.top();
        *self.locked(&self.step) = Step::Over { line, depth };
        self.wake();
    }

    /// Step out: stop once this frame has returned.
    pub(crate) fn step_out(&self) {
        let (_, depth) = self.top();
        *self.locked(&self.step) = Step::Out { depth };
        self.wake();
    }

    /// Current (line, depth) of the top frame.
    fn top(&self) -> (u32, usize) {
        let stack = self.locked(&self.stack);
        (stack.last().map_or(0, |f| f.line), stack.len())
    }

    /// Release the stop the debuggee is currently in — whether it has already
    /// parked or is still between claiming the stop and parking. A `wake` with
    /// no stop in flight is a no-op, so a stray `continue` cannot swallow a
    /// later breakpoint.
    fn wake(&self) {
        let mut wait = self.locked(&self.wait);
        wait.stopped = false;
        self.cv.notify_all();
    }

    /// Capture every live frame's name/line/locals (debuggee thread, frames
    /// alive), publish the snapshot, then park until resumed.
    fn stop(&self, line: u32, reason: StopReason, hit_breakpoint_ids: Vec<i64>) {
        // A panic has already cost this session part of its state and the
        // request loop is about to end it (see [`Self::poisoned`]). Announce
        // nothing and park nobody: a thread parked here would be waiting for
        // a `continue` from a client that has been told the session is over,
        // and the frames it stopped to show would come off a stack whose top
        // may never have been finished.
        if self.poisoned() {
            return;
        }

        // Clone the frame pointers out under the lock, then render outside it.
        let frames: Vec<(usize, usize, u32, u32)> = {
            let stack = self.locked(&self.stack);
            stack
                .iter()
                .rev()
                .map(|f| (f.desc, f.values, f.line, f.source))
                .collect()
        };
        let snapshot = frames
            .into_iter()
            .map(|(desc, values, fline, fsource)| FrameSnapshot {
                name: leek_backend_native::frame_name(desc).unwrap_or_else(|| "<unknown>".into()),
                line: fline,
                path: self.sources.get(&fsource).map(|s| s.path.clone()),
                vars: leek_backend_native::read_frame_vars(desc, values)
                    .into_iter()
                    .map(|(name, value)| FrameVar {
                        name,
                        display: value.render(),
                        value,
                    })
                    .collect(),
            })
            .collect();
        *self.locked(&self.snapshot) = snapshot;

        // Claim the stop before announcing it. The client may answer the
        // `stopped` event with `continue` (or a step) before this thread
        // reaches the condvar; that `wake` clears the flag we just set, so the
        // loop below sees the resume instead of losing it.
        {
            let mut wait = self.locked(&self.wait);
            // A detached session has no client left to answer the stop, and
            // whoever detached is waiting for this thread to finish. A lock
            // found poisoned since the check at the top of this function is
            // the same story: the request loop is ending the session, so do
            // not announce one more stop into it.
            if wait.detached || self.poisoned() {
                return;
            }
            wait.stopped = true;
        }

        (self.on_stop)(StopInfo {
            line,
            reason,
            hit_breakpoint_ids,
        });

        let mut wait = self.locked(&self.wait);
        while wait.stopped {
            wait = lock::unpoisoned_seen(self.cv.wait(wait), &self.poisoned);
        }
    }
}

impl leek_backend_native::DebugHook for NativeDebugSession {
    fn safepoint(&self, source: u32, offset: u32, frame_desc: usize, frame_values: usize) {
        // An offset only means something in its own file: look it up in that
        // source's line table, not the entry file's. A source the adapter has
        // no text for (a synthetic span) reports line 0 and never matches a
        // breakpoint.
        let line = self
            .sources
            .get(&source)
            .map_or(0, |s| s.line_table.line_col(offset).line);

        // Update the top frame and read the current depth.
        let depth = {
            let mut stack = self.locked(&self.stack);
            if let Some(top) = stack.last_mut() {
                top.line = line;
                top.values = frame_values;
                top.desc = frame_desc;
                top.source = source;
            }
            stack.len()
        };

        // Recorded on every safepoint, stop or no stop, so the *next* one can
        // tell "still on this line" from "back on this line".
        let arrived = self.arrived(Site {
            depth,
            source,
            line,
            offset,
        });

        let (reason, hit_breakpoint_ids) = if self.stop_next.swap(false, Ordering::SeqCst) {
            if self.entry_stop.swap(false, Ordering::SeqCst) {
                (StopReason::Entry, Vec::new())
            } else {
                (StopReason::Pause, Vec::new())
            }
        } else if self.step_reached(line, depth) {
            (StopReason::Step, Vec::new())
        } else if let Some(trigger) = arrived.then(|| self.breakpoint_at(source, line)).flatten() {
            match self.fires(&trigger, frame_desc, frame_values) {
                Fires::No => return,
                // A logpoint prints and keeps running — that is the whole
                // point of one. It is tested after the forced-stop and step
                // arms above, so a user stepping is never skipped past.
                Fires::Log(text) => {
                    (self.on_output)(text);
                    return;
                }
                Fires::Yes => (StopReason::Breakpoint, vec![trigger.id]),
            }
        } else {
            return;
        };

        self.stop(line, reason, hit_breakpoint_ids);
    }

    fn enter_frame(&self, frame_desc: usize) {
        self.locked(&self.stack).push(Frame {
            desc: frame_desc,
            values: 0,
            line: 0,
            source: 0,
        });
    }

    fn leave_frame(&self) {
        let len = {
            let mut stack = self.locked(&self.stack);
            stack.pop();
            stack.len()
        };
        // A pending step-out completes when the target frame returns; arm a
        // forced stop at the next safepoint in the caller. (Copy the step out
        // first — `Step` is `Copy` — to avoid re-locking while borrowed.)
        let step = *self.locked(&self.step);
        if let Step::Out { depth } = step
            && len < depth
        {
            *self.locked(&self.step) = Step::Into {
                line: 0,
                depth: len,
            };
        }
    }
}

impl NativeDebugSession {
    /// Whether the in-progress step is satisfied at this (line, depth).
    fn step_reached(&self, line: u32, depth: usize) -> bool {
        let mut step = self.locked(&self.step);
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::sync::{Arc, OnceLock, Weak};
    use std::time::Duration;

    use leek_backend_native::DebugHook;

    use leek_span::LineTable;

    use super::{DebugSource, Frame, FrameSnapshot, Mutex, NativeDebugSession, StopInfo};
    use crate::breakpoints::{BreakpointSpec, BreakpointStore, ProgramMap, Trigger};

    const SOURCE: &str = "var a = 1;\nvar b = 2;\n";
    /// Raw `SourceId` of the entry file (the pipeline seeds it with 1).
    const ENTRY: u32 = 1;

    /// One-file source table, as a single-file launch builds.
    fn entry_sources() -> HashMap<u32, DebugSource> {
        sources([(ENTRY, "main.leek", SOURCE)])
    }

    fn sources<const N: usize>(files: [(u32, &str, &str); N]) -> HashMap<u32, DebugSource> {
        files
            .into_iter()
            .map(|(id, path, text)| {
                (
                    id,
                    DebugSource {
                        path: path.to_string(),
                        line_table: LineTable::new(text),
                    },
                )
            })
            .collect()
    }

    /// How long a released debuggee gets to leave `stop` before we call it
    /// parked. Generous: the assertion only fires on a genuine hang.
    const RELEASED: Duration = Duration::from_secs(5);
    /// How long a parked debuggee is watched to confirm it stays parked.
    const PARKED: Duration = Duration::from_millis(200);

    /// A session that runs `on_stop` with a handle to itself and the stop
    /// info, so the callback can answer the stop the way a DAP client does.
    fn session_from(
        sources: HashMap<u32, DebugSource>,
        breakpoints: HashMap<u32, HashMap<u32, Trigger>>,
        stop_on_entry: bool,
        on_stop: impl Fn(&NativeDebugSession, &StopInfo) + Send + Sync + 'static,
    ) -> Arc<NativeDebugSession> {
        session_logging(sources, breakpoints, stop_on_entry, on_stop, |_| {})
    }

    /// [`session_from`] with the `output` sink the DAP layer wires up — a
    /// logpoint's text, and a condition's complaint, arrive there.
    fn session_logging(
        sources: HashMap<u32, DebugSource>,
        breakpoints: HashMap<u32, HashMap<u32, Trigger>>,
        stop_on_entry: bool,
        on_stop: impl Fn(&NativeDebugSession, &StopInfo) + Send + Sync + 'static,
        on_output: impl Fn(String) + Send + Sync + 'static,
    ) -> Arc<NativeDebugSession> {
        let slot: Arc<OnceLock<Weak<NativeDebugSession>>> = Arc::new(OnceLock::new());
        let slot_for_cb = Arc::clone(&slot);
        let session = Arc::new(NativeDebugSession::new(
            sources,
            breakpoints,
            stop_on_entry,
            Box::new(move |info: StopInfo| {
                let me = slot_for_cb
                    .get()
                    .expect("session slot filled")
                    .upgrade()
                    .expect("session alive");
                on_stop(&me, &info);
            }),
            Box::new(on_output),
        ));
        assert!(
            slot.set(Arc::downgrade(&session)).is_ok(),
            "session slot set once"
        );
        session
    }

    /// A single-file session that stops on entry.
    fn session_with(
        on_stop: impl Fn(&NativeDebugSession) + Send + Sync + 'static,
    ) -> Arc<NativeDebugSession> {
        session_from(entry_sources(), HashMap::new(), true, move |session, _| {
            on_stop(session);
        })
    }

    /// Run one safepoint on a worker thread; the receiver fires when the
    /// debuggee leaves the stop. Driving it off-thread turns a hang into a
    /// failed assertion instead of a wedged test binary.
    fn spawn_safepoint(session: &Arc<NativeDebugSession>) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel();
        let worker = Arc::clone(session);
        std::thread::spawn(move || {
            worker.safepoint(ENTRY, 0, 0, 0);
            let _ = tx.send(());
        });
        rx
    }

    #[test]
    fn resume_racing_the_stopped_event_is_not_lost() {
        let session = session_with(NativeDebugSession::resume);
        assert!(
            spawn_safepoint(&session).recv_timeout(RELEASED).is_ok(),
            "debuggee stayed parked after a resume that raced the stopped event"
        );
    }

    #[test]
    fn step_racing_the_stopped_event_is_not_lost() {
        let session = session_with(NativeDebugSession::step_over);
        assert!(
            spawn_safepoint(&session).recv_timeout(RELEASED).is_ok(),
            "debuggee stayed parked after a step that raced the stopped event"
        );
    }

    #[test]
    fn resume_after_the_debuggee_parks_still_releases_it() {
        let (announced, stopped) = mpsc::channel();
        let session = session_with(move |_: &NativeDebugSession| {
            let _ = announced.send(());
        });

        let done = spawn_safepoint(&session);
        stopped.recv_timeout(RELEASED).expect("stopped event fired");
        assert!(
            done.recv_timeout(PARKED).is_err(),
            "debuggee left the stop without being resumed"
        );

        session.resume();
        assert!(
            done.recv_timeout(RELEASED).is_ok(),
            "debuggee stayed parked after resume"
        );
    }

    #[test]
    fn a_breakpoint_in_an_included_file_uses_that_files_lines() {
        const LIB: u32 = 2;
        // Byte 15 is line 3 of the included file but line 2 of the entry, so
        // resolving it against the entry's line table would report the wrong
        // line (and fire the entry's breakpoint instead of this one).
        let lib = "// leek\n// lib\nvar z = 9;\n";
        let (announced, stopped) = mpsc::channel();
        let session = session_from(
            sources([(ENTRY, "main.leek", SOURCE), (LIB, "lib.leek", lib)]),
            HashMap::from([
                (ENTRY, HashMap::from([(2, Trigger::plain(1))])),
                (LIB, HashMap::from([(3, Trigger::plain(2))])),
            ]),
            false,
            move |session, info| {
                let _ = announced.send(info.line);
                session.resume();
            },
        );

        let worker = Arc::clone(&session);
        let (finished, done) = mpsc::channel();
        std::thread::spawn(move || {
            worker.safepoint(LIB, 15, 0, 0);
            let _ = finished.send(());
        });

        assert_eq!(
            stopped.recv_timeout(RELEASED).ok(),
            Some(3),
            "included-file breakpoint reported the entry file's line"
        );
        assert!(
            done.recv_timeout(RELEASED).is_ok(),
            "debuggee stayed parked after resume"
        );
    }

    #[test]
    fn a_breakpoint_in_the_entry_does_not_fire_in_an_included_file() {
        const LIB: u32 = 2;
        let (announced, stopped) = mpsc::channel();
        let session = session_from(
            sources([
                (ENTRY, "main.leek", SOURCE),
                (LIB, "lib.leek", "var z = 9;\n"),
            ]),
            HashMap::from([(ENTRY, HashMap::from([(1, Trigger::plain(1))]))]),
            false,
            move |session, info| {
                let _ = announced.send(info.line);
                session.resume();
            },
        );

        // Line 1 of the *included* file: the entry's line-1 breakpoint is a
        // different file's and must not claim it.
        session.safepoint(LIB, 0, 0, 0);
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "an entry-file breakpoint fired inside an included file"
        );
    }

    /// A breakpoint session over a one-line program: every safepoint lands on
    /// line 1, so only the offsets tell them apart. Returns the session and a
    /// receiver of `(line, hit ids)` per stop; each stop resumes immediately,
    /// the way a client answering `stopped` with `continue` does.
    fn breakpoint_session(
        line: u32,
        id: i64,
    ) -> (Arc<NativeDebugSession>, mpsc::Receiver<(u32, Vec<i64>)>) {
        let (announced, stopped) = mpsc::channel();
        let session = session_from(
            sources([(
                ENTRY,
                "main.leek",
                "var sum = 0 for (var i = 0; i < 3; i++) sum += i\n",
            )]),
            HashMap::from([(ENTRY, HashMap::from([(line, Trigger::plain(id))]))]),
            false,
            move |session, info| {
                let _ = announced.send((info.line, info.hit_breakpoint_ids.clone()));
                session.resume();
            },
        );
        (session, stopped)
    }

    #[test]
    fn a_one_line_loop_body_fires_every_iteration() {
        let (session, stopped) = breakpoint_session(1, 7);
        // Body, increment, body again — all on line 1. The second body
        // safepoint is at an offset the loop already ran past, so it is a
        // fresh arrival and the breakpoint re-arms.
        session.safepoint(ENTRY, 30, 0, 0);
        session.safepoint(ENTRY, 42, 0, 0);
        session.safepoint(ENTRY, 30, 0, 0);

        assert_eq!(stopped.recv_timeout(RELEASED).ok(), Some((1, vec![7])));
        assert_eq!(
            stopped.recv_timeout(RELEASED).ok(),
            Some((1, vec![7])),
            "the loop's second pass over the breakpoint line did not fire"
        );
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "the increment safepoint fired a second stop on the same line"
        );
    }

    #[test]
    fn two_statements_on_one_line_stop_once() {
        let (session, stopped) = breakpoint_session(1, 7);
        // Forward through the line, statement by statement: one arrival.
        session.safepoint(ENTRY, 0, 0, 0);
        session.safepoint(ENTRY, 12, 0, 0);

        assert!(stopped.recv_timeout(RELEASED).is_ok(), "breakpoint missed");
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "a second statement on the same line fired the breakpoint again"
        );
    }

    #[test]
    fn a_breakpoint_hit_reports_its_id() {
        let (session, stopped) = breakpoint_session(1, 42);
        session.safepoint(ENTRY, 0, 0, 0);
        assert_eq!(
            stopped.recv_timeout(RELEASED).ok(),
            Some((1, vec![42])),
            "the stop did not name the breakpoint that caused it"
        );
    }

    #[test]
    fn a_breakpoint_set_after_the_run_started_still_fires() {
        let (announced, stopped) = mpsc::channel();
        let session = session_from(entry_sources(), HashMap::new(), false, move |s, info| {
            let _ = announced.send(info.hit_breakpoint_ids.clone());
            s.resume();
        });

        session.safepoint(ENTRY, 0, 0, 0);
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "stopped with no breakpoint set"
        );

        session.set_breakpoints(HashMap::from([(
            ENTRY,
            HashMap::from([(2, Trigger::plain(5))]),
        )]));
        session.safepoint(ENTRY, 11, 0, 0);
        assert_eq!(
            stopped.recv_timeout(RELEASED).ok(),
            Some(vec![5]),
            "a breakpoint set after the run started never reached the debuggee"
        );
    }

    #[test]
    fn setting_breakpoints_while_the_debuggee_is_parked_does_not_deadlock() {
        let (announced, stopped) = mpsc::channel();
        let session = session_with(move |_: &NativeDebugSession| {
            let _ = announced.send(());
        });

        let done = spawn_safepoint(&session);
        stopped.recv_timeout(RELEASED).expect("stopped event fired");

        // The debuggee is parked inside `stop`; the request loop updates the
        // breakpoint set from this thread, exactly as `setBreakpoints` does.
        session.set_breakpoints(HashMap::from([(
            ENTRY,
            HashMap::from([(1, Trigger::plain(3))]),
        )]));

        session.resume();
        assert!(
            done.recv_timeout(RELEASED).is_ok(),
            "setting breakpoints at a stop wedged the debuggee"
        );
    }

    /// A trigger as the DAP layer builds one: a spec put through the store
    /// and compiled against a one-file program, so these tests exercise the
    /// same path `setBreakpoints` does rather than a hand-built `Trigger`.
    fn trigger_for(line: u32, spec: BreakpointSpec) -> Trigger {
        let path = std::path::Path::new("/tmp/leek-dap-trigger/main.leek");
        let mut store = BreakpointStore::default();
        store.replace(path, &[spec]);
        let program = ProgramMap::new(
            HashMap::from([(leek_span::paths::canonical_or_normalized(path), ENTRY)]),
            HashMap::from([(ENTRY, std::collections::BTreeSet::from([line]))]),
            leek_span::pragma::LATEST_VERSION,
        );
        store
            .by_source(&program)
            .remove(&ENTRY)
            .and_then(|mut lines| lines.remove(&line))
            .expect("the breakpoint compiled")
    }

    /// A session with one breakpoint on line 1, reporting stops and output
    /// separately. Locals come from `vars`, the frame-descriptor pair a real
    /// safepoint carries — `(0, 0)` for a frame with no named locals.
    fn conditional_session(
        spec: BreakpointSpec,
    ) -> (
        Arc<NativeDebugSession>,
        mpsc::Receiver<Vec<i64>>,
        mpsc::Receiver<String>,
    ) {
        let (announced, stopped) = mpsc::channel();
        let (printed, output) = mpsc::channel();
        let session = session_logging(
            entry_sources(),
            HashMap::from([(ENTRY, HashMap::from([(1, trigger_for(1, spec))]))]),
            false,
            move |session, info| {
                let _ = announced.send(info.hit_breakpoint_ids.clone());
                session.resume();
            },
            move |text| {
                let _ = printed.send(text);
            },
        );
        (session, stopped, output)
    }

    #[test]
    fn a_condition_that_is_false_does_not_stop() {
        let (session, stopped, _output) = conditional_session(BreakpointSpec {
            line: 1,
            condition: Some("false".to_string()),
            ..BreakpointSpec::default()
        });
        session.safepoint(ENTRY, 0, 0, 0);
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "a false condition stopped the debuggee"
        );
    }

    #[test]
    fn a_condition_that_cannot_be_evaluated_stops_and_says_why() {
        // `nope` is not a local of this frame. Failing open is the decision:
        // a condition that silently switched the breakpoint off would be
        // invisible to the user.
        let (session, stopped, output) = conditional_session(BreakpointSpec {
            line: 1,
            condition: Some("nope > 1".to_string()),
            ..BreakpointSpec::default()
        });
        session.safepoint(ENTRY, 0, 0, 0);
        assert!(
            stopped.recv_timeout(RELEASED).is_ok(),
            "a condition that could not be evaluated turned the breakpoint off"
        );
        let complaint = output.recv_timeout(RELEASED).expect("an output line");
        assert!(complaint.contains("nope"), "{complaint}");
    }

    #[test]
    fn a_logpoint_prints_and_never_stops() {
        let (session, stopped, output) = conditional_session(BreakpointSpec {
            line: 1,
            log_message: Some("hit {1 + 1}".to_string()),
            ..BreakpointSpec::default()
        });
        session.safepoint(ENTRY, 0, 0, 0);
        assert_eq!(
            output.recv_timeout(RELEASED).ok(),
            Some("hit 2".to_string())
        );
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "a logpoint stopped the debuggee"
        );
    }

    #[test]
    fn a_hit_condition_skips_the_hits_it_excludes() {
        let (session, stopped, _output) = conditional_session(BreakpointSpec {
            line: 1,
            hit_condition: Some(">=2".to_string()),
            ..BreakpointSpec::default()
        });
        // Three arrivals on line 1; only the second and third qualify. The
        // offsets go backwards so each counts as a fresh arrival.
        for offset in [0, 0, 0] {
            session.safepoint(ENTRY, offset, 0, 0);
        }
        assert!(stopped.recv_timeout(RELEASED).is_ok(), "the second hit");
        assert!(stopped.recv_timeout(RELEASED).is_ok(), "the third hit");
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "more stops than there were hits"
        );
    }

    #[test]
    fn removing_a_breakpoint_forgets_its_hit_count() {
        let spec = BreakpointSpec {
            line: 1,
            hit_condition: Some("2".to_string()),
            ..BreakpointSpec::default()
        };
        let (session, stopped, _output) = conditional_session(spec.clone());
        session.safepoint(ENTRY, 0, 0, 0);
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "the first hit stopped on a `hitCondition` of 2"
        );

        // The client clears the file, then sets the same breakpoint again:
        // a new id, so the count starts over and the *next* hit is the first.
        session.set_breakpoints(HashMap::new());
        let fresh = trigger_for(1, spec);
        session.set_breakpoints(HashMap::from([(ENTRY, HashMap::from([(1, fresh)]))]));
        session.safepoint(ENTRY, 0, 0, 0);
        assert!(
            stopped.recv_timeout(PARKED).is_err(),
            "the re-set breakpoint kept the old count"
        );
        session.safepoint(ENTRY, 0, 0, 0);
        assert!(stopped.recv_timeout(RELEASED).is_ok(), "the second hit");
    }

    #[test]
    fn a_resume_sent_while_running_does_not_swallow_the_next_stop() {
        let (announced, stopped) = mpsc::channel();
        let session = session_with(move |_: &NativeDebugSession| {
            let _ = announced.send(());
        });

        // No stop is in flight, so this must be a no-op rather than a credit
        // the next stop can spend.
        session.resume();

        let done = spawn_safepoint(&session);
        stopped.recv_timeout(RELEASED).expect("stopped event fired");
        assert!(
            done.recv_timeout(PARKED).is_err(),
            "a stray resume let the debuggee run past the next stop"
        );

        session.resume();
        assert!(
            done.recv_timeout(RELEASED).is_ok(),
            "debuggee stayed parked after resume"
        );
    }

    /// Panic on another thread while holding `mutex`, exactly as a panicking
    /// handler (or a panicking debuggee hook) leaves a lock behind.
    fn poison<T: Send + 'static>(
        session: &Arc<NativeDebugSession>,
        mutex: fn(&NativeDebugSession) -> &Mutex<T>,
        write: impl FnOnce(&mut T) + Send + 'static,
    ) {
        let held = Arc::clone(session);
        let outcome = std::thread::spawn(move || {
            let mut guard = mutex(&held).lock().expect("a fresh mutex is not poisoned");
            write(&mut guard);
            panic!("deliberate panic: poisoning a debug-session lock");
        })
        .join();
        assert!(outcome.is_err(), "the thread was supposed to panic");
    }

    #[test]
    fn a_poisoned_snapshot_lock_is_recovered_rather_than_panicking() {
        let session = session_with(|_: &NativeDebugSession| {});
        poison(
            &session,
            |s| &s.snapshot,
            |snapshot| {
                snapshot.push(FrameSnapshot {
                    name: "half a stop".to_string(),
                    line: 1,
                    path: None,
                    vars: Vec::new(),
                });
            },
        );

        // The call the adapter makes to answer `stackTrace`: it used to
        // `expect` here and take the whole adapter down with it.
        assert_eq!(session.frames().len(), 1, "the recovered snapshot was lost");
        assert!(
            session.poisoned(),
            "taking a poisoned lock was not recorded, so the session would run on"
        );
    }

    #[test]
    fn a_poisoned_session_stops_announcing_stops() {
        let (announced, stopped) = mpsc::channel();
        let session = session_with(move |_: &NativeDebugSession| {
            let _ = announced.send(());
        });
        // A panic while the shadow stack was being pushed: the top frame is
        // there but its line was never filled in.
        poison(
            &session,
            |s| &s.stack,
            |stack| {
                stack.push(Frame {
                    desc: 0,
                    values: 0,
                    line: 0,
                    source: ENTRY,
                });
            },
        );

        // This session stops on entry, so an untouched one would park here
        // until a client resumed it.
        let done = spawn_safepoint(&session);
        assert!(
            done.recv_timeout(RELEASED).is_ok(),
            "a poisoned session parked the debuggee instead of running on"
        );
        assert!(
            stopped.try_recv().is_err(),
            "a poisoned session announced a stop it cannot describe"
        );
        assert!(session.poisoned(), "the poisoned lock went unrecorded");
    }
}
