//! A scoped-thread work pool for work addressed by index.
//!
//! One shape, used by everything in this workspace that has a long list of
//! independent jobs to run: the upstream corpus suite (#149) and the fight
//! sweep drivers (#134). Workers claim batches of indices from a shared atomic
//! cursor, and the results are merged **by index, never by completion order**
//! — which is the whole point. Both callers write their output to disk and
//! diff it against a baseline, so a report that depended on which worker
//! finished first would be worse than no parallelism at all.
//!
//! The pool is deliberately tiny and dependency-free: no runtime, no global
//! thread pool, nothing to configure but the worker count. [`Pool::map`]
//! borrows everything it touches out of the caller's frame through
//! [`std::thread::scope`], so there is no `'static` bound and no `Arc`
//! ceremony at the call sites.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Stack for one worker.
///
/// Both callers compile user-authored code on their workers, and a deeply
/// nested program overflows the ~2 MiB a thread gets by default — which aborts
/// the *process* (`fatal runtime error: stack overflow`) rather than failing
/// one job, so a sweep would die naming nothing.
pub const WORKER_STACK: usize = 64 * 1024 * 1024;

/// Cap on the worker count [`default_jobs`] picks.
///
/// Peak memory scales with the workers (each holds live JIT modules and its
/// own bump arenas) and the work is compute-bound, so beyond this the trade is
/// memory for very little. An explicit count is never capped.
pub const MAX_DEFAULT_JOBS: usize = 8;

/// `env_var` when it is set to a positive number, else this machine's
/// parallelism capped at [`MAX_DEFAULT_JOBS`].
///
/// Each caller passes its own variable name (`LEEK_CORPUS_JOBS`,
/// `LEEK_FIGHT_JOBS`) so the two can be turned down independently — on a
/// small CI runner, say — without one of them having to guess for the other.
#[must_use]
pub fn default_jobs(env_var: &str) -> usize {
    parse_jobs(
        std::env::var_os(env_var)
            .as_deref()
            .and_then(std::ffi::OsStr::to_str),
    )
    .unwrap_or_else(machine_jobs)
}

/// The worker count `raw` asks for, or `None` when it asks for nothing usable
/// — unset, empty, not a number, or zero. A malformed override falls back to
/// the machine rather than running no work at all.
fn parse_jobs(raw: Option<&str>) -> Option<usize> {
    raw?.trim().parse::<usize>().ok().filter(|&n| n >= 1)
}

fn machine_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_DEFAULT_JOBS)
}

/// How to run one batch of index-addressed work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pool {
    jobs: usize,
    batch: usize,
    name: &'static str,
}

impl Pool {
    /// A pool of `jobs` workers named `name-0`, `name-1`, … (the name shows up
    /// in a panic message and in a debugger, so it is worth spending).
    ///
    /// `jobs = 1` is not a special case: it takes the same code path as
    /// `jobs = 8`, so a serial measurement — or a serial run taken for
    /// determinism — exercises the shipped pool rather than a second one.
    #[must_use]
    pub fn new(name: &'static str, jobs: usize) -> Self {
        Self {
            jobs: jobs.max(1),
            batch: 1,
            name,
        }
    }

    /// Indices a worker claims per `fetch_add`. The default of 1 suits jobs
    /// that cost milliseconds and up (a fight); raise it when a job is cheap
    /// enough that the cursor itself would be the bottleneck (a corpus case).
    ///
    /// Batching trades load balance for contention, so it is the caller's
    /// call: a static split would be worse than either, since per-job cost
    /// here spans orders of magnitude and neighbouring jobs cost alike.
    #[must_use]
    pub fn with_batch(mut self, batch: usize) -> Self {
        self.batch = batch.max(1);
        self
    }

