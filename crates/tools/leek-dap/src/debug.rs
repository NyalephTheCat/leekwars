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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, RwLock};

use leek_span::LineTable;

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

/// A frame captured at a stop, ready to serve `stackTrace`/`variables`.
#[derive(Clone)]
pub(crate) struct FrameSnapshot {
    pub name: String,
    pub line: u32,
    /// Path of the file the frame is executing in — an included file when the
    /// program was compiled through the project include graph. `None` for a
    /// source the adapter has no text for (a synthetic span).
    pub path: Option<String>,
    pub vars: Vec<(String, String)>,
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
}

/// One native debug session. Implements [`leek_backend_native::DebugHook`];
/// install it with `leek_backend_native::set_debug_hook` before running an
/// instrumented program.
pub(crate) struct NativeDebugSession {
    /// Line tables and paths, keyed by raw `SourceId`.
    sources: HashMap<u32, DebugSource>,
    /// Breakpoint id by line, per raw `SourceId`, so a breakpoint in an
    /// included file is matched against that file's own lines. Behind a lock
    /// and replaced wholesale by [`set_breakpoints`]: the client adds and
    /// removes breakpoints at any time, including while the debuggee is
    /// parked at a stop.
    ///
    /// [`set_breakpoints`]: NativeDebugSession::set_breakpoints
    breakpoints: RwLock<HashMap<u32, HashMap<u32, i64>>>,
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
    /// Sends the DAP `stopped` event. Called on the debuggee thread.
    on_stop: Box<dyn Fn(StopInfo) + Send + Sync>,
}

impl NativeDebugSession {
    pub(crate) fn new(
        sources: HashMap<u32, DebugSource>,
        breakpoints: HashMap<u32, HashMap<u32, i64>>,
        stop_on_entry: bool,
        on_stop: Box<dyn Fn(StopInfo) + Send + Sync>,
    ) -> Self {
        Self {
            sources,
            breakpoints: RwLock::new(breakpoints),
            stack: Mutex::new(Vec::new()),
            snapshot: Mutex::new(Vec::new()),
            prev: Mutex::new(None),
            stop_next: AtomicBool::new(stop_on_entry),
            entry_stop: AtomicBool::new(stop_on_entry),
            step: Mutex::new(Step::None),
            wait: Mutex::new(Wait { stopped: false }),
            cv: Condvar::new(),
            on_stop,
        }
    }

    /// Replace the whole breakpoint set, keyed by raw `SourceId` then line.
    ///
    /// Whole-set replacement, not add/remove: the session owns the client's
    /// breakpoints and always hands over the complete picture, so the two
    /// cannot drift apart. Safe to call at any point in the run — while the
    /// debuggee is running, and while it is parked at a stop.
    pub(crate) fn set_breakpoints(&self, by_source: HashMap<u32, HashMap<u32, i64>>) {
        *self.breakpoints.write().expect("breakpoint lock poisoned") = by_source;
    }

    /// The id of the breakpoint on this line, if the client set one.
    ///
    /// Takes the read lock and drops it before returning: the caller goes on
    /// to park the debuggee, and a lock held across that park would block the
    /// very `setBreakpoints` that could free it.
    fn breakpoint_at(&self, source: u32, line: u32) -> Option<i64> {
        self.breakpoints
            .read()
            .expect("breakpoint lock poisoned")
            .get(&source)?
            .get(&line)
            .copied()
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
        let mut prev = self.prev.lock().expect("prev site lock poisoned");
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
        self.snapshot
            .lock()
            .expect("snapshot lock poisoned")
            .clone()
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

    /// Release the stop the debuggee is currently in — whether it has already
    /// parked or is still between claiming the stop and parking. A `wake` with
    /// no stop in flight is a no-op, so a stray `continue` cannot swallow a
    /// later breakpoint.
    fn wake(&self) {
        let mut wait = self.wait.lock().expect("debug wait lock poisoned");
        wait.stopped = false;
        self.cv.notify_all();
    }

    /// Capture every live frame's name/line/locals (debuggee thread, frames
    /// alive), publish the snapshot, then park until resumed.
    fn stop(&self, line: u32, reason: StopReason, hit_breakpoint_ids: Vec<i64>) {
        // Clone the frame pointers out under the lock, then render outside it.
        let frames: Vec<(usize, usize, u32, u32)> = {
            let stack = self.stack.lock().expect("stack lock poisoned");
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
                vars: leek_backend_native::render_frame_vars(desc, values),
            })
            .collect();
        *self.snapshot.lock().expect("snapshot lock poisoned") = snapshot;

        // Claim the stop before announcing it. The client may answer the
        // `stopped` event with `continue` (or a step) before this thread
        // reaches the condvar; that `wake` clears the flag we just set, so the
        // loop below sees the resume instead of losing it.
        self.wait.lock().expect("debug wait lock poisoned").stopped = true;

        (self.on_stop)(StopInfo {
            line,
            reason,
            hit_breakpoint_ids,
        });

        let mut wait = self.wait.lock().expect("debug wait lock poisoned");
        while wait.stopped {
            wait = self.cv.wait(wait).expect("debug wait lock poisoned");
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
            let mut stack = self.stack.lock().expect("stack lock poisoned");
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
        } else if let Some(id) = arrived.then(|| self.breakpoint_at(source, line)).flatten() {
            (StopReason::Breakpoint, vec![id])
        } else {
            return;
        };

        self.stop(line, reason, hit_breakpoint_ids);
    }

    fn enter_frame(&self, frame_desc: usize) {
        self.stack.lock().expect("stack lock poisoned").push(Frame {
            desc: frame_desc,
            values: 0,
            line: 0,
            source: 0,
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
            *self.step.lock().expect("step lock poisoned") = Step::Into {
                line: 0,
                depth: len,
            };
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::sync::{Arc, OnceLock, Weak};
    use std::time::Duration;

    use leek_backend_native::DebugHook;

    use leek_span::LineTable;

    use super::{DebugSource, NativeDebugSession, StopInfo};

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
        breakpoints: HashMap<u32, HashMap<u32, i64>>,
        stop_on_entry: bool,
        on_stop: impl Fn(&NativeDebugSession, &StopInfo) + Send + Sync + 'static,
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
                (ENTRY, HashMap::from([(2, 1)])),
                (LIB, HashMap::from([(3, 2)])),
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
            HashMap::from([(ENTRY, HashMap::from([(1, 1)]))]),
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
            HashMap::from([(ENTRY, HashMap::from([(line, id)]))]),
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

        session.set_breakpoints(HashMap::from([(ENTRY, HashMap::from([(2, 5)]))]));
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
        session.set_breakpoints(HashMap::from([(ENTRY, HashMap::from([(1, 3)]))]));

        session.resume();
        assert!(
            done.recv_timeout(RELEASED).is_ok(),
            "setting breakpoints at a stop wedged the debuggee"
        );
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
}
