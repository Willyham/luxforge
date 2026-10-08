//! The analysis worker: what a pixel-reading call computes after its tile read, such as Auto
//! tone's solve over its sample grid, run off the tile service's thread so the next read,
//! `render.sample`, the neutral picker and an export's bands do not wait behind it.
//!
//! # Shape
//!
//! A call's evaluation on the tile service reads what it needs and hands the rest of its work on
//! ([`super::Step::Then`]); [`super::TileCall::run`] queues that work here with the call's own
//! cancellation and reply, so the caller is answered from this thread exactly as it would have
//! been from the service's. The owner's parked reads, a batch's reads and the owner's analysis
//! queries all reach it this way, so their cancellation covers this work unchanged.
//!
//! One thread, `luxforge-analysis`, per render context, started by the first work handed to it
//! and blocked on its channel while nothing waits. It answers in the order work arrived: every
//! piece has a caller waiting on it, so none is superseded (this is not [`crate::latest`]).
//!
//! # Bounds
//!
//! At most [`ANALYSIS_QUEUE_CAPACITY`] pieces wait behind the one running, the tile queue's own
//! bound, since each comes from a call the tile service took; past it the call is refused with
//! `resource-limit` at once, and handing work on never blocks the tile service (rule 12). Work
//! whose cancellation is set is answered `cancelled` when its turn comes, without running. A
//! panic answers `internal`, and the thread lives on. The thread ends once its render context is
//! dropped and nothing is left to run.
use crate::Error;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
};

/// How many pieces of work may wait behind the one the worker runs: the tile queue's bound
/// ([`super::TILE_QUEUE_CAPACITY`]), since each piece continues a call the tile service took.
pub const ANALYSIS_QUEUE_CAPACITY: usize = super::TILE_QUEUE_CAPACITY;

/// A piece of work, which answers its own caller exactly once: by running, or by being refused.
pub(crate) trait Work: Send {
    /// Run and answer, cancelled or not, containing its own panics.
    fn run(self: Box<Self>);
    /// Answer `error` without running.
    fn refuse(self: Box<Self>, error: Error);
}

/// The render context's analysis worker. See the [module documentation](self).
#[derive(Default)]
pub(crate) struct AnalysisWorker {
    /// The worker's queue, once the first piece of work has started it.
    queue: Mutex<Option<SyncSender<Box<dyn Work>>>>,
    /// What the worker shares with its thread: in tests, a hold and a count of waiting work.
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    /// Called on the worker before each piece of work runs, so a test can hold it there.
    #[cfg(test)]
    hold: Mutex<Option<super::Hold>>,
    /// How many pieces wait behind the one running.
    #[cfg(test)]
    waiting: std::sync::atomic::AtomicUsize,
}

impl AnalysisWorker {
    /// Queue `work` behind the pieces waiting, or refuse it at once: with `resource-limit` past
    /// [`ANALYSIS_QUEUE_CAPACITY`] waiting pieces, and `internal` when the thread cannot be
    /// started. Never blocks.
    pub(crate) fn submit(&self, work: Box<dyn Work>) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.is_none() {
            let (sender, receiver) = sync_channel(ANALYSIS_QUEUE_CAPACITY);
            let shared = Arc::clone(&self.shared);
            if let Err(error) = std::thread::Builder::new()
                .name("luxforge-analysis".into())
                .spawn(move || run(&receiver, &shared))
            {
                drop(queue);
                work.refuse(Error::internal(format!(
                    "the analysis worker could not be started: {error}"
                )));
                return;
            }
            *queue = Some(sender);
        }
        let sender = queue.as_ref().expect("started above");
        #[cfg(test)]
        self.shared
            .waiting
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (work, error) = match sender.try_send(work) {
            Ok(()) => return,
            Err(TrySendError::Full(work)) => (
                work,
                Error::resource_limit(
                    "analyses are already waiting for the analysis worker; retry after one is \
                     answered",
                ),
            ),
            Err(TrySendError::Disconnected(work)) => {
                *queue = None;
                (work, Error::internal("the analysis worker stopped"))
            }
        };
        #[cfg(test)]
        self.shared
            .waiting
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        drop(queue);
        work.refuse(error);
    }

    /// Have the worker call `hold` before it runs each piece of work, or stop calling it.
    #[cfg(test)]
    pub(crate) fn hold(&self, hold: Option<super::Hold>) {
        *self
            .shared
            .hold
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = hold;
    }

    /// How many pieces of work wait behind the one the worker runs.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.shared
            .waiting
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The worker's loop: each piece in turn until the render context, which holds the sender, is
/// dropped. A piece contains its own panics; this guard only keeps the thread alive past one.
fn run(receiver: &Receiver<Box<dyn Work>>, shared: &Shared) {
    while let Ok(work) = receiver.recv() {
        #[cfg(test)]
        {
            shared
                .waiting
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            let hold = shared
                .hold
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(hold) = hold {
                hold();
            }
        }
        #[cfg(not(test))]
        let _ = shared;
        let _ = catch_unwind(AssertUnwindSafe(|| work.run()));
    }
}