    /// Apply `f` to every index in `indices` across the pool's workers, and
    /// return the `Some` results **in `indices` order**, each paired with the
    /// index it came from.
    ///
    /// `init` builds whatever one worker needs to reuse across its jobs (a
    /// compiler pipeline, say) once per *worker* rather than once per job, so
    /// `W` does not have to be `Send`: it never leaves the thread that made
    /// it.
    ///
    /// # Panics
    /// A panic in `f` is re-raised on the calling thread once the other
    /// workers have finished. Swallowing it would hand back a quietly thinner
    /// result — which, for a caller that diffs against a baseline, is the one
    /// failure mode worse than a crash.
    pub fn map<T, W>(
        self,
        indices: &[usize],
        init: impl Fn() -> W + Sync,
        f: impl Fn(usize, &W) -> Option<T> + Sync,
    ) -> Vec<(usize, T)>
    where
        T: Send,
    {
        let jobs = self.jobs.min(indices.len().max(1));
        let cursor = AtomicUsize::new(0);
        let (init, f, cursor) = (&init, &f, &cursor);

        let mut parts: Vec<Vec<(usize, T)>> = Vec::with_capacity(jobs);
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..jobs)
                .map(|w| {
                    std::thread::Builder::new()
                        .name(format!("{}-{w}", self.name))
                        .stack_size(WORKER_STACK)
                        .spawn_scoped(scope, move || {
                            let state = init();
                            let mut out = Vec::new();
                            loop {
                                let start = cursor.fetch_add(self.batch, Ordering::Relaxed);
                                if start >= indices.len() {
                                    break;
                                }
                                let end = (start + self.batch).min(indices.len());
                                for &i in &indices[start..end] {
                                    if let Some(value) = f(i, &state) {
                                        out.push((i, value));
                                    }
                                }
                            }
                            out
                        })
                        .expect("spawn worker")
                })
                .collect();
            for worker in workers {
                match worker.join() {
                    Ok(part) => parts.push(part),
                    Err(payload) => std::panic::resume_unwind(payload),
                }
            }
        });

        let mut out: Vec<(usize, T)> = parts.into_iter().flatten().collect();
        out.sort_by_key(|(i, _)| *i);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_come_back_in_index_order_whatever_the_worker_count() {
        // Reversed cost: the last index is the slowest, so a completion-order
        // merge would put it first.
        let indices: Vec<usize> = (0..64).collect();
        let run = |jobs| {
            Pool::new("test", jobs).map(
                &indices,
                || (),
                |i, ()| {
                    for _ in 0..(64 - i) * 200 {
                        std::hint::black_box(i);
                    }
                    Some(i * 2)
                },
            )
        };
        let expected: Vec<(usize, usize)> = indices.iter().map(|&i| (i, i * 2)).collect();
        assert_eq!(run(1), expected);
        assert_eq!(run(8), expected);
    }

    #[test]
    fn a_skipped_job_leaves_no_hole_and_keeps_its_index() {
        let indices: Vec<usize> = (0..20).collect();
        let out = Pool::new("test", 4).map(&indices, || (), |i, ()| (i % 3 == 0).then_some(i));
        assert_eq!(
            out,
            (0..20)
                .filter(|i| i % 3 == 0)
                .map(|i| (i, i))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn only_the_listed_indices_are_visited() {
        let indices = [7usize, 2, 9];
        let out = Pool::new("test", 3).map(&indices, || (), |i, ()| Some(i));
        // Sorted by index, not by position in `indices`.
        assert_eq!(out, vec![(2, 2), (7, 7), (9, 9)]);
    }

    #[test]
    fn an_empty_job_list_spawns_a_worker_that_finds_nothing() {
        let out: Vec<(usize, usize)> = Pool::new("test", 8).map(&[], || (), |i, ()| Some(i));
        assert!(out.is_empty());
    }

    #[test]
    #[should_panic(expected = "job 13 blew up")]
    fn a_worker_panic_is_re_raised_rather_than_swallowed() {
        let indices: Vec<usize> = (0..32).collect();
        let _ = Pool::new("test", 4).map(
            &indices,
            || (),
            |i, ()| {
                assert!(i != 13, "job 13 blew up");
                Some(i)
            },
        );
    }

    #[test]
    fn a_batch_larger_than_the_job_list_still_runs_every_job() {
        let indices: Vec<usize> = (0..5).collect();
        let out = Pool::new("test", 4)
            .with_batch(64)
            .map(&indices, || (), |i, ()| Some(i));
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn zero_is_read_as_one_rather_than_as_no_workers() {
        let indices: Vec<usize> = (0..4).collect();
        let out = Pool::new("test", 0)
            .with_batch(0)
            .map(&indices, || (), |i, ()| Some(i));
        assert_eq!(out.len(), 4, "jobs = 0 must not run zero jobs");
    }

    #[test]
    fn a_worker_count_is_read_from_the_override_only_when_it_asks_for_one() {
        assert_eq!(parse_jobs(Some(" 3 ")), Some(3));
        assert_eq!(parse_jobs(Some("1")), Some(1));
        assert_eq!(parse_jobs(None), None);
        assert_eq!(parse_jobs(Some("")), None);
        assert_eq!(
            parse_jobs(Some("0")),
            None,
            "0 must fall back, not run nothing"
        );
        assert_eq!(parse_jobs(Some("-2")), None);
        assert_eq!(parse_jobs(Some("eight")), None);
        assert!(machine_jobs() >= 1 && machine_jobs() <= MAX_DEFAULT_JOBS);
    }
}
