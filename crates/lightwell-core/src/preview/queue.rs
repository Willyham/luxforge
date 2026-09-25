//! The preview queue: one persistent latest-job worker with one active and one replaceable pending
//! job, whose results are delivered in generation order above a cancel floor.

use super::{PreviewJob, PreviewResult, worker::run};
#[cfg(doc)]
use crate::ErrorKind;
use crate::{ProxyCache, activity::ActivityBoard, latest::Latest};
use std::{sync::Arc, time::Instant};

/// One preview job as the worker receives it: the job, the activity board it is published on, and
/// the moment it was requested when the caller opted into phase timing.
pub(super) struct PreviewTask {
    pub(super) job: PreviewJob,
    pub(super) board: Option<Arc<ActivityBoard>>,
    pub(super) requested_at: Option<Instant>,
}

/// What one [`PreviewQueue::request_replacing`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct Queued {
    /// The generation the new job, and both of its results, are tagged with.
    pub generation: u64,
    /// The job this request took the pending slot from. It never started and never will, so it
    /// delivers nothing at all: this answer is the only moment its end is known.
    pub replaced: Option<u64>,
}

/// The preview worker: one persistent [`Latest`] worker that renders each job's proxy phase and
/// then its exact phase, with one active job and one replaceable pending job, results tagged with a
/// generation. It owns the one cached proxy source, so planning a job's proxy phase, building its
/// source and caching it all happen on the worker.
///
/// # What is delivered
///
/// A completed result is delivered whenever it is newer than what the display already has, not
/// only when it belongs to the newest request. Under a sustained drag a render almost always
/// finishes after a newer job has been requested, so dropping every superseded result presents no
/// frames at all. The rule is therefore a floor and a monotone order:
///
/// - [`Self::cancel`] raises the floor to the generation it returns, so everything in flight at
///   that moment is stale. It is the only thing that invalidates an in-flight result, which is what
///   an asset or selection change needs.
/// - [`Self::poll`] delivers results in the order the worker produced them, which is
///   `(generation, phase)` order: jobs run one at a time and a pending job is always newer than the
///   active one, so an older frame never follows a newer one on screen and a job's exact phase
///   follows its own proxy phase.
/// - An exact phase that answered [`ErrorKind::Cancelled`] carries no frame, and is delivered all
///   the same, under the same rules, as that outcome ([`PreviewResult::cancelled`]). So every job
///   that starts delivers exactly one exact-phase outcome above the floor — a frame, a failure or
///   cancelled — and a caller waiting for one generation learns when it has ended.
///
/// Newest-wins survives where it belongs: a newer request replaces the pending job, so at most one
/// job waits and the newest value is the one that runs next. A replaced job never starts and has
/// nothing to deliver; [`Self::request_replacing`] names it.
///
/// # The two phases and the two tokens
///
/// The proxy phase reads the job's **abandoned** token and the exact phase its **superseded** one
/// ([`crate::latest`]). A newer request supersedes the active job, which stops its exact phase —
/// nothing is waiting for that full-resolution frame, and it would compete for the Rayon pool with
/// the render that replaced it — but leaves its proxy phase running, because that frame is still
/// newer than what is on screen and stopping it is what starves a drag. [`Self::cancel`] abandons
/// the job, which stops both.
///
/// # When the next job starts
///
/// The worker takes the pending job itself as soon as the active one has handed over its exact
/// phase, so the next proxy render never waits for the consumer to poll.
pub struct PreviewQueue {
    worker: Latest<PreviewTask, PreviewResult>,
    /// Where each job is published as a `preview.render` activity; `None` publishes nothing.
    activity: Option<Arc<ActivityBoard>>,
}

impl Default for PreviewQueue {
    fn default() -> Self {
        // One proxy source, keyed by source identity and plan, held by the worker alone. Bounded by
        // construction: a new plan replaces the old entry rather than accumulating beside it.
        let mut cache = ProxyCache::default();
        Self {
            worker: Latest::new("lightwell-preview", move |task, running| {
                run(&mut cache, task, running)
            }),
            activity: None,
        }
    }
}

impl PreviewQueue {
    /// Queue `job` as the newest request and return its generation. The active job's exact phase
    /// is stopped; its proxy phase runs on.
    pub fn request(&mut self, job: PreviewJob) -> u64 {
        self.request_replacing(job).generation
    }

    /// [`Self::request`], also naming the job it replaced in the pending slot. That job never
    /// starts and delivers nothing at all, so this answer is the only way to learn that it ended.
    /// It is given in the same step as the request: asking [`Self::pending_generation`] first
    /// would race the worker, which takes the pending job by itself when the active one ends.
    pub fn request_replacing(&mut self, job: PreviewJob) -> Queued {
        self.request_inner(job, None)
    }

    /// [`Self::request_replacing`] with opt-in timing from this call until the worker starts the
    /// job, reported as [`PreviewResult::queue_wait_ms`]. The other request methods do not read the
    /// clock.
    pub fn request_timed(&mut self, job: PreviewJob) -> (Queued, Instant) {
        let requested_at = Instant::now();
        (self.request_inner(job, Some(requested_at)), requested_at)
    }

    fn request_inner(&mut self, job: PreviewJob, requested_at: Option<Instant>) -> Queued {
        let requested = self.worker.request(PreviewTask {
            job,
            board: self.activity.clone(),
            requested_at,
        });
        Queued {
            generation: requested.generation,
            replaced: requested.replaced.map(|(generation, _)| generation),
        }
    }

    /// Abandon the preview: drop the pending job, stop both phases of the active one and raise the
    /// delivery floor, so no frame planned before this call reaches the display.
    pub fn cancel(&mut self) -> u64 {
        self.worker.cancel()
    }

    /// Call this after every result is handed over, so nothing has to wake on a timer to find out.
    /// It runs on the worker thread, never on the catalog owner thread, and it must do nothing but
    /// post a message.
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.worker.set_waker(waker);
    }

    /// Publish every job requested from now on to `board` as a `preview.render` activity, from the
    /// moment the worker starts it to the end of its exact phase, with its phase as it moves from
    /// `proxy` to `exact`. The entry ends before the exact result is handed over, so by the time
    /// [`Self::poll`] delivers it the activity has already ended. A queue without a board
    /// publishes nothing.
    pub fn set_activity(&mut self, board: Arc<ActivityBoard>) {
        self.activity = Some(board);
    }

    /// The generation of the job waiting in the pending slot. It starts by itself when the active
    /// job has handed over its exact phase, so a caller that is about to request learns what that
    /// request replaced from [`Self::request_replacing`] instead.
    pub fn pending_generation(&self) -> Option<u64> {
        self.worker.pending_generation()
    }

    /// The generation of the last result [`Self::poll`] delivered, so a caller can correlate its
    /// frames and outcomes with the request that produced them. `0` before anything is delivered.
    pub fn last_delivered(&self) -> u64 {
        self.worker.last_delivered()
    }

    /// Whether a job is active or pending, or a result waits for [`Self::poll`].
    pub fn is_busy(&self) -> bool {
        self.worker.is_busy()
    }

    /// Whether a result waits for [`Self::poll`].
    pub fn ready(&self) -> bool {
        self.worker.ready()
    }

    /// The oldest result waiting, or nothing. What counts as stale is on [`PreviewQueue`]; a stale
    /// result is never handed over at all.
    pub fn poll(&mut self) -> Option<PreviewResult> {
        self.worker.poll().map(|(_, result)| result)
    }
}
