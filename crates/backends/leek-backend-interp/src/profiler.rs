//! Stack-aware ops profiler.
//!
//! The interpreter calls [`Profiler::enter`] when a user function
//! starts and [`Profiler::exit`] when it returns. Each call
//! snapshots the global `op_count`, and on exit the difference is
//! attributed to the **call stack** (the chain of function names
//! from `<main>` down to the leaf).
//!
//! "Self ops" = total ops in this call minus the total ops
//! attributed to child calls — i.e. the work the function itself
//! did, not counting time spent in callees. The standard
//! flamegraph-folded format uses self-ops per stack frame, which
//! is what [`Profiler::folded_lines`] emits.
//!
//! No work happens per-op — only at function boundaries — so the
//! profiler is cheap regardless of the program's op count.

use std::collections::HashMap;

/// One stack frame's bookkeeping while it's active. Popped when
/// the call returns; its contribution to `samples` is appended at
/// that point.
#[derive(Debug)]
struct Frame {
    name: String,
    ops_at_entry: u64,
    /// Total ops spent in child calls (running tally as each one
    /// exits). On exit, `self_ops = (ops_at_exit - ops_at_entry)
    /// - child_total_ops`.
    child_total_ops: u64,
}

/// Records per-stack op samples for a single run.
#[derive(Default, Debug)]
pub struct Profiler {
    /// Active call stack — top is the currently-executing function.
    stack: Vec<Frame>,
    /// Aggregate self-ops per fully-qualified call stack. Keys are
    /// `["main", "outer", "inner"]`-style paths. Multiple
    /// invocations of the same call stack accumulate. This is the
    /// folded-flame format's natural shape.
    samples: HashMap<Vec<String>, u64>,
}

impl Profiler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new frame. The interpreter calls this *before* the
    /// function body starts running, so `ops_now` is the global
    /// counter at the call's first op.
    pub fn enter(&mut self, name: String, ops_now: u64) {
        self.stack.push(Frame {
            name,
            ops_at_entry: ops_now,
            child_total_ops: 0,
        });
    }

    /// Pop the topmost frame, computing its self-ops and appending
    /// to `samples` under the current stack path. Tells the
    /// parent how many ops this child cost.
    pub fn exit(&mut self, ops_now: u64) {
        let Some(frame) = self.stack.pop() else {
            return;
        };
        let total = ops_now.saturating_sub(frame.ops_at_entry);
        let self_ops = total.saturating_sub(frame.child_total_ops);
        let path: Vec<String> = self
            .stack
            .iter()
            .map(|f| f.name.clone())
            .chain(std::iter::once(frame.name.clone()))
            .collect();
        *self.samples.entry(path).or_insert(0) += self_ops;
        if let Some(parent) = self.stack.last_mut() {
            parent.child_total_ops = parent.child_total_ops.saturating_add(total);
        }
    }

    /// Borrow the aggregate samples map. Caller is responsible for
    /// any flattening / sorting needed for display.
    pub fn samples(&self) -> &HashMap<Vec<String>, u64> {
        &self.samples
    }

    /// Emit the standard flamegraph "folded stacks" format —
    /// one line per stack: `frame1;frame2;...;leaf N`. Pipes
    /// directly to `flamegraph.pl` or `inferno-flamegraph`.
    pub fn folded_lines(&self) -> Vec<String> {
        let mut entries: Vec<(Vec<String>, u64)> = self
            .samples
            .iter()
            .filter(|&(_, v)| *v > 0)
            .map(|(k, &v)| (k.clone(), v))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
            .into_iter()
            .map(|(stack, ops)| format!("{} {}", stack.join(";"), ops))
            .collect()
    }
}
