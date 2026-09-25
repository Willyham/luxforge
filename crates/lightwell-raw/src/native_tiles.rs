//! Synchronous admission to the existing Rayon pool for native X-Trans tiles.

use std::{
    ffi::{c_int, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

// One-pass Markesteijn allocates 988,208 scratch bytes per admitted slot.
// Eight slots across *all* RawSource callers add at most 7,905,664 explicit
// scratch bytes; there is no full-frame allocation per slot.
const MAX_SCRATCH_SLOTS: usize = 8;
// Leave capacity in the shared pool for concurrent preview rendering. Other
// RawSource callers may use the remaining process-wide scratch slots.
const MAX_WORKERS_PER_SOURCE: usize = 4;
static SLOTS: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
#[cfg(test)]
static PEAK_SLOTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub(super) struct ExecutorContext<'a> {
    pub cancel: &'a AtomicBool,
    /// Zero uses the production per-source cap; nonzero is an exactness-test
    /// override up to the process-wide scratch cap.
    pub worker_limit: usize,
}

pub(super) type TileWorker = extern "C" fn(*mut c_void, usize);
pub(super) type TileExecutor = extern "C" fn(*mut c_void, usize, TileWorker, *mut c_void) -> c_int;

struct ScratchPermit {
    count: usize,
    slots: &'static (Mutex<usize>, Condvar),
}

impl Drop for ScratchPermit {
    fn drop(&mut self) {
        let mut active = self.slots.0.lock().unwrap_or_else(|e| e.into_inner());
        *active -= self.count;
        self.slots.1.notify_all();
    }
}

fn admit(desired: usize, cancel: &AtomicBool) -> Result<ScratchPermit, c_int> {
    let slots = SLOTS.get_or_init(|| (Mutex::new(0), Condvar::new()));
    let mut active = slots.0.lock().unwrap_or_else(|e| e.into_inner());
    while *active == MAX_SCRATCH_SLOTS {
        if cancel.load(Ordering::Relaxed) {
            return Err(2);
        }
        if rayon::current_thread_index().is_some() {
            // A Rayon caller must help run queued jobs. Blocking every pool
            // thread here can starve the permit owner's scoped workers.
            drop(active);
            if matches!(rayon::yield_now(), Some(rayon::Yield::Idle)) {
                // No current work to help with; avoid a hot spin while an
                // external caller owns the slots, but retry promptly.
                std::thread::park_timeout(Duration::from_millis(1));
            }
            active = slots.0.lock().unwrap_or_else(|e| e.into_inner());
        } else {
            active = slots
                .1
                .wait_timeout(active, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(2);
    }
    let count = desired.min(MAX_SCRATCH_SLOTS - *active);
    *active += count;
    #[cfg(test)]
    PEAK_SLOTS.fetch_max(*active, Ordering::Relaxed);
    Ok(ScratchPermit { count, slots })
}

/// # Safety contract
///
/// C++ supplies a live immutable job context and a no-throw worker entry.
/// Each callback evaluates one C++ row group with its own scratch. At most
/// `desired` callbacks are queued in a batch; every batch joins before the
/// next is dispatched. A scratch permit is held only while a native callback
/// executes, never by a Rayon scope waiting for children. The final scope
/// joins before borrowed context or image buffers drop.
/// This trampoline catches Rust panics so none crosses the C ABI.
pub(super) extern "C" fn execute(
    context: *mut c_void,
    job_count: usize,
    worker: TileWorker,
    worker_context: *mut c_void,
) -> c_int {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: lw_raw_develop receives this stack context and calls the
        // executor synchronously; Markesteijn stores neither pointer.
        let state = unsafe { &*context.cast::<ExecutorContext<'_>>() };
        let width = rayon::current_num_threads();
        let source_limit = if state.worker_limit == 0 {
            MAX_WORKERS_PER_SOURCE
        } else {
            state.worker_limit
        };
        let desired = job_count
            .min(width)
            .min(source_limit)
            .clamp(1, MAX_SCRATCH_SLOTS);
        // Raw pointers are converted to integer addresses solely to satisfy
        // Rayon closure Send bounds. The scope is synchronous and C++ joins
        // before the stack-backed job and buffers can be released.
        let worker_context = worker_context as usize;
        let status = std::sync::atomic::AtomicI32::new(0);
        let run_job = |job| {
            if status.load(Ordering::Relaxed) != 0 {
                return;
            }
            match admit(1, state.cancel) {
                Ok(_permit) => worker(worker_context as *mut c_void, job),
                Err(code) => status.store(code, Ordering::Relaxed),
            }
        };
        for first in (0..job_count).step_by(desired) {
            if status.load(Ordering::Relaxed) != 0 {
                break;
            }
            let end = (first + desired).min(job_count);
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
    static TEST_BUDGET_LOCK: Mutex<()> = Mutex::new(());

    extern "C" fn count_worker(context: *mut c_void, _slot: usize) {
        // SAFETY: both execute calls join before this local counter drops.
        let count = unsafe { &*context.cast::<std::sync::atomic::AtomicUsize>() };
        count.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(2));
    }

    struct JobTracker {
        seen: Vec<std::sync::atomic::AtomicUsize>,
        active: std::sync::atomic::AtomicUsize,
        peak: std::sync::atomic::AtomicUsize,
    }

    extern "C" fn track_one_group(context: *mut c_void, group: usize) {
        // SAFETY: execute joins every callback before this tracker drops.
        let tracker = unsafe { &*context.cast::<JobTracker>() };
        tracker.seen[group].fetch_add(1, Ordering::Relaxed);
        let active = tracker.active.fetch_add(1, Ordering::Relaxed) + 1;
        tracker.peak.fetch_max(active, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(1));
        tracker.active.fetch_sub(1, Ordering::Relaxed);
    }

    struct PlacementTracker {
        caller: std::thread::ThreadId,
        seen: Vec<std::sync::atomic::AtomicUsize>,
        ran_on_caller: AtomicBool,
        ran_outside_pool: AtomicBool,
    }

    extern "C" fn track_placement(context: *mut c_void, group: usize) {
        // SAFETY: execute joins all callbacks before this tracker drops.
        let tracker = unsafe { &*context.cast::<PlacementTracker>() };
        tracker.seen[group].fetch_add(1, Ordering::Relaxed);
        if std::thread::current().id() == tracker.caller {
            tracker.ran_on_caller.store(true, Ordering::Relaxed);
        }
        if rayon::current_thread_index().is_none() {
            tracker.ran_outside_pool.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn rayon_waiter_runs_queued_permit_release_in_a_one_thread_pool() {
        let _test_guard = TEST_BUDGET_LOCK.lock().unwrap();
        let cancel = AtomicBool::new(false);
        let permit = admit(MAX_SCRATCH_SLOTS, &cancel).unwrap();
        assert_eq!(permit.count, MAX_SCRATCH_SLOTS);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| {
            pool.spawn_fifo(move || drop(permit));
            let acquired = admit(1, &cancel).unwrap();
            assert_eq!(acquired.count, 1);
        });
        assert_eq!(PEAK_SLOTS.load(Ordering::Relaxed), MAX_SCRATCH_SLOTS);
    }

    #[test]
    fn nested_develop_executors_share_slots_in_a_small_rayon_pool() {
        let _test_guard = TEST_BUDGET_LOCK.lock().unwrap();
        let cancel = AtomicBool::new(false);
        let held = admit(MAX_SCRATCH_SLOTS - 1, &cancel).unwrap();
        let context = ExecutorContext {
            cancel: &cancel,
            worker_limit: 2,
        };
        let count = std::sync::atomic::AtomicUsize::new(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        pool.install(|| {
            rayon::join(
                || {
                    assert_eq!(
                        execute(
                            (&context as *const ExecutorContext<'_>).cast_mut().cast(),
                            2,
                            count_worker,
                            (&count as *const std::sync::atomic::AtomicUsize)
                                .cast_mut()
                                .cast(),
                        ),
                        0
                    );
                },
                || {
                    assert_eq!(
                        execute(
                            (&context as *const ExecutorContext<'_>).cast_mut().cast(),
                            2,
                            count_worker,
                            (&count as *const std::sync::atomic::AtomicUsize)
                                .cast_mut()
                                .cast(),
                        ),
                        0
                    );
                },
            );
        });
        drop(held);
        assert!(count.load(Ordering::Relaxed) >= 2);
        assert!(PEAK_SLOTS.load(Ordering::Relaxed) <= MAX_SCRATCH_SLOTS);
    }

    #[test]
    fn one_callback_per_row_group_with_bounded_batches() {
        let _test_guard = TEST_BUDGET_LOCK.lock().unwrap();
        let cancel = AtomicBool::new(false);
        for (worker_limit, cap) in [
            (3, 3),
            (0, MAX_WORKERS_PER_SOURCE),
            (usize::MAX, MAX_SCRATCH_SLOTS),
        ] {
            let context = ExecutorContext {
                cancel: &cancel,
                worker_limit,
            };
            let tracker = JobTracker {
                seen: (0..17)
                    .map(|_| std::sync::atomic::AtomicUsize::new(0))
                    .collect(),
                active: std::sync::atomic::AtomicUsize::new(0),
                peak: std::sync::atomic::AtomicUsize::new(0),
            };
            assert_eq!(
                execute(
                    (&context as *const ExecutorContext<'_>).cast_mut().cast(),
                    tracker.seen.len(),
                    track_one_group,
                    (&tracker as *const JobTracker).cast_mut().cast(),
                ),
                0
            );
            assert!(
                tracker
                    .seen
                    .iter()
                    .all(|seen| seen.load(Ordering::Relaxed) == 1)
            );
            assert!(tracker.peak.load(Ordering::Relaxed) <= cap);
            assert_eq!(tracker.active.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn external_caller_only_coordinates_bounded_native_batches() {
        let _test_guard = TEST_BUDGET_LOCK.lock().unwrap();
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
            ran_on_caller: AtomicBool::new(false),
            ran_outside_pool: AtomicBool::new(false),
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
        assert!(!tracker.ran_on_caller.load(Ordering::Relaxed));
        assert!(!tracker.ran_outside_pool.load(Ordering::Relaxed));
        assert!(PEAK_SLOTS.load(Ordering::Relaxed) <= MAX_SCRATCH_SLOTS);
    }
}
