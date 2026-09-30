//! Lane C's worker thread: one thread, started on the lane's first job, that takes one job at a time
//! from a one-slot channel and blocks on it while idle (performance rule 8). What a job is, and what
//! the worker does with its result, is the owner's side (`api/owner/library.rs`).
use crate::Error;
use std::{
    sync::mpsc::{SyncSender, sync_channel},
    thread,
};

/// Start the worker thread `name`, which calls `run` with each job sent on the answered channel, in
/// order, until the channel closes. The channel holds one job, so a sender that sends only while
/// the worker is idle never waits.
pub(crate) fn start<T: Send + 'static>(
    name: &str,
    mut run: impl FnMut(T) + Send + 'static,
) -> Result<SyncSender<T>, Error> {
    let (sender, receiver) = sync_channel(1);
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            while let Ok(job) = receiver.recv() {
                run(job);
            }
        })
        .map_err(|error| Error::resource_limit(format!("cannot start {name}: {error}")))?;
    Ok(sender)
}
