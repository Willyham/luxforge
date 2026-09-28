//! The development executor: synchronous, bounded admission to the shared Rayon pool for native
//! demosaic tiles (Bayer RCD and X-Trans one-pass Markesteijn), native normalization batches and
//! the DNG correction rows.

use std::{
    ffi::{c_int, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

// One-pass Markesteijn allocates 988,208 scratch bytes per running callback
// and RCD 978,536. One development runs at most eight callbacks at once, so
// its explicit scratch is at most 7,905,664 bytes; there is no full-frame
// allocation per callback. The source worker runs one development at a time.
const MAX_LANES: usize = 8;

pub(super) struct ExecutorContext<'a> {
    pub cancel: &'a AtomicBool,
    /// Zero chooses the available shared-pool width; nonzero is an exactness
    /// test override. The eight-lane cap still applies.
    pub worker_limit: usize,
}

pub(super) type TileWorker = extern "C" fn(*mut c_void, usize);
pub(super) type TileExecutor = extern "C" fn(*mut c_void, usize, TileWorker, *mut c_void) -> c_int;

/// How many native jobs run at once for `worker_limit`: the shared pool's width, or a nonzero
/// exactness test override no wider than the pool, and never more than [`MAX_LANES`].
fn native_lanes(worker_limit: usize) -> usize {
    let width = rayon::current_num_threads();
    match worker_limit {
        0 => width,
        limit => limit.min(width),
    }
    .clamp(1, MAX_LANES)
}

/// Run `run(job)` once for each job in `0..jobs`, at most `lanes` at once, until a call returns
/// `false`; no job starts once the executor has seen that. Returns when every started job has
/// returned.
///
/// With one lane the caller runs every job in order. Otherwise the caller runs the final job
/// first, since a native demosaic's final job carries the frame's longer tail, then pulls ordinary
/// jobs from a shared counter until none remain. Each of the other `lanes - 1` lanes is one task in
/// the scope that runs one job and then spawns its successor to pull the next. So no job waits
/// for another to finish before it can start, and a pool thread that steals a lane while it waits
/// on its own work takes one job, never a run of them: at most `lanes - 1` jobs are ever open to
/// stealing. `in_place_scope` keeps the caller's share on its own thread, so an external caller's
/// jobs never enter the pool.
pub(crate) fn refill(lanes: usize, jobs: usize, run: impl Fn(usize) -> bool + Sync) {
    let lanes = lanes.min(jobs);
    if lanes <= 1 {
        for job in 0..jobs {
            if !run(job) {
                break;
            }
        }
        return;
    }
    let queue = &Queue {
        next: AtomicUsize::new(0),
        ordinary: jobs - 1,
        stopped: AtomicBool::new(false),
        run: &run,
    };
    rayon::in_place_scope(|scope| {
        for _ in 1..lanes {
            scope.spawn(move |scope| queue.lane(scope));
        }
        if queue.run(jobs - 1) {
            while let Some(job) = queue.pull() {
                if !queue.run(job) {
                    break;
                }
            }
        }
    });
}

/// Run `run` once for each of `jobs` through [`refill`] with `lanes` at once: the development
/// executor for a Rust pass, whose jobs are owned values such as disjoint slices of its planes.
/// One lane runs them in order on the caller. The first error stops the queue and is returned
/// once every started job has joined.
///
/// Every pass on the shared pool during a development uses this, so a pool thread that steals
/// its work while waiting on its own takes one job, never a range of the frame; a job should be
/// no longer than a native tile job. The job list is one entry per job, bounded by the pass's own
/// job size.
pub fn refill_each<T: Send, E: Send>(
    lanes: usize,
    jobs: impl IntoIterator<Item = T>,
    run: impl Fn(T) -> Result<(), E> + Sync,
) -> Result<(), E> {
    let jobs: Vec<Mutex<Option<T>>> = jobs.into_iter().map(|job| Mutex::new(Some(job))).collect();
    let failure = Mutex::new(None);
    refill(lanes, jobs.len(), |index| {
        let job = jobs[index].lock().unwrap().take();
        match run(job.expect("refill runs each job once")) {
            Ok(()) => true,
            Err(error) => {
                failure.lock().unwrap().get_or_insert(error);
                false
            }
        }
    });
    failure.into_inner().unwrap().map_or(Ok(()), Err)
}

/// The ordinary jobs of one [`refill`] call and what stops them.
struct Queue<'a, F> {
    next: AtomicUsize,
    ordinary: usize,
    stopped: AtomicBool,
    run: &'a F,
}

