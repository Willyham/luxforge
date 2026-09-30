//! The index lane's query threads: the file system questions the catalog owner must not ask
//! itself, since a hung network volume or a slow card can keep any of them waiting for seconds or
//! for good (performance rule 5). The owner hands a thread one question at a time and answers the
//! call that asked when the thread posts back (`api/owner/files/queries.rs`); nothing here touches
//! the catalog.
//!
//! Two threads, each a [`Worker`]: the **query thread** answers what a client names — a folder's
//! subfolders, a path to add, browse or refresh, a card by its volume — and the **survey thread**
//! learns what the owner answers the volume and card lists from ([`super::survey`]). They are
//! apart because a survey stats every mounted volume, so it is the one question sure to meet a
//! hung one whenever there is one; on its own thread it holds up only the next survey, never a
//! question about another volume.
use crate::{Error, atomic_file::file_error};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::mpsc::{SyncSender, sync_channel},
    thread::{self, JoinHandle},
};

/// What the owner hands a thread: it runs there and posts its own answer back.
pub(crate) type Job = Box<dyn FnOnce() + Send>;

/// One thread of the index lane that runs jobs one at a time, started on its first job and blocked
/// on its channel while idle (performance rule 8). The channel holds one job, and the owner sends
/// one only while the thread is idle, so sending never waits; what waits for the thread waits on
/// the owner, in a queue it bounds.
pub(crate) struct Worker {
    jobs: SyncSender<Job>,
    thread: JoinHandle<()>,
}

impl Worker {
    pub(crate) fn start(name: &str) -> Result<Self, Error> {
        let (jobs, queued) = sync_channel::<Job>(1);
        let thread = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Ok(job) = queued.recv() {
                    // A job answers its caller even when it panics; this keeps the thread for the
                    // next one.
                    let _ = catch_unwind(AssertUnwindSafe(job));
                }
            })
            .map_err(|error| Error::internal(format!("cannot start {name}: {error}")))?;
        Ok(Self { jobs, thread })
    }

    /// Hand the thread its next job; the owner sends one only while the thread is idle.
    pub(crate) fn send(&self, job: Job) -> Result<(), Error> {
        self.jobs
            .try_send(job)
            .map_err(|_| Error::internal("an index lane thread is busy or has stopped"))
    }

    /// Stop the thread: its channel closes, so it ends once the job it runs, if any, returns. An
    /// idle thread is joined at once. A `busy` one is not waited for: its job may be waiting on a
    /// volume that never answers, and stopping the owner must not wait with it; it ends by itself
    /// if the volume ever answers, and its answer goes nowhere.
    pub(crate) fn stop(self, busy: bool) {
        let Self { jobs, thread } = self;
        drop(jobs);
        if !busy {
            let _ = thread.join();
        }
    }
}

/// The folder at the absolute `path`, which must exist and be a directory: its canonical path.
pub(crate) fn existing_folder(path: &Path) -> Result<PathBuf, Error> {
    let canonical = path
        .canonicalize()
        .map_err(|error| file_error(format!("cannot find {}", path.display()), error.kind()))?;
    if !canonical.is_dir() {
        return Err(Error::validation(format!(
            "{} is a file, not a folder",
            path.display()
        )));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    /// Jobs run in order on the one thread, a panicking one does not end it, and an idle worker
    /// stops at once.
    #[test]
    fn a_worker_runs_jobs_in_order_and_outlives_a_panic() {
        let worker = Worker::start("luxforge-test-worker").unwrap();
        let (done, answers) = channel();
        for index in 0..3 {
            let done = done.clone();
            // Each job is sent once the one before has answered, so the one-job channel is empty,
            // as the owner sends only to an idle thread.
            worker
                .send(Box::new(move || {
                    done.send(index).unwrap();
                    assert_ne!(index, 1, "the second job panics after answering");
                }))
                .unwrap();
            assert_eq!(answers.recv().unwrap(), index);
        }
        worker.stop(false);
        assert!(
            existing_folder(Path::new("/no/such/folder")).is_err(),
            "a missing folder"
        );
    }
}
