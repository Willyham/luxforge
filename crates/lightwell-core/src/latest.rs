//! One persistent worker that runs the newest job: the primitive behind the preview, the histogram
//! analysis and the desktop's clipping overlay.
//!
//! # Shape
//!
//! A [`Latest`] owns one thread for its whole life. The thread sleeps on a condition variable until
//! a job is handed to it, runs it, and takes the next job **itself** the moment the run returns, so
//! a job that waited never waits for the consumer to ask. At most one job runs and at most one
//! waits: a newer [`Latest::request`] replaces the waiting job, which never starts and delivers
//! nothing, and the request hands it back so the caller learns that it ended. Every job is tagged
//! with a generation, and so is every result it produces. Results reach the consumer through
//! [`Latest::poll`]; the worker calls the consumer's waker once per result, on the worker thread,
//! so nothing wakes on a timer to ask whether something has finished.
//!
//! # Delivery
//!
//! Results are delivered in the order they were produced, which is generation order: jobs run one
//! at a time and a waiting job is always newer than the running one. A result is delivered unless
//! [`Latest::cancel`] has since raised the floor to or above its generation. Nothing else makes a
//! result stale: a job that finished after a newer request still produced the newest result the
//! consumer has not seen, and dropping it is what starves a drag of frames. A consumer that wants
//! only the newest request's result filters by generation itself.
//!
//! # Tokens
//!
//! A running job reads two [`Cancel`] tokens, which say what has happened to it since it was handed
//! to the worker:
//!
//! - **superseded** is raised when a newer job is requested. The job is no longer the newest one,
//!   but whatever it still produces is delivered. Work that is only worth finishing while it is the
//!   newest stops on it.
//! - **abandoned** is raised by [`Latest::cancel`], by [`Latest::withdraw`] of this job, and when
//!   the worker is dropped. Nobody wants anything more from it. Abandoning a job also supersedes it,
//!   so work that stops on `superseded` stops on abandonment too. A job abandoned before the worker
//!   takes it never starts.
//!
//! The preview's two phases are why there are two. Its proxy phase stops only when abandoned: a
//! newer request does not interrupt it, because its display-size frame is still newer than anything
//! on screen and interrupting it is what starves a drag. Its exact phase stops when superseded: a
//! full-resolution render of a value the pointer has already left would only compete for the shared
//! Rayon pool with the proxy render of the newer one. An analysis stops only when abandoned, because
//! a newer request for another image does not make the running one worthless to the client that
//! asked for it.
//!
//! # Bounds
//!
//! One thread, one running job, one waiting job, and at most [`WAITING_RESULTS`] results waiting
//! for the consumer. A job with another result to hand over while that many wait holds on to it
//! until the consumer takes one — or until the result becomes stale, which drops it — so a
//! consumer that stops polling holds the worker back instead of letting results pile up. A job
//! that panics delivers nothing more, and the worker lives on to run the next one. Dropping the
//! [`Latest`] abandons the running job and ends the thread once that job returns; nothing joins it.
use crate::Cancel;
use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread,
};

/// How many results may wait for the consumer before a job with another to hand over waits for
/// room. Two is one preview job's two phases, so a worker one job ahead of its consumer never
/// waits.
pub const WAITING_RESULTS: usize = 2;

/// Called on the worker thread once per delivered result. It must do nothing but post a signal.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// One persistent worker, one running job and one replaceable waiting job, results tagged with a
/// generation. See the [module documentation](self).
pub struct Latest<J, R> {
    shared: Arc<Shared<J, R>>,
    last_delivered: u64,
}

/// What [`Latest::request`] did.
#[must_use]
pub struct Requested<J> {
    /// The generation the new job, and every result it produces, is tagged with.
    pub generation: u64,
    /// The job this request took the waiting slot from, with its generation. It never started and
    /// never will, so nothing of it is ever delivered: this is the only moment its end is known.
    pub replaced: Option<(u64, J)>,
}

