//! macOS: one FSEvents stream per root ([`fsevents`]) and one Disk Arbitration session
//! ([`disks`]), all delivering on one serial dispatch queue, so the watcher runs no thread of its
//! own. The queue runs the callbacks one at a time; a barrier on it (`exec_sync` of nothing) waits
//! for any callback running or queued, which is what makes freeing a callback's context safe.
use crate::{
    Mount, RETRY, WatchEvent, WatchRoot, already_watched,
    delivery::{Delivery, Sink},
    mounts,
};
use dispatch2::{DispatchQueue, DispatchRetained, DispatchTime};
use std::{
    collections::HashMap,
    io,
    path::Path,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

mod disks;
mod fsevents;

pub(crate) use fsevents::current_cursor;

/// What every callback reaches: the queue, the delivery and the retry timer's state.
pub(crate) struct Shared {
    queue: DispatchRetained<DispatchQueue>,
    delivery: Delivery,
    /// The shared state itself, for the retry timer, which holds it weakly so a watcher dropped
    /// while one is pending is not kept alive by it.
    me: Weak<Shared>,
    /// Whether a retry is pending on the queue.
    armed: AtomicBool,
    /// Set when the watcher is dropped: a retry that fires after sends nothing.
    stopped: AtomicBool,
}

impl Shared {
    /// Send a root's events, and arm a retry if anything is owed.
    fn root(&self, root: u64, path: &Path, events: Vec<WatchEvent>) {
        if !events.is_empty() && self.delivery.root(root, path, events) {
            self.arm();
        }
    }

    /// Read the mount table and tell the receiver how it differs, arming a retry if anything is
    /// owed.
    fn volumes(&self) {
        let owed = match mounts() {
            Ok(table) => self.delivery.volumes(&table),
            Err(_) => {
                self.delivery.owe_volumes();
                true
            }
        };
        if owed {
            self.arm();
        }
    }

    /// Try again in [`RETRY`], unless a retry is pending already or the watcher stopped.
    fn arm(&self) {
        if self.stopped.load(Ordering::SeqCst) || self.armed.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.me.clone();
        let when = DispatchTime::try_from(RETRY).unwrap_or(DispatchTime::NOW);
        let _ = self.queue.after(when, move || {
            if let Some(shared) = me.upgrade() {
                shared.armed.store(false, Ordering::SeqCst);
                shared.retry();
            }
        });
    }

    fn retry(&self) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if self.delivery.retry() {
            self.arm();
        }
        if self.delivery.volumes_owed() {
            self.volumes();
        }
    }

    /// Wait for every callback running or queued on the queue.
    fn barrier(&self) {
        self.queue.exec_sync(|| {});
    }
}

pub(crate) struct Watcher {
    shared: Arc<Shared>,
    streams: HashMap<u64, fsevents::Stream>,
    disks: Option<disks::Session>,
}

impl Watcher {
    pub(crate) fn start(sink: Sink) -> io::Result<Self> {
        let table: Vec<Mount> = mounts().unwrap_or_default();
        let shared = Arc::new_cyclic(|me| Shared {
            queue: DispatchQueue::new("org.luxforge.watch", None),
            delivery: Delivery::new(sink, table),
            me: me.clone(),
            armed: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });
        let disks = disks::Session::start(&shared)?;
        Ok(Self {
            shared,
            streams: HashMap::new(),
            disks: Some(disks),
        })
    }

    pub(crate) fn add_root(&mut self, root: WatchRoot) -> io::Result<()> {
        if self.streams.contains_key(&root.id) {
            return Err(already_watched(root.id));
        }
        let id = root.id;
        let stream = fsevents::Stream::open(&self.shared, root)?;
        self.streams.insert(id, stream);
        Ok(())
    }

    pub(crate) fn remove_root(&mut self, id: u64) {
        if let Some(stream) = self.streams.remove(&id) {
            let context = stream.stop();
            self.shared.barrier();
            self.shared.delivery.forget(id);
            drop(context);
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        let contexts: Vec<_> = self
            .streams
            .drain()
            .map(|(_, stream)| stream.stop())
            .collect();
        let disks = self.disks.take().map(disks::Session::stop);
        // No callback runs after this, so the contexts they read can go.
        self.shared.barrier();
        drop(contexts);
        drop(disks);
    }
}
