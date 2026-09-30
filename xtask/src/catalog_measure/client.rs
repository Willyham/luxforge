//! The core in the harness's own process, as the `select` and `loupe` scenarios reach it: an
//! [`OwnerHandle`] over a scratch catalog and the harness's clients, each request the JSON API's
//! own method and parameters.
//!
//! Nothing here polls in a loop to learn that work ended. A job is waited for on the owner's
//! activity board ([`Core::wait_job`]): the harness watches the board's catalog jobs, and each
//! begin, progress or end of one wakes it once through a channel of one slot, after which it reads
//! the board and, when the job is no longer running, `job.read`. The wait on the channel is bounded
//! (a quarter of a second), so a job that ends without its board entry changing is still read. The
//! end of a job that ran a quarter of a second or more is taken from the board's own record of it
//! (`ended_ms_ago`), so the figure does not depend on when the harness woke.
use crate::*;
use luxforge_core::{
    ApiRequest, ClientId, OwnerHandle,
    activity::{ActivitySnapshot, ActivityWatch},
    catalog_types::jobs::{PREVIEW_EXTRACT, catalog_job},
};
use std::{
    cell::Cell,
    sync::{
        Arc,
        mpsc::{Receiver, sync_channel},
    },
    time::{Duration, Instant},
};

/// Who the harness's changes are recorded as.
const ACTOR: &str = "catalog-measure";
/// The longest the harness sleeps between two reads of the board while it waits.
const WAKE_BOUND: Duration = Duration::from_millis(250);

/// A job that has ended: its `job.read` record, and when it ended.
pub struct Ended {
    pub record: Value,
    pub at: Instant,
}

/// One owner over one catalog, with the harness's client and its watch on the owner's board.
pub struct Core {
    owner: OwnerHandle,
    join: Option<std::thread::JoinHandle<()>>,
    client: ClientId,
    watch: ActivityWatch,
    woken: Receiver<()>,
    requests: Cell<u64>,
}

impl Core {
    /// An owner over `catalog`, which it creates when it does not exist.
    pub fn open(catalog: &Path) -> Result<Self> {
        let (owner, join) = OwnerHandle::start(catalog)
            .map_err(|error| format!("the core cannot open {}: {error}", catalog.display()))?;
        let (wake, woken) = sync_channel(1);
        let watch = owner
            .activity()
            .watch(
                |entry| catalog_job(&entry.kind).is_some(),
                Arc::new(move || {
                    let _ = wake.try_send(());
                }),
            )
            .map_err(|error| format!("the activity board cannot be watched: {error}"))?;
        let client = owner.register();
        Ok(Self {
            owner,
            join: Some(join),
            client,
            watch,
            woken,
            requests: Cell::new(0),
        })
    }

    /// Another client of the same owner.
    pub fn register(&self) -> ClientId {
        self.owner.register()
    }

    /// One request through the harness's client.
    pub fn ask(&self, method: &str, params: Value) -> Result<Value> {
        self.ask_as(self.client, method, params)
    }

    /// One request through `client`.
    pub fn ask_as(&self, client: ClientId, method: &str, params: Value) -> Result<Value> {
        let sequence = self.requests.get();
        self.requests.set(sequence + 1);
        let response = self
            .owner
            .call(
                client,
                ApiRequest {
                    id: format!("{ACTOR}-{sequence}"),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .map_err(|error| format!("{method}: {error}"))?;
        match response.error {
            Some(error) => Err(format!("{method}: {}: {}", error.code, error.message).into()),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }

    /// A mutation envelope with a request id of its own.
    pub fn mutation(&self) -> Value {
        let sequence = self.requests.get();
        self.requests.set(sequence + 1);
        json!({"request_id": format!("{ACTOR}-change-{sequence}"), "actor": ACTOR})
    }

    /// The board now, with the watch armed for its next change.
    fn board(&self) -> ActivitySnapshot {
        self.watch.read()
    }

    /// Sleep until the board's catalog jobs change, or [`WAKE_BOUND`] passes.
    pub fn pause(&self) {
        let _ = self.woken.recv_timeout(WAKE_BOUND);
    }

    /// Whether `job` is running on the board.
    pub fn running(&self, job: &str) -> bool {
        self.board()
            .active
            .iter()
            .any(|entry| entry.entry.job_id.as_deref() == Some(job))
    }

    /// The newest entry the board has held so far, to tell the work a request begins from what
    /// was there before it.
    pub fn newest_entry(&self) -> u64 {
        let board = self.board();
        board
            .active
            .iter()
            .map(|entry| entry.entry.id)
            .chain(board.recent.iter().map(|entry| entry.entry.id))
            .max()
            .unwrap_or(0)
    }

    /// The preview lane's view job (`Reading previews`) a `browse.view` began after the board's
    /// entry `after`, running or ended; none when the view lacked no preview.
    pub fn view_job_since(&self, after: u64) -> Option<String> {
        let board = self.board();
        board
            .active
            .iter()
            .map(|entry| &entry.entry)
            .chain(board.recent.iter().map(|entry| &entry.entry))
            .filter(|entry| entry.kind == PREVIEW_EXTRACT.activity && entry.id > after)
            .max_by_key(|entry| entry.id)
            .and_then(|entry| entry.job_id.clone())
    }

    /// Wait for `job` to end, within `deadline`.
    pub fn wait_job(&self, job: &str, deadline: Duration) -> Result<Ended> {
        let started = Instant::now();
        loop {
            let now = Instant::now();
            let board = self.board();
            let recent = board
                .recent
                .iter()
                .find(|entry| entry.entry.job_id.as_deref() == Some(job));
            let active = board
                .active
                .iter()
                .any(|entry| entry.entry.job_id.as_deref() == Some(job));
            if recent.is_some() || !active {
                let record = self.ask("job.read", json!({"job_id": job}))?;
                if !matches!(record["status"].as_str(), Some("queued" | "running")) {
                    let at = recent.map_or(now, |entry| {
                        now.checked_sub(Duration::from_millis(entry.ended_ms_ago))
                            .unwrap_or(now)
                    });
                    return Ok(Ended { record, at });
                }
            }
            ensure(
                started.elapsed() < deadline,
                format!("the job {job} did not end within {} s", deadline.as_secs()),
            )?;
            self.pause();
        }
    }

    /// Stop the owner and wait for its thread.
    pub fn close(mut self) -> Result {
        self.stop()
    }

    fn stop(&mut self) -> Result {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        self.owner.stop();
        join.join()
            .map_err(|_| "the catalog owner's thread panicked".into())
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The `job_id` a method that starts a job answered.
pub fn job_id(answer: &Value) -> Result<String> {
    answer["job_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("the answer names no job: {answer}").into())
}

/// Milliseconds.
pub fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// How long `body` took, and what it answered.
pub fn timed<T>(body: impl FnOnce() -> Result<T>) -> Result<(f64, T)> {
    let started = Instant::now();
    let answer = body()?;
    Ok((ms(started.elapsed()), answer))
}