/// The job the worker is running, as its run function sees it: its generation, its two tokens, and
/// the way to hand over a result before the last one.
pub struct Running<'a, J, R> {
    generation: u64,
    superseded: Cancel,
    abandoned: Cancel,
    shared: &'a Shared<J, R>,
}

struct Shared<J, R> {
    state: Mutex<State<J, R>>,
    /// Signalled when a job is handed to the worker, when room opens for a result, and when the
    /// worker is to stop. The worker is the only thread that waits on it.
    changed: Condvar,
}

struct State<J, R> {
    /// The last generation handed out, by a request or a cancel.
    generation: u64,
    /// Results at or below this generation are never delivered.
    floor: u64,
    /// The job handed to the worker. It is set the moment a request finds the worker idle, so a
    /// job that is requested from idle is running, not waiting, and it is released in the same
    /// step that delivers the job's last result.
    active: Option<Active<J>>,
    /// The one job waiting behind the active one. Never set while nothing is active.
    pending: Option<(u64, J)>,
    /// Results waiting for the consumer, oldest first; all above the floor.
    results: VecDeque<(u64, R)>,
    waker: Option<Wake>,
    stopped: bool,
}

struct Active<J> {
    generation: u64,
    superseded: Cancel,
    abandoned: Cancel,
    /// The job itself, until the worker takes it.
    job: Option<J>,
}

impl<J> Active<J> {
    fn new(generation: u64, job: J) -> Self {
        Self {
            generation,
            superseded: Cancel::new(),
            abandoned: Cancel::new(),
            job: Some(job),
        }
    }

    fn abandon(&self) {
        self.superseded.cancel();
        self.abandoned.cancel();
    }
}

impl<J, R> Shared<J, R> {
    fn lock(&self) -> MutexGuard<'_, State<J, R>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State<J, R>>) -> MutexGuard<'a, State<J, R>> {
        self.changed
            .wait(state)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Put one result of job `generation` among the waiting results, waiting while
    /// [`WAITING_RESULTS`] are already there. A result that will never be delivered — the floor has
    /// passed it, or the worker is stopping — is handed back so the caller drops it outside the
    /// lock: a frame can be large, and freeing it must not hold up the consumer's own calls.
    fn hand_over<'a>(
        &'a self,
        mut state: MutexGuard<'a, State<J, R>>,
        generation: u64,
        result: R,
    ) -> (MutexGuard<'a, State<J, R>>, Option<R>) {
        loop {
            if state.stopped || generation <= state.floor {
                return (state, Some(result));
            }
            if state.results.len() < WAITING_RESULTS {
                state.results.push_back((generation, result));
                return (state, None);
            }
            state = self.wait(state);
        }
    }
}

impl<J, R> Running<'_, J, R> {
    /// The generation this job was requested under.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Raised when a newer job is requested, and whenever this one is abandoned.
    pub fn superseded(&self) -> &Cancel {
        &self.superseded
    }

    /// Raised by [`Latest::cancel`], by [`Latest::withdraw`] of this job and when the worker is
    /// dropped.
    pub fn abandoned(&self) -> &Cancel {
        &self.abandoned
    }

    /// Hand one result to the consumer before the job returns its last one, and wake the consumer.
    ///
    /// Waits while [`WAITING_RESULTS`] results are already waiting. `false` means the result will
    /// never be delivered, because a cancel has made this job's results stale or the worker is
    /// stopping; the job has nothing more to do.
    pub fn send(&self, result: R) -> bool {
        let state = self.shared.lock();
        let (state, stale) = self.shared.hand_over(state, self.generation, result);
        let waker = if stale.is_none() {
            state.waker.clone()
        } else {
            None
        };
        drop(state);
        let delivered = stale.is_none();
        drop(stale);
        if let Some(waker) = waker {
            waker();
        }
        delivered
    }
}

