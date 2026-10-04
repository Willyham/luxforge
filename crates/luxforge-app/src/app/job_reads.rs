//! Following a queued or running job without a message for every look at it.
//!
//! The core pushes no client anything about a job, so a client that wants to know when one moved
//! or ended reads it. The desktop reads a live job every 100 ms, and it must not pay for a read
//! that found nothing new: iced rebuilds the view and asks for a redraw after every message it
//! processes, whatever `update` did with it, so a tick message and a read message that change
//! nothing still cost two view rebuilds. The reads therefore happen inside the subscription's
//! stream, off the update loop, and only a read that has something for the desktop to apply
//! becomes a message.
//!
//! The stream runs on iced's executor like any subscription. Each turn it waits for the next
//! tick of its interval (the first is at once), reads the job through the owner (`job.read`, the
//! same call the desktop's read tasks made), asks a [`Watch`] whether the read is news, and yields
//! a message only when it is; otherwise it waits for the next tick without yielding, so the update
//! loop is not woken. The read blocks an executor thread for the owner's brief answer, exactly as
//! an [`owner_task`](crate::app::tasks::owner_task) does, and never the update loop. The runtime
//! pulls the stream, so a desktop that is behind holds one read and no queue. Dropping the
//! subscription drops the stream, which is how the desktop ends it when the job is no longer
//! live; a stream that has yielded a job's end yields nothing more, so no read follows the end
//! before the desktop drops it.
use iced::futures::{
    StreamExt,
    stream::{self, BoxStream},
};
use luxforge_core::{ClientId, OwnerHandle};
use std::{
    hash::{Hash, Hasher},
    time::Duration,
};
use tokio::time::{Interval, MissedTickBehavior};

/// What identifies a reader in iced's subscription table, and what it reads through. The identity
/// is hashed alone: the owner handle and the client are carried to the stream's builder and are
/// never part of the subscription's identity, so the same job is the same subscription, with the
/// same stream and the same memory of what it has sent, however often the desktop rebuilds its
/// subscriptions.
pub(crate) struct Reader<K> {
    pub(crate) identity: K,
    pub(crate) owner: OwnerHandle,
    pub(crate) client: ClientId,
}

impl<K: Hash> Hash for Reader<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity.hash(state);
    }
}

/// What a [`Watch`] decides about one read of a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The record is the one the last yielded read carried, and the job is still live: there is
    /// nothing for the desktop to apply.
    Skip,
    /// The first read, or a record that differs from the last one yielded: send it.
    Yield,
    /// The job ended, or the read failed: send it, and read this job no more.
    Last,
}

/// What a reader remembers of one job: the last live record it sent, and whether it is finished
/// with the job.
#[derive(Clone, Debug)]
pub(crate) struct Watch<R> {
    last: Option<R>,
    finished: bool,
}

impl<R> Default for Watch<R> {
    fn default() -> Self {
        Self {
            last: None,
            finished: false,
        }
    }
}

impl<R: Clone + PartialEq> Watch<R> {
    /// The reader sent this job's end or a failed read, and reads it no more.
    pub(crate) fn finished(&self) -> bool {
        self.finished
    }

    /// Decide on one read. `ended` says whether a record is the job's last. A failed read is the
    /// last, as is any ended record, and each is sent once; a live record is sent when it is the
    /// first or differs from the last one sent (progress included).
    pub(crate) fn observe(
        &mut self,
        read: &Result<R, String>,
        ended: impl FnOnce(&R) -> bool,
    ) -> Verdict {
        if self.finished {
            return Verdict::Skip;
        }
        match read {
            Err(_) => {
                self.finished = true;
                Verdict::Last
            }
            Ok(record) if ended(record) => {
                self.finished = true;
                Verdict::Last
            }
            Ok(record) if self.last.as_ref() == Some(record) => Verdict::Skip,
            Ok(record) => {
                self.last = Some(record.clone());
                Verdict::Yield
            }
        }
    }
}

/// What one pass of a reader came to.
pub(crate) enum Pass<M> {
    /// Nothing new: the reader waits for the next tick and reads again, yielding nothing.
    Quiet,
    /// Tell the desktop this, and read again at the next tick.
    Send(M),
    /// Tell the desktop this, and stop: every job the reader follows has ended.
    Last(M),
}

impl<M> Pass<M> {
    /// The pass a [`Verdict`] makes of `message`, built only when there is something to send.
    pub(crate) fn of(verdict: Verdict, message: impl FnOnce() -> M) -> Self {
        match verdict {
            Verdict::Skip => Self::Quiet,
            Verdict::Yield => Self::Send(message()),
            Verdict::Last => Self::Last(message()),
        }
    }
}

/// A reader's stream between two messages.
struct Reads<P> {
    /// Created on the first poll, inside the runtime that owns the timer.
    ticks: Option<Interval>,
    pass: P,
    finished: bool,
}

/// The stream of a reader that makes one `pass` at once and then one every `interval`, and yields
/// only the messages its passes make. It ends after a pass that was the last, and an interval that
/// a slow read overran skips the ticks it missed rather than bursting.
pub(crate) fn reads<M: Send + 'static>(
    interval: Duration,
    pass: impl FnMut() -> Pass<M> + Send + 'static,
) -> BoxStream<'static, M> {
    stream::unfold(
        Reads {
            ticks: None,
            pass,
            finished: false,
        },
        move |mut reads| async move {
            if reads.finished {
                return None;
            }
            let ticks = reads.ticks.get_or_insert_with(|| {
                let mut ticks = tokio::time::interval(interval);
                ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
                ticks
            });
            loop {
                let _ = ticks.tick().await;
                match (reads.pass)() {
                    Pass::Quiet => {}
                    Pass::Send(message) => return Some((message, reads)),
                    Pass::Last(message) => {
                        reads.finished = true;
                        return Some((message, reads));
                    }
                }
            }
        },
    )
    .fuse()
    .boxed()
}
