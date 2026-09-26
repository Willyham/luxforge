//! The point worker: one thread that evaluates the point samples the catalog owner planned through a
//! spatial layer and answers each on the call's own reply channel, so another client's call never
//! waits behind a sample's tile.
//!
//! # Shape
//!
//! The owner plans a sample in `O(layers)` and hands the plan here with the reply channel its caller
//! is blocked on; it then moves on to its next message. The worker takes calls in the order they
//! were queued and answers each with the request's identity and the event sequence the owner had
//! when it planned it. A client's order is kept by construction: every transport serves one call at
//! a time per connection, so a client has at most one sample here. This is not the latest-wins
//! primitive ([`crate::latest`]): every queued sample was asked for by someone still waiting on it.
//!
//! # Bounds
//!
//! One thread, started with the first sample and blocked on a condition variable while its queue is
//! empty (rule 8). At most [`POINT_QUEUE_CAPACITY`] calls wait behind the one being evaluated: one
//! per live loopback connection plus the desktop's one sample in flight. A full queue answers
//! `resource-limit` at once (rule 6). A disconnect drops that client's waiting calls, whose reply
//! channels close unanswered. The worker evaluates one sample at a time from the shared spatial
//! budget, and the tile's input is pulled serially, as it was on the owner: pulled on the shared
//! pool it would wait behind any render that holds the pool.
use super::{ApiResponse, ClientId};
use crate::{Error, api::transport::MAX_CLIENTS};
use serde_json::Value;
use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc::SyncSender},
    thread::JoinHandle,
};

/// How many planned samples may wait behind the one being evaluated: every live loopback
/// connection's one outstanding call, and the desktop's one sample in flight.
pub(super) const POINT_QUEUE_CAPACITY: usize = MAX_CLIENTS + 1;

/// The evaluation one caller is waiting for, planned on the owner.
pub(super) type Evaluation = Box<dyn FnOnce() -> Result<Value, Error> + Send>;

/// Called on the worker before each evaluation, so a test can hold it there.
#[cfg(test)]
pub(super) type Hold = Arc<dyn Fn() + Send + Sync>;

/// One call the owner planned and the point worker answers.
pub(super) struct PointCall {
    pub(super) client: ClientId,
    pub(super) evaluate: Evaluation,
    pub(super) reply: Reply,
}

/// Where and how one call is answered.
pub(super) struct Reply {
    /// The request's identity, which the answer repeats.
    pub(super) id: String,
    /// The event sequence when the owner planned the sample, which the answer carries: the sample
    /// describes the catalog as it was then, whatever has been committed since.
    pub(super) sequence: u64,
    pub(super) response: SyncSender<ApiResponse>,
}

impl Reply {
    pub(super) fn answer(self, result: Result<Value, Error>) {
        let response = match result {
            Ok(value) => ApiResponse::success(self.id, self.sequence, value),
            Err(error) => ApiResponse::failure(self.id, self.sequence, error),
        };
        // A caller that has gone has nobody to answer.
        let _ = self.response.send(response);
    }
}

/// The owner's handle on the point worker. See the [module documentation](self).
pub(super) struct PointWorker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    capacity: usize,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a call is queued and when the worker is to stop. Only the worker waits on it.
    queued: Condvar,
}

