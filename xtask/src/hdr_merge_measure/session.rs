//! One catalog owner in the worker's own process, as an agent reaches it: every request is the JSON
//! API's method and parameters through [`OwnerHandle::call`], and every job is followed on the
//! owner's activity board, as `catalog-measure`'s core is.
//!
//! The session holds two watches on the board. One follows the catalog's jobs and wakes the worker
//! through a channel of one slot when any of them begins, progresses or ends, so a merge's phases
//! and its `Reading k of n` and `Merging k of m` progress are seen as they change rather than by
//! polling; the wait on the channel is bounded, so the samples [`MergeRun`] takes between changes
//! are taken at least every [`SAMPLE_EVERY`]. The other follows only HDR merges and counts every
//! change to one ([`Session::merge_activity`]): undo, redo, reopening and rendering a saved result
//! must leave it at zero.
use super::*;
use luxforge_core::{
    ApiRequest, ClientId, OwnerHandle,
    activity::{ActivitySnapshot, ActivityWatch},
    catalog_types::jobs::{HDR_MERGE, catalog_job},
};
use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, sync_channel},
    },
    time::{Duration, Instant},
};

/// Who the worker's changes are recorded as.
pub const ACTOR: &str = "hdr-merge-measure";
/// The longest the worker waits between two looks at the board, and so between two samples of the
/// process's memory and the merge's accounting while a merge runs.
pub const SAMPLE_EVERY: Duration = Duration::from_millis(100);
/// How long any one job may take before the run gives up on it.
pub const JOB_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// One owner over one catalog, its client and its two watches.
pub struct Session {
    owner: OwnerHandle,
    join: Option<std::thread::JoinHandle<()>>,
    client: ClientId,
    jobs: ActivityWatch,
    woken: Receiver<()>,
    merges: ActivityWatch,
    merge_changes: Arc<AtomicU64>,
    requests: Cell<u64>,
    pub catalog: PathBuf,
}

impl Session {
    /// An owner over `catalog`, made when it does not exist.
    pub fn open(catalog: &Path) -> Result<Self> {
        let (owner, join) = OwnerHandle::start(catalog)
            .map_err(|error| format!("the core cannot open {}: {error}", catalog.display()))?;
        let (wake, woken) = sync_channel(1);
        let jobs = owner
            .activity()
            .watch(
                |entry| catalog_job(&entry.kind).is_some(),
                Arc::new(move || {
                    let _ = wake.try_send(());
                }),
            )
            .map_err(|error| format!("the activity board cannot be watched: {error}"))?;
        let merge_changes = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&merge_changes);
        let merges = owner
            .activity()
            .watch(
                |entry| entry.kind == HDR_MERGE.activity,
                Arc::new(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .map_err(|error| format!("the activity board cannot be watched: {error}"))?;
        let client = owner.register();
        let session = Self {
            owner,
            join: Some(join),
            client,
            jobs,
            woken,
            merges,
            merge_changes,
            requests: Cell::new(0),
            catalog: catalog.into(),
        };
        // A watch hears the next change only after a read.
        session.jobs.read();
        session.merges.read();
        Ok(session)
    }

    /// `<catalog stem>.artifacts` beside the catalog.
    pub fn artifact_root(&self) -> PathBuf {
        artifact_root(&self.catalog)
    }

    /// One request that must succeed.
    pub fn ask(&self, method: &str, params: Value) -> Result<Value> {
        let response = self.answer(method, params)?;
        match response.error {
            Some(error) => Err(format!("{method}: {}: {}", error.code, error.message).into()),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }

    /// One request's whole answer, a refusal included.
    pub fn answer(&self, method: &str, params: Value) -> Result<luxforge_core::ApiResponse> {
        let sequence = self.requests.get();
        self.requests.set(sequence + 1);
        Ok(self
            .owner
            .call(
                self.client,
                ApiRequest {
                    id: format!("{ACTOR}-{sequence}"),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .map_err(|error| format!("{method}: {error}"))?)
    }

    /// A mutation envelope with a request id of its own.
    pub fn mutation(&self, tag: &str) -> Value {
        let sequence = self.requests.get();
        self.requests.set(sequence + 1);
        json!({"request_id": format!("{ACTOR}-{tag}-{sequence}-{}", std::process::id()), "actor": ACTOR})
    }

    pub fn owner(&self) -> (&OwnerHandle, ClientId) {
        (&self.owner, self.client)
    }

    /// The board's catalog jobs now, with the watch armed for their next change.
    pub fn board(&self) -> ActivitySnapshot {
        self.jobs.read()
    }

    /// Sleep until a catalog job changes, or `bound` passes.
    pub fn pause(&self, bound: Duration) {
        let _ = self.woken.recv_timeout(bound);
    }

    /// Changes to HDR merges on the board since the session opened, with the watch armed again.
    pub fn merge_activity(&self) -> u64 {
        self.merges.read();
        self.merge_changes.load(Ordering::SeqCst)
    }

    /// Wait until no catalog job runs: what the worker does before it times anything.
    pub fn settle(&self) -> Result {
        let started = Instant::now();
        while !self.board().active.is_empty() {
            ensure(
                started.elapsed() < JOB_DEADLINE,
                "the catalog's jobs did not settle",
            )?;
            self.pause(SAMPLE_EVERY);
        }
        Ok(())
    }

    /// Wait for `job` to end and answer its `job.read` record and when it ended: the board's own
    /// record of the end when it kept one, else when the worker saw it.
    pub fn wait_job(&self, job: &str) -> Result<(Value, Instant)> {
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
                    return Ok((record, at));
                }
            }
            ensure(
                started.elapsed() < JOB_DEADLINE,
                format!("the job {job} did not end"),
            )?;
            self.pause(SAMPLE_EVERY);
        }
    }

    /// Wait for the job a method answered, which must end `ready`: its result.
    pub fn finished(&self, answer: &Value) -> Result<Value> {
        let job = job_id(answer)?;
        let (record, _) = self.wait_job(&job)?;
        ensure(
            record["status"] == "ready",
            format!("the job {job} ended {record}"),
        )?;
        Ok(record["result"].clone())
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

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// `<catalog stem>.artifacts` beside `catalog`.
pub fn artifact_root(catalog: &Path) -> PathBuf {
    let stem = catalog.file_stem().map_or_else(
        || "catalog".into(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    catalog.with_file_name(format!("{stem}.artifacts"))
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