impl<J: Send + 'static, R: Send + 'static> Latest<J, R> {
    /// Start the worker thread, named `name`. `run` is called on it once per job; what it returns
    /// is the job's last result, delivered in the same step that releases the job, and `None`
    /// delivers nothing more. `run` may keep state between jobs, which only this thread touches.
    pub fn new<F>(name: &str, mut run: F) -> Self
    where
        F: FnMut(J, &Running<'_, J, R>) -> Option<R> + Send + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                generation: 0,
                floor: 0,
                active: None,
                pending: None,
                results: VecDeque::new(),
                waker: None,
                stopped: false,
            }),
            changed: Condvar::new(),
        });
        let worker = shared.clone();
        thread::Builder::new()
            .name(name.into())
            .spawn(move || work(&worker, &mut run))
            .expect("the operating system starts one worker thread");
        Self {
            shared,
            last_delivered: 0,
        }
    }
}

impl<J, R> Latest<J, R> {
    /// Install the waker every delivered result calls, on the worker thread.
    pub fn set_waker(&self, waker: Wake) {
        self.shared.lock().waker = Some(waker);
    }

    /// Queue `job` as the newest request. When the worker is idle it runs at once; otherwise it
    /// takes the waiting slot, replacing whatever waited there, and the running job is told it has
    /// been superseded.
    pub fn request(&mut self, job: J) -> Requested<J> {
        let mut state = self.shared.lock();
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let replaced = match &state.active {
            Some(active) => {
                active.superseded.cancel();
                state.pending.replace((generation, job))
            }
            None => {
                state.active = Some(Active::new(generation, job));
                self.shared.changed.notify_all();
                None
            }
        };
        drop(state);
        Requested {
            generation,
            replaced,
        }
    }

    /// Abandon everything: drop the waiting job, abandon the running one and raise the floor, so
    /// no result of a job requested before this call is ever delivered, including those already
    /// waiting. Returns the new floor, which is above every generation handed out so far.
    pub fn cancel(&mut self) -> u64 {
        let mut state = self.shared.lock();
        state.generation = state.generation.saturating_add(1);
        state.floor = state.generation;
        let floor = state.floor;
        if let Some(active) = &state.active {
            active.abandon();
        }
        let pending = state.pending.take();
        let stale = std::mem::take(&mut state.results);
        // Room for a result the running job may be waiting to hand over, which it will now drop.
        self.shared.changed.notify_all();
        drop(state);
        drop((pending, stale));
        floor
    }

    /// Abandon one job: drop it from the waiting slot, or abandon it when it is the running one.
    /// A running job's results are still delivered, so the consumer learns how it ended; one that
    /// had not started yet never starts. `false` when no job of this generation is held.
    pub fn withdraw(&mut self, generation: u64) -> bool {
        let mut state = self.shared.lock();
        if state
            .pending
            .as_ref()
            .is_some_and(|(waiting, _)| *waiting == generation)
        {
            let pending = state.pending.take();
            drop(state);
            drop(pending);
            return true;
        }
        match &state.active {
            Some(active) if active.generation == generation => {
                active.abandon();
                true
            }
            _ => false,
        }
    }

    /// The oldest result waiting, with its generation, or nothing. Taking it makes room for a job
    /// that waits to hand over another.
    pub fn poll(&mut self) -> Option<(u64, R)> {
        let mut state = self.shared.lock();
        let (generation, result) = state.results.pop_front()?;
        self.shared.changed.notify_all();
        drop(state);
        self.last_delivered = generation;
        Some((generation, result))
    }

    /// Whether a result is waiting for [`Self::poll`].
    pub fn ready(&self) -> bool {
        !self.shared.lock().results.is_empty()
    }

    /// Whether anything is running, waiting to run, or waiting to be delivered.
    pub fn is_busy(&self) -> bool {
        let state = self.shared.lock();
        state.active.is_some() || state.pending.is_some() || !state.results.is_empty()
    }

