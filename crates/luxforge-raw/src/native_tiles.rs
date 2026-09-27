//! Synchronous admission to the existing Rayon pool for native demosaic tiles
//! (Bayer RCD and X-Trans one-pass Markesteijn).

use std::{
    ffi::{c_int, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicBool, Ordering},
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

/// # Safety contract
///
/// C++ supplies a live immutable job context and a no-throw worker entry.
/// Each callback evaluates one C++ tile job with its own scratch. At most
/// `desired` callbacks, never more than eight, run in a batch; every batch
/// joins before the next is dispatched. The final C++ job, which carries the
/// frame's last tile rows, runs on the source caller, while shorter ordinary
/// jobs enter Rayon. Cancellation is checked before every callback. The final
/// scope joins before borrowed context or image buffers drop.
/// This trampoline catches Rust panics so none crosses the C ABI.
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
        let width = rayon::current_num_threads();
        let desired = job_count
            .min(width)
            .min(if state.worker_limit == 0 {
                width
            } else {
                state.worker_limit
            })
            .clamp(1, MAX_LANES);
        // Raw pointers are converted to integer addresses solely to satisfy
        // Rayon closure Send bounds. The scope is synchronous and C++ joins
        // before the stack-backed job and buffers can be released.
        let worker_context = worker_context as usize;
        let status = std::sync::atomic::AtomicI32::new(0);
        let run_job = |job| {
            if status.load(Ordering::Relaxed) != 0 {
                return;
            }
            if state.cancel.load(Ordering::Relaxed) {
                status.store(2, Ordering::Relaxed);
                return;
            }
            worker(worker_context as *mut c_void, job);
        };
        if desired == 1 {
            for job in 0..job_count {
                run_job(job);
                if status.load(Ordering::Relaxed) != 0 {
                    break;
                }
            }
            return status.load(Ordering::Relaxed);
        }
        // The last C++ job retains the final rows' scratch history. Run
        // it on the source caller while the first ordinary jobs use the pool.
        let ordinary_jobs = job_count - 1;
        let first_end = ordinary_jobs.min(desired - 1);
        rayon::in_place_scope(|scope| {
            for job in 0..first_end {
                let run_job = &run_job;
                scope.spawn(move |_| run_job(job));
            }
            run_job(job_count - 1);
        });
        for first in (first_end..ordinary_jobs).step_by(desired) {
            if status.load(Ordering::Relaxed) != 0 {
                break;
            }
            let end = (first + desired).min(ordinary_jobs);
            // Keep the batch coordinator on its calling thread. `scope` may
            // inject the whole closure into Rayon when called externally, so
            // a preview worker could steal its native work and joined wait.
            rayon::in_place_scope(|scope| {
                for job in first..end {
                    let run_job = &run_job;
                    scope.spawn(move |_| run_job(job));
                }
            });
        }
        status.load(Ordering::Relaxed)
    }));
    result.unwrap_or(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    struct JobTracker {
        seen: Vec<std::sync::atomic::AtomicUsize>,
        active: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
    }

    impl JobTracker {
        fn new(jobs: usize) -> Self {
            Self {
                seen: (0..jobs)
                    .map(|_| std::sync::atomic::AtomicUsize::new(0))
                    .collect(),
                active: std::sync::atomic::AtomicUsize::new(0),
                peak: std::sync::atomic::AtomicUsize::new(0),
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

        fn each_job_ran_once(&self) -> bool {
            self.seen
                .iter()
                .all(|seen| seen.load(Ordering::Relaxed) == 1)
        }
    }

    extern "C" fn track_one_job(context: *mut c_void, job: usize) {
        // SAFETY: execute joins every callback before this tracker drops.
        let tracker = unsafe { &*context.cast::<JobTracker>() };
        tracker.seen[job].fetch_add(1, Ordering::Relaxed);
        let active = tracker.active.fetch_add(1, Ordering::Relaxed) + 1;
        tracker.peak.fetch_max(active, Ordering::Relaxed);
        // The tile's work, modelled as a millisecond of wall time so that jobs overlap. The
        // tests assert only upper bounds on that overlap, so however long a loaded host makes it
        // take never decides them.
        let started = Instant::now();
        luxforge_testbase::wait_until("the job's work", || {
            started.elapsed() >= Duration::from_millis(1)
        });
        tracker.active.fetch_sub(1, Ordering::Relaxed);
    }

    struct PlacementTracker {
        caller: std::thread::ThreadId,
        seen: Vec<std::sync::atomic::AtomicUsize>,
        on_caller: Vec<AtomicBool>,
        noncaller_outside_pool: AtomicBool,
    }

    extern "C" fn track_placement(context: *mut c_void, group: usize) {
        // SAFETY: execute joins all callbacks before this tracker drops.
        let tracker = unsafe { &*context.cast::<PlacementTracker>() };
        tracker.seen[group].fetch_add(1, Ordering::Relaxed);
        if std::thread::current().id() == tracker.caller {
            tracker.on_caller[group].store(true, Ordering::Relaxed);
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

    #[test]
    fn one_callback_per_tile_job_with_bounded_batches() {
        let cancel = AtomicBool::new(false);
        for (worker_limit, cap) in [(3, 3), (0, MAX_LANES), (usize::MAX, MAX_LANES)] {
            let context = ExecutorContext {
                cancel: &cancel,
                worker_limit,
            };
            let tracker = JobTracker::new(17);
            assert_eq!(tracker.execute(&context), 0);
            assert!(tracker.each_job_ran_once());
            assert!(tracker.peak.load(Ordering::Relaxed) <= cap);
            assert_eq!(tracker.active.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn external_caller_runs_only_final_job_with_bounded_native_batches() {
        assert!(rayon::current_thread_index().is_none());
        let cancel = AtomicBool::new(false);
        let context = ExecutorContext {
            cancel: &cancel,
            worker_limit: 2,
        };
        let tracker = PlacementTracker {
            caller: std::thread::current().id(),
            seen: (0..5)
                .map(|_| std::sync::atomic::AtomicUsize::new(0))
                .collect(),
            on_caller: (0..5).map(|_| AtomicBool::new(false)).collect(),
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
        assert!(tracker.on_caller[4].load(Ordering::Relaxed));
        assert!(
            tracker.on_caller[..4]
                .iter()
                .all(|on_caller| !on_caller.load(Ordering::Relaxed))
        );
        assert!(!tracker.noncaller_outside_pool.load(Ordering::Relaxed));
    }
}