impl<F: Fn(usize) -> bool + Sync> Queue<'_, F> {
    fn pull(&self) -> Option<usize> {
        if self.stopped.load(Ordering::Relaxed) {
            return None;
        }
        let job = self.next.fetch_add(1, Ordering::Relaxed);
        (job < self.ordinary).then_some(job)
    }

    fn run(&self, job: usize) -> bool {
        let go_on = (self.run)(job);
        if !go_on {
            self.stopped.store(true, Ordering::Relaxed);
        }
        go_on
    }

    /// One pool lane: one job, then a successor task for the next.
    fn lane<'s>(&'s self, scope: &rayon::Scope<'s>) {
        if let Some(job) = self.pull()
            && self.run(job)
        {
            scope.spawn(move |scope| self.lane(scope));
        }
    }
}

/// # Safety contract
///
/// C++ supplies a live immutable job context and a no-throw worker entry.
/// Each callback evaluates one C++ job with its own scratch. The jobs run
/// through [`refill`], at most the pool's width and never more than eight at
/// once; the final C++ job, which carries the frame's last tile rows, runs
/// first on the source caller. Cancellation is checked before every callback
/// and stops the queue. The scope joins before borrowed context or image
/// buffers drop. This trampoline catches Rust panics so none crosses the C ABI.
pub(super) extern "C" fn execute(
    context: *mut c_void,
    job_count: usize,
    worker: TileWorker,
    worker_context: *mut c_void,
) -> c_int {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: lf_raw_develop receives this stack context and calls the
        // executor synchronously; neither demosaic stores either pointer.
        let state = unsafe { &*context.cast::<ExecutorContext<'_>>() };
        // Raw pointers are converted to integer addresses solely to satisfy
        // Rayon closure Send bounds. The scope is synchronous and C++ joins
        // before the stack-backed job and buffers can be released.
        let worker_context = worker_context as usize;
        let cancelled = AtomicBool::new(false);
        refill(native_lanes(state.worker_limit), job_count, |job| {
            if state.cancel.load(Ordering::Relaxed) {
                cancelled.store(true, Ordering::Relaxed);
                return false;
            }
            worker(worker_context as *mut c_void, job);
            true
        });
        if cancelled.load(Ordering::Relaxed) {
            2
        } else {
            0
        }
    }));
    result.unwrap_or(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct JobTracker {
        seen: Vec<AtomicUsize>,
        active: AtomicUsize,
        peak: AtomicUsize,
    }

    impl JobTracker {
        fn new(jobs: usize) -> Self {
            Self {
                seen: (0..jobs).map(|_| AtomicUsize::new(0)).collect(),
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            }
        }

        /// Run `execute` over this tracker's jobs under `context`.
        fn execute(&self, context: &ExecutorContext<'_>) -> c_int {
            execute(
                (context as *const ExecutorContext<'_>).cast_mut().cast(),
                self.seen.len(),
                track_one_job,
                (self as *const JobTracker).cast_mut().cast(),
            )
        }

        /// One job's work, modelled as a millisecond of wall time so that jobs overlap. The tests
        /// assert only upper bounds on that overlap, so however long a loaded host makes it take
        /// never decides them.
        fn track(&self, job: usize) {
            self.seen[job].fetch_add(1, Ordering::Relaxed);
            let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
            self.peak.fetch_max(active, Ordering::Relaxed);
            let started = Instant::now();
            luxforge_testbase::wait_until("the job's work", || {
                started.elapsed() >= Duration::from_millis(1)
            });
            self.active.fetch_sub(1, Ordering::Relaxed);
        }

        fn each_job_ran_once(&self) -> bool {
            self.seen
                .iter()
                .all(|seen| seen.load(Ordering::Relaxed) == 1)
        }
    }

    extern "C" fn track_one_job(context: *mut c_void, job: usize) {
        // SAFETY: execute joins every callback before this tracker drops.
        let tracker = unsafe { &*context.cast::<JobTracker>() };
        tracker.track(job);
    }

    struct PlacementTracker {
        caller: std::thread::ThreadId,
        seen: Vec<AtomicUsize>,
        on_caller: Mutex<Vec<usize>>,
        noncaller_outside_pool: AtomicBool,
    }

    extern "C" fn track_placement(context: *mut c_void, job: usize) {
        // SAFETY: execute joins all callbacks before this tracker drops.
        let tracker = unsafe { &*context.cast::<PlacementTracker>() };
        tracker.seen[job].fetch_add(1, Ordering::Relaxed);
        if std::thread::current().id() == tracker.caller {
            tracker.on_caller.lock().unwrap().push(job);
        } else if rayon::current_thread_index().is_none() {
            tracker
                .noncaller_outside_pool
                .store(true, Ordering::Relaxed);
        }
    }

    /// Two developments nested in a two-thread Rayon pool, each already on a
    /// pool thread, both complete: each runs every job once and never more
    /// callbacks at once than its own cap.
    #[test]
    fn nested_develop_executors_complete_in_a_small_rayon_pool() {
        let cancel = AtomicBool::new(false);
        let context = ExecutorContext {
            cancel: &cancel,
            worker_limit: 2,
        };
        let trackers = [JobTracker::new(5), JobTracker::new(5)];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let statuses = pool.install(|| {
            rayon::join(
                || trackers[0].execute(&context),
                || trackers[1].execute(&context),
            )
        });
        assert_eq!(statuses, (0, 0));
        for tracker in &trackers {
            assert!(tracker.each_job_ran_once());
            assert!(tracker.peak.load(Ordering::Relaxed) <= 2);
            assert_eq!(tracker.active.load(Ordering::Relaxed), 0);
        }
    }

    /// Every job runs exactly once and never more than the lane cap at once, for every worker
    /// limit, including more lanes than jobs and a single job.
    #[test]
    fn refill_runs_every_job_once_within_the_lane_cap() {
        let cancel = AtomicBool::new(false);
        let width = rayon::current_num_threads();
        for jobs in [1, 2, 17, 64] {
            for (worker_limit, cap) in [
                (1, 1),
                (2, 2.min(width)),
                (3, 3.min(width)),
                (0, MAX_LANES.min(width)),
                (usize::MAX, MAX_LANES.min(width)),
            ] {
                let context = ExecutorContext {
                    cancel: &cancel,
                    worker_limit,
                };
                let tracker = JobTracker::new(jobs);
                assert_eq!(tracker.execute(&context), 0);
                assert!(tracker.each_job_ran_once(), "{jobs} jobs at {worker_limit}");
                assert!(tracker.peak.load(Ordering::Relaxed) <= cap);
                assert_eq!(tracker.active.load(Ordering::Relaxed), 0);
            }
        }
        // `refill` itself, under a caller's own cap wider than the native one.
        let tracker = JobTracker::new(40);
        refill(3, 40, |job| {
            tracker.track(job);
            true
        });
        assert!(tracker.each_job_ran_once());
        assert!(tracker.peak.load(Ordering::Relaxed) <= 3);
    }

    /// An external caller runs the final job before any other, then pulls ordinary jobs itself;
    /// every job it does not run runs on the pool, never on another thread.
    #[test]
    fn external_caller_runs_the_final_job_first_and_never_leaves_the_pool() {
        assert!(rayon::current_thread_index().is_none());
        let cancel = AtomicBool::new(false);
        for worker_limit in [1, 2, 0] {
            let context = ExecutorContext {
                cancel: &cancel,
                worker_limit,
            };
            let tracker = PlacementTracker {
                caller: std::thread::current().id(),
                seen: (0..33).map(|_| AtomicUsize::new(0)).collect(),
                on_caller: Mutex::new(Vec::new()),
                noncaller_outside_pool: AtomicBool::new(false),
            };
            assert_eq!(
                execute(
                    (&context as *const ExecutorContext<'_>).cast_mut().cast(),
                    tracker.seen.len(),
                    track_placement,
                    (&tracker as *const PlacementTracker).cast_mut().cast(),
                ),
                0
            );
            assert!(
                tracker
                    .seen
                    .iter()
                    .all(|seen| seen.load(Ordering::Relaxed) == 1)
            );
            let on_caller = tracker.on_caller.into_inner().unwrap();
            if native_lanes(worker_limit) == 1 {
                // One lane is the serial raster, in order, on the caller.
                assert_eq!(on_caller, (0..33).collect::<Vec<_>>());
            } else {
                assert_eq!(on_caller.first(), Some(&32));
            }
            assert!(!tracker.noncaller_outside_pool.load(Ordering::Relaxed));
        }
    }

    /// Once a job stops the queue no ordinary job is pulled; the only ones that can still start
    /// are those another lane had already pulled, at most one per lane.
    #[test]
    fn a_stopping_job_ends_the_queue() {
        for lanes in [1, 4] {
            let stopped = AtomicBool::new(false);
            let (runs, late) = (AtomicUsize::new(0), AtomicUsize::new(0));
            refill(lanes, 10_000, |job| {
                if stopped.load(Ordering::Relaxed) {
                    late.fetch_add(1, Ordering::Relaxed);
                }
                runs.fetch_add(1, Ordering::Relaxed);
                if job == 100 {
                    stopped.store(true, Ordering::Relaxed);
                    return false;
                }
                true
            });
            assert!(late.load(Ordering::Relaxed) < lanes);
            assert!(runs.load(Ordering::Relaxed) < 10_000);
        }
        // The native trampoline reports a cancellation seen between jobs.
        let cancel = AtomicBool::new(true);
        let tracker = JobTracker::new(9);
        let context = ExecutorContext {
            cancel: &cancel,
            worker_limit: 0,
        };
        assert_eq!(tracker.execute(&context), 2);
        assert!(
            tracker
                .seen
                .iter()
                .all(|seen| seen.load(Ordering::Relaxed) == 0)
        );
    }
}