    /// The generation of the job in the waiting slot. It becomes the running job by itself when
    /// the running one returns.
    pub fn pending_generation(&self) -> Option<u64> {
        self.shared
            .lock()
            .pending
            .as_ref()
            .map(|(generation, _)| *generation)
    }

    /// The generation of the job handed to the worker.
    pub fn active_generation(&self) -> Option<u64> {
        self.shared
            .lock()
            .active
            .as_ref()
            .map(|active| active.generation)
    }

    /// The generation of the last result [`Self::poll`] delivered; `0` before any.
    pub fn last_delivered(&self) -> u64 {
        self.last_delivered
    }
}

impl<J, R> Drop for Latest<J, R> {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.stopped = true;
        if let Some(active) = &state.active {
            active.abandon();
        }
        let pending = state.pending.take();
        let results = std::mem::take(&mut state.results);
        self.shared.changed.notify_all();
        drop(state);
        drop((pending, results));
    }
}

/// A job's run function, as the worker thread calls it.
type Run<J, R> = dyn FnMut(J, &Running<'_, J, R>) -> Option<R>;

/// The worker thread: take the active job, run it, deliver its last result and release it, and
/// promote the waiting job — all without the consumer.
fn work<J, R>(shared: &Shared<J, R>, run: &mut Run<J, R>) {
    let mut state = shared.lock();
    loop {
        let (generation, superseded, abandoned, job) = loop {
            if state.stopped {
                return;
            }
            if let Some(active) = &mut state.active
                && let Some(job) = active.job.take()
            {
                if active.abandoned.is_cancelled() {
                    // Abandoned before it started: it never starts, and the job behind it does.
                    state.active = state
                        .pending
                        .take()
                        .map(|(generation, job)| Active::new(generation, job));
                    drop(state);
                    drop(job);
                    state = shared.lock();
                    continue;
                }
                break (
                    active.generation,
                    active.superseded.clone(),
                    active.abandoned.clone(),
                    job,
                );
            }
            state = shared.wait(state);
        };
        drop(state);
        let running = Running {
            generation,
            superseded,
            abandoned,
            shared,
        };
        let last = catch_unwind(AssertUnwindSafe(|| run(job, &running))).unwrap_or(None);
        state = shared.lock();
        let (mut held, delivered, stale) = match last {
            Some(result) => {
                let (held, stale) = shared.hand_over(state, generation, result);
                (held, stale.is_none(), stale)
            }
            None => (state, false, None),
        };
        // Released in the same step that delivers its last result, so a consumer that has taken
        // that result finds this job gone; and the job that waited is handed over now.
        held.active = held
            .pending
            .take()
            .map(|(generation, job)| Active::new(generation, job));
        let waker = if delivered { held.waker.clone() } else { None };
        drop(held);
        drop(stale);
        if let Some(waker) = waker {
            waker();
        }
        state = shared.lock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc::{Receiver, Sender, channel},
        },
        time::{Duration, Instant},
    };

    const DEADLINE: Duration = Duration::from_secs(60);

    /// A job that waits for the test to release it and reports what happened to its tokens.
    struct Held {
        value: u64,
        release: Receiver<()>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Ran {
        value: u64,
        superseded: bool,
        abandoned: bool,
    }

    /// A worker whose jobs each wait for their own release, and announce on `started` that they
    /// have begun.
    fn held_worker(started: Sender<u64>) -> Latest<Held, Ran> {
        Latest::new("lightwell-latest-test", move |job: Held, running| {
            let _ = started.send(job.value);
            let _ = job.release.recv();
            Some(Ran {
                value: job.value,
                superseded: running.superseded().is_cancelled(),
                abandoned: running.abandoned().is_cancelled(),
            })
        })
    }

    fn held(value: u64) -> (Held, Sender<()>) {
        let (release, receiver) = channel();
        (
            Held {
                value,
                release: receiver,
            },
            release,
        )
    }

    fn poll_one<J, R>(latest: &mut Latest<J, R>) -> (u64, R) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(result) = latest.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "no result arrived");
            std::thread::yield_now();
        }
    }

    fn drain<J, R>(latest: &mut Latest<J, R>) -> Vec<(u64, R)> {
        let deadline = Instant::now() + DEADLINE;
        let mut results = Vec::new();
        while latest.is_busy() {
            if let Some(result) = latest.poll() {
                results.push(result);
            }
            assert!(Instant::now() < deadline, "the worker never went idle");
            std::thread::yield_now();
        }
        results
    }

    /// The point of the primitive: when the running job returns, the worker takes the waiting one
    /// by itself. Nothing here polls until the second job has started, and the first job's result
    /// is still waiting for the consumer when it does.
    #[test]
    fn the_next_job_starts_without_a_consumer_poll() {
        let (started, starts) = channel();
        let mut latest = held_worker(started);
        let (first, release_first) = held(1);
        let (second, release_second) = held(2);
        let one = latest.request(first).generation;
        assert_eq!(starts.recv_timeout(DEADLINE), Ok(1));
        let two = latest.request(second);
        assert!(two.replaced.is_none(), "nothing waited before it");
        assert_eq!(latest.pending_generation(), Some(two.generation));

        release_first.send(()).unwrap();
        assert_eq!(
            starts.recv_timeout(DEADLINE),
            Ok(2),
            "the waiting job started with no poll"
        );
        assert_eq!(latest.pending_generation(), None);
        assert_eq!(latest.active_generation(), Some(two.generation));
        assert!(latest.ready(), "the first result is still undelivered");
        assert_eq!(latest.last_delivered(), 0);

        release_second.send(()).unwrap();
        let delivered: Vec<(u64, u64)> = drain(&mut latest)
            .into_iter()
            .map(|(generation, ran)| (generation, ran.value))
            .collect();
        assert_eq!(delivered, [(one, 1), (two.generation, 2)]);
        assert_eq!(latest.last_delivered(), two.generation);
    }

    /// A newer request supersedes the running job and replaces the waiting one, which is handed
    /// back and never runs. The superseded job's result is still delivered, in order.
    #[test]
    fn a_newer_request_supersedes_the_running_job_and_replaces_the_waiting_one() {
        let (started, starts) = channel();
        let mut latest = held_worker(started);
        let (first, release_first) = held(1);
        let one = latest.request(first).generation;
        assert_eq!(starts.recv_timeout(DEADLINE), Ok(1));
        let (second, _never) = held(2);
        let two = latest.request(second).generation;
        let (third, release_third) = held(3);
        let three = latest.request(third);
        let (replaced, job) = three.replaced.expect("the second job waited");
        assert_eq!((replaced, job.value), (two, 2));

        release_first.send(()).unwrap();
        release_third.send(()).unwrap();
        let delivered = drain(&mut latest);
        assert_eq!(
            delivered,
            [
                (
                    one,
                    Ran {
                        value: 1,
                        superseded: true,
                        abandoned: false
                    }
                ),
                (
                    three.generation,
                    Ran {
                        value: 3,
                        superseded: false,
                        abandoned: false
                    }
                ),
            ]
        );
        assert_eq!(
            starts.try_iter().collect::<Vec<_>>(),
            [3],
            "the replaced job never ran"
        );
    }

    /// `cancel` abandons the running job, drops the waiting one and every waiting result, and
    /// raises the floor so nothing requested before it is delivered, even what finishes after it.
    #[test]
    fn cancel_abandons_everything_below_the_floor() {
        let (started, starts) = channel();
        let mut latest = held_worker(started);
        let (first, release_first) = held(1);
        let _ = latest.request(first);
        assert_eq!(starts.recv_timeout(DEADLINE), Ok(1));
        let (second, _never) = held(2);
        let _ = latest.request(second);
        let floor = latest.cancel();
        assert_eq!(
            latest.pending_generation(),
            None,
            "the waiting job was dropped"
        );
        release_first.send(()).unwrap();
        assert!(drain(&mut latest).is_empty(), "nothing below the floor");
        assert!(floor > 2);

        // A request after the cancel runs and is delivered as usual.
        let (third, release_third) = held(3);
        let three = latest.request(third).generation;
        assert!(three > floor);
        release_third.send(()).unwrap();
        let (generation, ran) = poll_one(&mut latest);
        assert_eq!((generation, ran.value), (three, 3));
        assert!(!ran.superseded && !ran.abandoned);
    }

    /// `withdraw` drops a waiting job, and abandons a running one whose outcome is still delivered.
    #[test]
    fn withdraw_drops_a_waiting_job_and_abandons_a_running_one() {
        let (started, starts) = channel();
        let mut latest = held_worker(started);
        let (first, release_first) = held(1);
        let one = latest.request(first).generation;
        assert_eq!(starts.recv_timeout(DEADLINE), Ok(1));
        let (second, _never) = held(2);
        let two = latest.request(second).generation;
        assert!(latest.withdraw(two), "the waiting job");
        assert_eq!(latest.pending_generation(), None);
        assert!(latest.withdraw(one), "the running job");
        assert!(!latest.withdraw(two), "already gone");
        release_first.send(()).unwrap();
        let delivered = drain(&mut latest);
        assert_eq!(
            delivered,
            [(
                one,
                Ran {
                    value: 1,
                    superseded: true,
                    abandoned: true
                }
            )],
            "the withdrawn running job's outcome still arrives; the waiting one never ran"
        );
        assert!(starts.try_iter().next().is_none());
    }

    /// The waker is called once per delivered result, on the worker thread, and never for a result
    /// that is dropped.
    #[test]
    fn the_waker_is_called_once_per_delivered_result() {
        let calls = Arc::new(AtomicU64::new(0));
        let counter = calls.clone();
        let consumer = std::thread::current().id();
        let mut latest: Latest<u64, u64> =
            Latest::new("lightwell-latest-test", |value, running| {
                running.send(value * 10);
                Some(value * 10 + 1)
            });
        latest.set_waker(Arc::new(move || {
            assert_ne!(std::thread::current().id(), consumer);
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        let one = latest.request(1).generation;
        let delivered = drain(&mut latest);
        assert_eq!(delivered, [(one, 10), (one, 11)]);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    /// A consumer that does not poll holds the worker back: no more than [`WAITING_RESULTS`] wait,
    /// and the job with another to hand over continues only once one is taken.
    #[test]
    fn a_consumer_that_does_not_poll_holds_the_worker_back() {
        let sent = Arc::new(AtomicU64::new(0));
        let counter = sent.clone();
        let mut latest: Latest<u64, u64> =
            Latest::new("lightwell-latest-test", move |count, running| {
                for value in 0..count {
                    running.send(value);
                    counter.fetch_add(1, Ordering::Relaxed);
                }
                None
            });
        let generation = latest.request(5).generation;
        let deadline = Instant::now() + DEADLINE;
        while sent.load(Ordering::Relaxed) < WAITING_RESULTS as u64 {
            assert!(Instant::now() < deadline, "the first results never arrived");
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            sent.load(Ordering::Relaxed),
            WAITING_RESULTS as u64,
            "the job waits for room"
        );
        let delivered = drain(&mut latest);
        assert_eq!(
            delivered,
            (0..5).map(|value| (generation, value)).collect::<Vec<_>>()
        );
    }

    /// A job that panics delivers nothing, and the worker lives on to run the next one.
    #[test]
    fn a_job_that_panics_does_not_stop_the_worker() {
        let mut latest: Latest<u64, u64> = Latest::new("lightwell-latest-test", |value, _| {
            assert!(value != 0, "a job that panics");
            Some(value)
        });
        let _ = latest.request(0);
        let two = latest.request(2).generation;
        assert_eq!(drain(&mut latest), [(two, 2)]);
    }
}
