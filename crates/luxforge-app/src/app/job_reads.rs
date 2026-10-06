//! Bounded, event-driven readers: first, changed and terminal job records only.
use iced::futures::{
    StreamExt,
    stream::{self, BoxStream},
};
use luxforge_core::ApiRequest;
use luxforge_core::jobs::JOB_WAIT;
use luxforge_core::{ClientId, OwnerHandle};
use serde_json::{Value, json};
use std::future::Future;
use std::hash::{Hash, Hasher};

/// What identifies a reader in iced's subscription table, and what it reads through. The job identity and presentation visibility
/// are hashed: the owner handle and the client are carried to the stream's builder and are
/// never part of the subscription's identity, so the same job is the same subscription, with the
/// same stream and the same memory of what it has sent across unrelated rebuilds. A visibility
/// transition reconciles the current record once.
pub(crate) struct Reader<K> {
    pub(crate) identity: K,
    pub(crate) owner: OwnerHandle,
    pub(crate) client: ClientId,
    pub(crate) presentation_visible: bool,
}

impl<K: Hash> Hash for Reader<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity.hash(state);
        self.presentation_visible.hash(state);
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
    #[cfg(test)]
    pub(crate) fn finished(&self) -> bool {
        self.finished
    }

    /// Decide on one read. `ended` says whether a record is the job's last. A failed read is the
    /// last, as is any ended record, and each is sent once; a live record is sent when it is the
    /// first or differs from the last one sent (progress included).
    #[cfg(test)]
    pub(crate) fn observe(
        &mut self,
        read: &Result<R, String>,
        ended: impl FnOnce(&R) -> bool,
    ) -> Verdict {
        self.observe_filtered(read, ended, PartialEq::eq)
    }

    pub(crate) fn observe_filtered(
        &mut self,
        read: &Result<R, String>,
        ended: impl FnOnce(&R) -> bool,
        same: impl FnOnce(&R, &R) -> bool,
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
            Ok(record) if self.last.as_ref().is_some_and(|last| same(last, record)) => {
                Verdict::Skip
            }
            Ok(record) => {
                self.last = Some(record.clone());
                Verdict::Yield
            }
        }
    }
}

/// What one pass of a reader came to.
pub(crate) enum Pass<M> {
    /// Nothing new: await the next relevant change without yielding.
    Quiet,
    /// Tell the desktop this, then await the next relevant change.
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

/// One notification-driven observation through the same command method JSON clients use.
pub(crate) async fn wait(
    owner: &OwnerHandle,
    client: ClientId,
    job: &str,
    after: Option<u64>,
) -> Result<(u64, Value), String> {
    let response = owner
        .call_async(
            client,
            ApiRequest {
                id: "desktop-job-wait".into(),
                method: JOB_WAIT.into(),
                params: json!({"job_id":job,"after":after}),
                token: None,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    if let Some(error) = response.error {
        return Err(error.message);
    }
    let mut value = response
        .result
        .ok_or_else(|| "job.wait omitted its answer".to_owned())?;
    let change = value["change"]
        .as_u64()
        .ok_or_else(|| "job.wait omitted its change token".to_owned())?;
    let record = value
        .get_mut("job")
        .ok_or_else(|| "job.wait omitted its record".to_owned())?
        .take();
    Ok((change, record))
}

/// Await each pass's relevant change; there is no clock, polling, or thread per reader.
pub(crate) fn reads<M: Send + 'static, F: Future<Output = Pass<M>> + Send + 'static>(
    pass: impl FnMut() -> F + Send + 'static,
) -> BoxStream<'static, M> {
    stream::unfold((pass, false), |(mut pass, finished)| async move {
        if finished {
            return None;
        }
        loop {
            match pass().await {
                Pass::Quiet => {}
                Pass::Send(message) => return Some((message, (pass, false))),
                Pass::Last(message) => return Some((message, (pass, true))),
            }
        }
    })
    .fuse()
    .boxed()
}

/// Hidden readers retain lifecycle and partial/result changes, coalescing only presentation progress.
pub(crate) fn same_without_progress(one: &Value, two: &Value) -> bool {
    let (Some(one), Some(two)) = (one.as_object(), two.as_object()) else {
        return one == two;
    };
    one.iter()
        .filter(|(key, _)| key.as_str() != "progress")
        .eq(two.iter().filter(|(key, _)| key.as_str() != "progress"))
}