#[derive(Default)]
struct State {
    queue: VecDeque<PointCall>,
    stopping: bool,
    #[cfg(test)]
    hold: Option<Hold>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing panics while holding the lock: evaluations run outside it.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PointWorker {
    /// A worker that starts no thread until the first sample is queued.
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                queued: Condvar::new(),
            }),
            thread: None,
            capacity,
        }
    }

    /// Queue one planned call behind the others, starting the thread with the first. A full queue
    /// answers the call with `resource-limit` now, on its own channel.
    pub(super) fn submit(&mut self, call: PointCall) {
        let mut state = self.shared.lock();
        if state.queue.len() >= self.capacity {
            drop(state);
            call.reply.answer(Err(Error::resource_limit(
                format!(
                    "{} point samples through a spatial layer are already waiting; retry after one is answered",
                    self.capacity
                ))));
            return;
        }
        state.queue.push_back(call);
        drop(state);
        self.shared.queued.notify_one();
        if self.thread.is_none() {
            let shared = self.shared.clone();
            self.thread = std::thread::Builder::new()
                .name("luxforge-point".into())
                .spawn(move || run(&shared))
                .ok();
            if self.thread.is_none() {
                // Without its thread nothing would ever answer these calls: refuse them now.
                let waiting = std::mem::take(&mut self.shared.lock().queue);
                for call in waiting {
                    call.reply.answer(Err(Error::internal(
                        "the point worker could not be started",
                    )));
                }
            }
        }
    }

    /// Drop a disconnected client's waiting calls. Their reply channels close unanswered; a call
    /// already being evaluated finishes and its answer goes nowhere.
    pub(super) fn disconnect(&self, client: ClientId) {
        let gone: VecDeque<PointCall> = {
            let mut state = self.shared.lock();
            let (gone, kept) = std::mem::take(&mut state.queue)
                .into_iter()
                .partition(|call| call.client == client);
            state.queue = kept;
            gone
        };
        drop(gone);
    }

    /// How many calls are waiting behind the one being evaluated.
    #[cfg(test)]
    pub(super) fn waiting(&self) -> usize {
        self.shared.lock().queue.len()
    }

    /// Whether the worker thread has started.
    #[cfg(test)]
    pub(super) fn started(&self) -> bool {
        self.thread.is_some()
    }

    #[cfg(test)]
    pub(super) fn hold(&self, hold: Option<Hold>) {
        self.shared.lock().hold = hold;
    }

    /// Stop the worker once the sample it is evaluating returns. Waiting calls are dropped
    /// unanswered, as every other call is when the owner stops.
    pub(super) fn stop(&mut self) {
        let waiting = {
            let mut state = self.shared.lock();
            state.stopping = true;
            std::mem::take(&mut state.queue)
        };
        drop(waiting);
        self.shared.queued.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for PointWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(shared: &Shared) {
    loop {
        let PointCall {
            evaluate, reply, ..
        } = {
            let mut state = shared.lock();
            loop {
                if state.stopping {
                    return;
                }
                if let Some(call) = state.queue.pop_front() {
                    break call;
                }
                state = shared
                    .queued
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        #[cfg(test)]
        {
            let hold = shared.lock().hold.clone();
            if let Some(hold) = hold {
                hold();
            }
        }
        // A sample that panics answers `internal`, and the worker lives on for the next one.
        let result = catch_unwind(AssertUnwindSafe(evaluate))
            .unwrap_or_else(|_| Err(Error::internal("the point sample's evaluation panicked")));
        reply.answer(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        sync::mpsc::{Receiver, channel, sync_channel},
        time::Duration,
    };

    fn call(client: u64, id: &str, value: Value) -> (PointCall, Receiver<ApiResponse>) {
        let (response, answer) = sync_channel(1);
        (
            PointCall {
                client: ClientId::testing(client),
                evaluate: Box::new(move || Ok(value)),
                reply: Reply {
                    id: id.into(),
                    sequence: client * 10,
                    response,
                },
            },
            answer,
        )
    }

    /// A worker held before each evaluation until the test releases it, with a signal each time it
    /// reaches the hold.
    fn held(capacity: usize) -> (PointWorker, Receiver<()>, SyncSender<()>) {
        let worker = PointWorker::new(capacity);
        let (reached, reaches) = channel();
        let (release, released) = sync_channel::<()>(64);
        let reached = Mutex::new(reached);
        let released = Mutex::new(released);
        worker.hold(Some(Arc::new(move || {
            let _ = reached.lock().unwrap().send(());
            let _ = released.lock().unwrap().recv();
        })));
        (worker, reaches, release)
    }

    #[test]
    fn calls_are_answered_in_order_with_their_identity_and_planned_sequence() {
        let mut worker = PointWorker::new(POINT_QUEUE_CAPACITY);
        assert!(!worker.started(), "no thread before the first sample");
        let answers: Vec<_> = (1..=3)
            .map(|client| {
                let (call, answer) = call(client, &format!("s{client}"), json!(client));
                worker.submit(call);
                answer
            })
            .collect();
        assert!(worker.started());
        for (client, answer) in (1..=3).zip(answers) {
            let response = answer.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(response.id, format!("s{client}"));
            assert_eq!(response.sequence, client * 10);
            assert_eq!(response.result, Some(json!(client)));
        }
        worker.stop();
    }

    #[test]
    fn a_full_queue_refuses_with_resource_limit_and_a_disconnect_drops_only_that_clients_calls() {
        let (mut worker, reaches, release) = held(2);
        // The first call is taken and held; two more fill the queue behind it.
        let (first, first_answer) = call(1, "running", json!(1));
        worker.submit(first);
        reaches.recv_timeout(Duration::from_secs(5)).unwrap();
        let (second, second_answer) = call(2, "queued-2", json!(2));
        let (third, third_answer) = call(3, "queued-3", json!(3));
        worker.submit(second);
        worker.submit(third);
        assert_eq!(worker.waiting(), 2);
        let (fourth, fourth_answer) = call(4, "refused", json!(4));
        worker.submit(fourth);
        let refused = fourth_answer.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(refused.error.unwrap().code, "resource-limit");
        assert_eq!(
            refused.sequence, 40,
            "a refusal carries the planned sequence"
        );

        // Client 2 goes; its waiting call closes unanswered and client 3's stays.
        worker.disconnect(ClientId::testing(2));
        assert_eq!(worker.waiting(), 1);
        assert!(
            second_answer.recv_timeout(Duration::from_secs(5)).is_err(),
            "a disconnected client's sample is dropped, not answered"
        );
        for _ in 0..2 {
            release.send(()).unwrap();
        }
        assert_eq!(
            first_answer
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .result,
            Some(json!(1))
        );
        assert_eq!(
            third_answer
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .result,
            Some(json!(3))
        );
        worker.stop();
    }

    #[test]
    fn a_panicking_sample_answers_internal_and_the_worker_answers_the_next() {
        let mut worker = PointWorker::new(POINT_QUEUE_CAPACITY);
        let (response, panicked) = sync_channel(1);
        worker.submit(PointCall {
            client: ClientId::testing(1),
            evaluate: Box::new(|| panic!("a sample panicked")),
            reply: Reply {
                id: "panics".into(),
                sequence: 0,
                response,
            },
        });
        assert_eq!(
            panicked
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .error
                .unwrap()
                .code,
            "internal"
        );
        let (next, answer) = call(1, "next", json!("fine"));
        worker.submit(next);
        assert_eq!(
            answer.recv_timeout(Duration::from_secs(5)).unwrap().result,
            Some(json!("fine"))
        );
        worker.stop();
    }
}
