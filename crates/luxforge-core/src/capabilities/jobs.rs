//! The capability worker: two lanes, `transfer` (resource installs and removals) and `module`
//! (activation, deactivation and, later, tasks). Each lane is one thread, spawned on its first job,
//! that blocks on its channel while idle, runs one job at a time and posts the result into the
//! catalog owner's own channel; nothing polls. The owner keeps each lane's waiting jobs itself, at
//! most [`LANE_QUEUE`] of them, so a queued job can be cancelled or superseded without touching the
//! thread, and it keeps at most [`FINISHED_RECORDS`] finished records. Progress travels the other
//! way through a [`JobControl`] the worker writes and the owner reads when a client asks.
use crate::{
    Error, ErrorKind, JobId,
    activity::{Activity, ActivityBoard, ActivitySpec, Outcome},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

/// Jobs that may wait on one lane behind the running one.
pub const LANE_QUEUE: usize = 4;
/// Finished job records the owner keeps; the oldest is forgotten first.
pub const FINISHED_RECORDS: usize = 32;

/// The capability job methods.
pub const JOB_READ: &str = "module.job.read";
pub const JOB_CANCEL: &str = "module.job.cancel";

/// The reason a job is cancelled when a grant it depends on is revoked.
pub const PERMISSION_REVOKED: &str = "permission revoked";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    Transfer,
    Module,
}

impl Lane {
    pub fn name(self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
            Self::Module => "module",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Transfer => 0,
            Self::Module => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobKind {
    Activate,
    Deactivate,
    Install,
    Remove,
    Task,
}

impl JobKind {
    pub fn lane(self) -> Lane {
        match self {
            Self::Install | Self::Remove => Lane::Transfer,
            Self::Activate | Self::Deactivate | Self::Task => Lane::Module,
        }
    }
}

/// The shared job lifecycle (`crate::JobStatus`), re-exported under its historical name here: a
/// capability job reaches every value, `superseded` only for a queued activation a deactivation
/// replaced before it ran.
pub use crate::JobStatus;

/// How far a job has come. The one progress model every activity publisher shares
/// (`crate::activity::ActivityProgress`): a capability job reports through the activity its lane
/// begins rather than keeping its own copy, and `module.job.read` answers with what the board
/// carries.
pub use crate::activity::ActivityProgress as JobProgress;

/// A failed or cancelled job's error, as a client reads it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl From<&Error> for JobError {
    fn from(error: &Error) -> Self {
        Self {
            code: error.kind.code().to_owned(),
            message: error.detail.clone(),
            data: error.data.as_deref().cloned(),
        }
    }
}

/// One job as `module.job.read` returns it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: JobId,
    pub kind: JobKind,
    pub module_id: String,
    /// The resource an install or removal is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    pub status: JobStatus,
    pub progress: JobProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobError>,
    /// The request that started the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// The request a job, or a change it causes, is announced under: the method and request identity a
/// client watching `events.since` sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub method: String,
    pub request_id: String,
}

impl Origin {
    pub fn new(method: &str, request_id: &str) -> Self {
        Self {
            method: method.to_owned(),
            request_id: request_id.to_owned(),
        }
    }
}

/// The state a job's worker and the owner share: the cancel flag with its reason, set by the
/// owner, and the activity it publishes to once its lane picks it up, which carries its progress.
/// A job has no activity before that: nothing reports progress before it runs, and a queued job's
/// progress reads empty.
#[derive(Debug, Default)]
pub struct JobControl {
    cancelled: AtomicBool,
    reason: Mutex<Option<String>>,
    activity: Mutex<Option<Activity>>,
}

impl JobControl {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Ask the job to stop at its next checkpoint. The first reason given is the one reported.
    pub(crate) fn cancel(&self, reason: &str) {
        let mut held = self.reason.lock().expect("job cancel reason");
        if held.is_none() {
            *held = Some(reason.to_owned());
        }
        self.cancelled.store(true, Ordering::Release);
    }

    /// The `cancelled` error naming why the job was stopped.
    pub fn cancelled_error(&self) -> Error {
        let reason = self
            .reason
            .lock()
            .expect("job cancel reason")
            .clone()
            .unwrap_or_else(|| "the job was cancelled".to_owned());
        Error::new(ErrorKind::Cancelled, reason)
    }

    /// `Err(cancelled)` once the job has been asked to stop.
    pub fn checkpoint(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(self.cancelled_error())
        } else {
            Ok(())
        }
    }

    /// Attach the activity this job publishes to, once its lane picks it up. Owner-only: called
    /// exactly once, from `Jobs::dispatch`, before the job's work reaches its worker thread.
    pub(crate) fn begin_activity(&self, activity: Activity) {
        *self.activity.lock().expect("job activity") = Some(activity);
    }

    /// Report progress: forwarded straight to the job's activity, the one progress model every
    /// publisher on the board shares. Called from the job's own worker thread.
    pub fn set_progress(&self, fraction: Option<f64>, message: &str) {
        if let Some(activity) = self.activity.lock().expect("job activity").as_ref() {
            activity.progress(fraction, message);
        }
    }

    /// The progress this job currently reports on the board, for `module.job.read` to answer
    /// with while it runs. A job with no activity yet reports nothing.
    fn progress(&self) -> JobProgress {
        self.activity
            .lock()
            .expect("job activity")
            .as_ref()
            .and_then(Activity::progress_snapshot)
            .unwrap_or_default()
    }

    /// End the job's activity with this outcome. Owner-only, called once a job's result is in; a
    /// job that never started running has no activity to end.
    pub(crate) fn finish_activity(&self, outcome: Outcome) {
        if let Some(activity) = self.activity.lock().expect("job activity").take() {
            activity.finish(outcome);
        }
    }
}

/// What a lane runs: everything the job needs, moved to the worker, returning the job's result.
pub(crate) type Work = Box<dyn FnOnce() -> Result<Value, Error> + Send>;
/// Posts one finished job back into the owner's channel. Called on the lane thread.
pub(crate) type Deliver = Arc<dyn Fn(JobId, Result<Value, Error>) + Send + Sync>;

/// Whether a job counts against its lane's bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Refused with `resource-limit` when [`LANE_QUEUE`] jobs already wait.
    Bounded,
    /// Always admitted. Only a deactivation is: it releases what a module holds, it is at most one
    /// per module because a module with one pending already reads inactive, and refusing it would
    /// keep memory held because a queue was busy.
    Always,
}

/// A job the host is about to queue. Its identity is chosen by the caller, so work that names its
/// own job, such as an install's staging directory, can be built before it is queued.
pub(crate) struct NewJob {
    pub job_id: JobId,
    pub kind: JobKind,
    pub module_id: String,
    pub resource_id: Option<String>,
    pub origin: Option<Origin>,
    /// Grants the job runs under; revoking one cancels it.
    pub grants: Vec<String>,
    pub admission: Admission,
}

struct Entry {
    record: JobRecord,
    control: Arc<JobControl>,
    /// Present while the job waits; taken when it is dispatched.
    work: Option<Work>,
    grants: Vec<String>,
    origin: Option<Origin>,
}

impl Entry {
    fn read(&self) -> JobRecord {
        let mut record = self.record.clone();
        if !record.status.is_finished() {
            record.progress = self.control.progress();
        }
        record
    }
}

struct Dispatch {
    job_id: JobId,
    control: Arc<JobControl>,
    work: Work,
}

struct LaneState {
    lane: Lane,
    waiting: VecDeque<JobId>,
    running: Option<JobId>,
    sender: Option<SyncSender<Dispatch>>,
    thread: Option<JoinHandle<()>>,
}

impl LaneState {
    fn new(lane: Lane) -> Self {
        Self {
            lane,
            waiting: VecDeque::new(),
            running: None,
            sender: None,
            thread: None,
        }
    }
}

/// A job that just finished, for the host to act on.
pub(crate) struct Finished {
    pub record: JobRecord,
    pub origin: Option<Origin>,
}

/// What a cancel did.
pub(crate) enum Cancelled {
    /// The job was waiting and is now `cancelled`.
    Removed(JobRecord),
    /// The job is running and will stop at its next checkpoint.
    Requested(JobRecord),
    /// The job had already finished; nothing changed.
    Finished(JobRecord),
}

/// The activity one capability job publishes, from the moment its lane picks it up to the moment
/// it reports back. `detail` names the resource an install or removal is for, or otherwise the
/// module, so the panel's job row says which one is running without a dynamic label.
fn capability_activity(record: &JobRecord) -> ActivitySpec {
    let (kind, label): (&'static str, &'static str) = match record.kind {
        JobKind::Activate => ("module.activate", "Activating module"),
        JobKind::Deactivate => ("module.deactivate", "Deactivating module"),
        JobKind::Install => ("module.resource.install", "Installing resource"),
        JobKind::Remove => ("module.resource.remove", "Removing resource"),
        JobKind::Task => ("module.task", "Running task"),
    };
    let detail = match &record.resource_id {
        Some(resource_id) => format!("{}/{resource_id}", record.module_id),
        None => record.module_id.clone(),
    };
    ActivitySpec {
        kind,
        label,
        detail: Some(detail),
        asset_id: None,
        job_id: Some(record.job_id.to_string()),
    }
}

/// The owner's job table and lanes.
pub(crate) struct Jobs {
    entries: HashMap<JobId, Entry>,
    finished: VecDeque<JobId>,
    lanes: [LaneState; 2],
    deliver: Deliver,
    /// Where every job publishes from the moment its lane picks it up, so `activity.list` shows
    /// capability work beside source preparation and analysis.
    board: Arc<ActivityBoard>,
}

impl Jobs {
    pub(crate) fn new(deliver: Deliver, board: Arc<ActivityBoard>) -> Self {
        Self {
            entries: HashMap::new(),
            finished: VecDeque::new(),
            lanes: [LaneState::new(Lane::Transfer), LaneState::new(Lane::Module)],
            deliver,
            board,
        }
    }

    /// The board this table publishes every running job to, for a test that wants to read what a
    /// job reported without going through `read`'s own `JobRecord` copy.
    #[cfg(test)]
    pub(crate) fn board(&self) -> &Arc<ActivityBoard> {
        &self.board
    }

    /// Queue a job, or refuse it with `resource-limit` when its lane is full. The job starts at
    /// once when its lane is idle.
    pub(crate) fn submit(
        &mut self,
        job: NewJob,
        control: Arc<JobControl>,
        work: Work,
    ) -> Result<JobRecord, Error> {
        let lane = job.kind.lane();
        let state = &self.lanes[lane.index()];
        // Deactivations are admitted beyond the bound, so they do not take a bounded place either.
        if job.admission == Admission::Bounded
            && state
                .waiting
                .iter()
                .filter(|id| {
                    self.entries
                        .get(*id)
                        .is_some_and(|entry| entry.record.kind != JobKind::Deactivate)
                })
                .count()
                >= LANE_QUEUE
        {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!("the {} lane is full", lane.name()),
            ));
        }
        let job_id = job.job_id;
        let record = JobRecord {
            job_id: job_id.clone(),
            kind: job.kind,
            module_id: job.module_id,
            resource_id: job.resource_id,
            status: JobStatus::Queued,
            progress: JobProgress::default(),
            result: None,
            error: None,
            request_id: job.origin.as_ref().map(|origin| origin.request_id.clone()),
        };
        self.entries.insert(
            job_id.clone(),
            Entry {
                record,
                control,
                work: Some(work),
                grants: job.grants,
                origin: job.origin,
            },
        );
        self.lanes[lane.index()].waiting.push_back(job_id.clone());
        if let Err(error) = self.dispatch(lane) {
            self.lanes[lane.index()].waiting.retain(|id| id != &job_id);
            self.entries.remove(&job_id);
            return Err(error);
        }
        Ok(self.entries[&job_id].read())
    }

    /// Start the next waiting job of an idle lane, spawning its thread on first use.
    fn dispatch(&mut self, lane: Lane) -> Result<(), Error> {
        let state = &mut self.lanes[lane.index()];
        if state.running.is_some() {
            return Ok(());
        }
        let Some(job_id) = state.waiting.pop_front() else {
            return Ok(());
        };
        if state.sender.is_none() {
            let (sender, receiver) = sync_channel(1);
            let deliver = self.deliver.clone();
            let thread = thread::Builder::new()
                .name(format!("luxforge-{}-lane", state.lane.name()))
                .spawn(move || lane_worker(receiver, deliver))
                .map_err(|error| {
                    Error::new(
                        ErrorKind::ResourceLimit,
                        format!("cannot start the {} lane: {error}", lane.name()),
                    )
                });
            match thread {
                Ok(thread) => {
                    state.sender = Some(sender);
                    state.thread = Some(thread);
                }
                Err(error) => {
                    state.waiting.push_front(job_id);
                    return Err(error);
                }
            }
        }
        let entry = self
            .entries
            .get_mut(&job_id)
            .expect("a waiting job has an entry");
        let work = entry.work.take().expect("a waiting job holds its work");
        entry.record.status = JobStatus::Running;
        entry
            .control
            .begin_activity(self.board.begin(capability_activity(&entry.record)));
        let dispatch = Dispatch {
            job_id: job_id.clone(),
            control: entry.control.clone(),
            work,
        };
        state.running = Some(job_id);
        // The lane is idle, so its one-slot channel is empty and this never blocks.
        let sent = state
            .sender
            .as_ref()
            .expect("the lane thread was started")
            .send(dispatch);
        if sent.is_err() {
            let job_id = state.running.take().expect("the job was just started");
            self.fail(&job_id, Error::new(ErrorKind::Internal, "the lane stopped"));
        }
        Ok(())
    }

    /// Record an outcome the lane never produced, and start the next job. A job that had not
    /// started running yet has no activity to end; ending one that had is harmless either way.
    fn fail(&mut self, job_id: &JobId, error: Error) {
        if let Some(entry) = self.entries.get_mut(job_id) {
            entry.record.status = JobStatus::Failed;
            entry.record.error = Some(JobError::from(&error));
            entry.control.finish_activity(Outcome::Failed);
        }
        self.retire(job_id);
    }

    /// One job as a client reads it, with its live progress.
    pub(crate) fn read(&self, job_id: &JobId) -> Option<JobRecord> {
        self.entries.get(job_id).map(Entry::read)
    }

    /// Cancel a job: a waiting one is removed at once, a running one is asked to stop at its next
    /// checkpoint, and a finished one is left as it is.
    pub(crate) fn cancel(&mut self, job_id: &JobId, reason: &str) -> Option<Cancelled> {
        let entry = self.entries.get_mut(job_id)?;
        match entry.record.status {
            JobStatus::Queued => {
                let lane = entry.record.kind.lane();
                entry.work = None;
                entry.control.cancel(reason);
                entry.record.status = JobStatus::Cancelled;
                entry.record.error = Some(JobError::from(&entry.control.cancelled_error()));
                let record = entry.read();
                self.lanes[lane.index()].waiting.retain(|id| id != job_id);
                self.retire(job_id);
                Some(Cancelled::Removed(record))
            }
            JobStatus::Running => {
                entry.control.cancel(reason);
                Some(Cancelled::Requested(entry.read()))
            }
            _ => Some(Cancelled::Finished(entry.read())),
        }
    }

    /// Replace a waiting activation that a deactivation made pointless. A running or finished job
    /// is left as it is and `None` is returned.
    pub(crate) fn supersede(&mut self, job_id: &JobId) -> Option<JobRecord> {
        let entry = self.entries.get_mut(job_id)?;
        if entry.record.status != JobStatus::Queued {
            return None;
        }
        let lane = entry.record.kind.lane();
        entry.work = None;
        entry.record.status = JobStatus::Superseded;
        let record = entry.read();
        self.lanes[lane.index()].waiting.retain(|id| id != job_id);
        self.retire(job_id);
        Some(record)
    }

    /// A lane reported its running job: record the outcome and start the lane's next job. A result
    /// for a job that is not running is ignored.
    pub(crate) fn complete(
        &mut self,
        job_id: &JobId,
        result: Result<Value, Error>,
    ) -> Option<Finished> {
        let entry = self.entries.get_mut(job_id)?;
        if entry.record.status != JobStatus::Running {
            return None;
        }
        let cancel_requested = entry.control.is_cancelled();
        // Captured from the still-live activity before `finish_activity` ends and takes it, so the
        // record keeps the last progress the board carried rather than an empty one.
        entry.record.progress = entry.control.progress();
        let outcome = Outcome::of(&result);
        match result {
            Ok(value) => {
                entry.record.status = JobStatus::Ready;
                entry.record.result = Some(value);
            }
            Err(error) if error.kind == ErrorKind::Cancelled => {
                entry.record.status = JobStatus::Cancelled;
                // A transport or module reports its own words for a stop; the reason the owner
                // gave is what a client needs to read.
                let error = if cancel_requested {
                    entry.control.cancelled_error()
                } else {
                    error
                };
                entry.record.error = Some(JobError::from(&error));
            }
            Err(error) => {
                entry.record.status = JobStatus::Failed;
                entry.record.error = Some(JobError::from(&error));
            }
        }
        entry.control.finish_activity(outcome);
        let finished = Finished {
            record: entry.record.clone(),
            origin: entry.origin.clone(),
        };
        let lane = entry.record.kind.lane();
        self.lanes[lane.index()].running = None;
        self.retire(job_id);
        if let Err(error) = self.dispatch(lane) {
            // The lane thread exists already, so only a stopped thread gets here; every waiting job
            // fails rather than waiting forever.
            let waiting: Vec<JobId> = self.lanes[lane.index()].waiting.drain(..).collect();
            for job_id in waiting {
                self.fail(&job_id, error.clone());
            }
        }
        Some(finished)
    }

    /// Move a finished job to the bounded ring of finished records.
    fn retire(&mut self, job_id: &JobId) {
        self.finished.push_back(job_id.clone());
        while self.finished.len() > FINISHED_RECORDS {
            if let Some(oldest) = self.finished.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    /// Live and retained records of one module, oldest first.
    pub(crate) fn of_module(&self, module_id: &str) -> Vec<JobRecord> {
        let mut records: Vec<JobRecord> = self
            .entries
            .values()
            .filter(|entry| entry.record.module_id == module_id)
            .map(Entry::read)
            .collect();
        let order = |record: &JobRecord| {
            self.finished
                .iter()
                .position(|id| id == &record.job_id)
                .unwrap_or(usize::MAX)
        };
        records.sort_by_key(|record| (order(record), record.job_id.clone()));
        records
    }

    /// The queued or running job of this kind for this module and resource, if any.
    pub(crate) fn live(
        &self,
        kind: JobKind,
        module_id: &str,
        resource_id: Option<&str>,
    ) -> Option<JobRecord> {
        self.entries
            .values()
            .find(|entry| {
                entry.record.kind == kind
                    && entry.record.module_id == module_id
                    && entry.record.resource_id.as_deref() == resource_id
                    && !entry.record.status.is_finished()
            })
            .map(Entry::read)
    }

    /// The most recently finished job of this kind for this module and resource, if still kept.
    pub(crate) fn last_finished(
        &self,
        kind: JobKind,
        module_id: &str,
        resource_id: Option<&str>,
    ) -> Option<JobRecord> {
        self.finished
            .iter()
            .rev()
            .filter_map(|id| self.entries.get(id))
            .find(|entry| {
                entry.record.kind == kind
                    && entry.record.module_id == module_id
                    && entry.record.resource_id.as_deref() == resource_id
            })
            .map(Entry::read)
    }

    /// Live jobs that run under this grant.
    pub(crate) fn depending_on(&self, grant_id: &str) -> Vec<JobId> {
        let mut ids: Vec<JobId> = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                !entry.record.status.is_finished()
                    && entry.grants.iter().any(|grant| grant == grant_id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// How many lane threads have been started.
    #[cfg(test)]
    pub(crate) fn lanes_started(&self) -> usize {
        self.lanes
            .iter()
            .filter(|lane| lane.thread.is_some())
            .count()
    }

    /// Stop everything: ask running jobs to stop, drop waiting ones, close the lanes and wait for
    /// their threads, which finish at their job's next checkpoint.
    pub(crate) fn shutdown(&mut self) {
        for entry in self.entries.values() {
            if !entry.record.status.is_finished() {
                entry.control.cancel("the editor is closing");
            }
        }
        for lane in &mut self.lanes {
            lane.waiting.clear();
            lane.sender = None;
        }
        for lane in &mut self.lanes {
            if let Some(thread) = lane.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

/// One lane: block for the next job, run it unless it was cancelled on the way, and post its
/// result. A panicking job fails with `internal` instead of taking the lane down.
fn lane_worker(receiver: Receiver<Dispatch>, deliver: Deliver) {
    while let Ok(Dispatch {
        job_id,
        control,
        work,
    }) = receiver.recv()
    {
        let result = if control.is_cancelled() {
            Err(control.cancelled_error())
        } else {
            panic::catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|_| {
                Err(Error::new(
                    ErrorKind::Internal,
                    "the job stopped unexpectedly",
                ))
            })
        };
        deliver(job_id, result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        sync::mpsc::{Receiver, channel},
        time::Duration,
    };

    /// One finished job as a lane delivers it.
    type Completion = (JobId, Result<Value, Error>);

    /// A job table whose completions arrive on a channel the test reads, as the owner's would, and
    /// whose board keeps work of any duration as recent, so a test can read a job's activity right
    /// after it finishes.
    fn jobs() -> (Jobs, Receiver<Completion>) {
        let (sender, receiver) = channel();
        let sender = Mutex::new(sender);
        let deliver: Deliver = Arc::new(move |job_id, result| {
            let _ = sender.lock().unwrap().send((job_id, result));
        });
        (
            Jobs::new(
                deliver,
                ActivityBoard::with_recent_threshold(Duration::ZERO),
            ),
            receiver,
        )
    }

    fn job(kind: JobKind, admission: Admission) -> NewJob {
        NewJob {
            job_id: JobId::new(),
            kind,
            module_id: "test.module".into(),
            resource_id: None,
            origin: Some(Origin::new("module.activate", "request")),
            grants: vec!["grant-a".into()],
            admission,
        }
    }

    /// Work that waits until the test opens its gate, checking its control as it waits.
    fn gated(control: &Arc<JobControl>) -> (Work, SyncSender<()>) {
        let (open, gate) = sync_channel::<()>(1);
        let control = control.clone();
        let work: Work = Box::new(move || {
            loop {
                control.checkpoint()?;
                if gate.recv_timeout(Duration::from_millis(5)).is_ok() {
                    return Ok(json!({"done": true}));
                }
            }
        });
        (work, open)
    }

    fn receive(receiver: &Receiver<Completion>) -> Completion {
        receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("a completion arrived")
    }

    #[test]
    fn a_lane_runs_one_job_holds_four_and_refuses_the_sixth() {
        let (mut jobs, completions) = jobs();
        assert_eq!(jobs.lanes_started(), 0, "nothing is spawned before a job");
        let control = JobControl::new();
        let (work, open) = gated(&control);
        let first = jobs
            .submit(job(JobKind::Activate, Admission::Bounded), control, work)
            .unwrap();
        assert_eq!(first.status, JobStatus::Running);
        assert_eq!(jobs.lanes_started(), 1, "only the module lane started");
        let mut waiting = Vec::new();
        // Every gate stays open for writing until the end, so a waiting job blocks when it runs.
        let mut gates = Vec::new();
        for _ in 0..LANE_QUEUE {
            let control = JobControl::new();
            let (work, gate) = gated(&control);
            gates.push(gate);
            let record = jobs
                .submit(job(JobKind::Activate, Admission::Bounded), control, work)
                .unwrap();
            assert_eq!(record.status, JobStatus::Queued);
            waiting.push(record.job_id);
        }
        let control = JobControl::new();
        let (work, _refused) = gated(&control);
        let error = jobs
            .submit(job(JobKind::Activate, Admission::Bounded), control, work)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(error.detail, "the module lane is full");
        // The transfer lane is separate and still empty.
        let control = JobControl::new();
        let (work, open_transfer) = gated(&control);
        let transfer = jobs
            .submit(job(JobKind::Install, Admission::Bounded), control, work)
            .unwrap();
        assert_eq!(transfer.status, JobStatus::Running);
        // A deactivation is admitted beyond the bound.
        let control = JobControl::new();
        let deactivate = jobs
            .submit(
                job(JobKind::Deactivate, Admission::Always),
                control,
                Box::new(|| Ok(json!({}))),
            )
            .unwrap();
        assert_eq!(deactivate.status, JobStatus::Queued);
        // Cancelling a waiting job removes it; the lane has room again.
        let Some(Cancelled::Removed(record)) = jobs.cancel(&waiting[0], "cancelled by test") else {
            panic!("a waiting job is removed");
        };
        assert_eq!(record.status, JobStatus::Cancelled);
        assert_eq!(record.error.unwrap().message, "cancelled by test");
        open.send(()).unwrap();
        let (id, result) = receive(&completions);
        assert_eq!(id, first.job_id);
        let finished = jobs.complete(&id, result).unwrap();
        assert_eq!(finished.record.status, JobStatus::Ready);
        assert_eq!(finished.record.result, Some(json!({"done": true})));
        assert_eq!(
            jobs.read(&waiting[1]).unwrap().status,
            JobStatus::Running,
            "the next waiting job started"
        );
        // Superseding works only while a job waits.
        assert!(jobs.supersede(&waiting[1]).is_none());
        assert_eq!(
            jobs.supersede(&waiting[2]).unwrap().status,
            JobStatus::Superseded
        );
        open_transfer.send(()).unwrap();
        let (id, result) = receive(&completions);
        assert_eq!(id, transfer.job_id);
        assert!(jobs.complete(&id, result).is_some());
        jobs.shutdown();
        let (id, result) = receive(&completions);
        assert_eq!(id, waiting[1]);
        assert_eq!(result.unwrap_err().detail, "the editor is closing");
    }

    #[test]
    fn a_running_cancel_stops_at_the_next_checkpoint_with_its_reason() {
        let (mut jobs, completions) = jobs();
        let control = JobControl::new();
        let (work, _gate) = gated(&control);
        let running = jobs
            .submit(
                job(JobKind::Install, Admission::Bounded),
                control.clone(),
                work,
            )
            .unwrap();
        control.set_progress(Some(1.5), "halfway");
        let read = jobs.read(&running.job_id).unwrap();
        assert_eq!(read.progress.fraction, Some(1.0), "a fraction is clamped");
        assert_eq!(read.progress.message.as_deref(), Some("halfway"));
        assert_eq!(jobs.depending_on("grant-a"), vec![running.job_id.clone()]);
        let Some(Cancelled::Requested(_)) = jobs.cancel(&running.job_id, PERMISSION_REVOKED) else {
            panic!("a running job is asked to stop");
        };
        let (id, result) = receive(&completions);
        let finished = jobs.complete(&id, result).unwrap();
        assert_eq!(finished.record.status, JobStatus::Cancelled);
        let error = finished.record.error.unwrap();
        assert_eq!(error.code, "cancelled");
        assert_eq!(error.message, PERMISSION_REVOKED);
        assert!(jobs.depending_on("grant-a").is_empty());
        let Some(Cancelled::Finished(record)) = jobs.cancel(&running.job_id, "again") else {
            panic!("a finished job is left alone");
        };
        assert_eq!(record.status, JobStatus::Cancelled);
        jobs.shutdown();
    }

    /// A capability job publishes to the activity board the moment its lane picks it up, reports
    /// progress through it and ends it on arrival — the same board `activity.list` and
    /// `source.prepare`/`analysis.request` publish to, and the only place its progress lives.
    #[test]
    fn a_capability_jobs_progress_and_activity_are_on_the_board() {
        let (mut jobs, completions) = jobs();
        let control = JobControl::new();
        let (work, open) = gated(&control);
        let running = jobs
            .submit(
                job(JobKind::Install, Admission::Bounded),
                control.clone(),
                work,
            )
            .unwrap();
        let snapshot = jobs.board().snapshot();
        assert_eq!(
            snapshot.active.len(),
            1,
            "the job is on the board as soon as it runs"
        );
        let entry = &snapshot.active[0].entry;
        assert_eq!(entry.kind, "module.resource.install");
        assert_eq!(entry.job_id.as_deref(), Some(running.job_id.as_str()));
        assert_eq!(entry.detail.as_deref(), Some("test.module"));
        assert!(entry.progress.is_none(), "nothing reported yet");

        control.set_progress(Some(0.5), "halfway");
        let midway = jobs.board().snapshot();
        assert_eq!(
            midway.active[0].entry.progress,
            Some(JobProgress {
                fraction: Some(0.5),
                message: Some("halfway".into())
            }),
            "the board carries the same progress `module.job.read` answers with"
        );
        assert_eq!(
            jobs.read(&running.job_id).unwrap().progress,
            midway.active[0].entry.progress.clone().unwrap(),
            "module.job.read reads the board's own progress, not a separate copy"
        );

        open.send(()).unwrap();
        let (id, result) = receive(&completions);
        let finished = jobs.complete(&id, result).unwrap();
        assert_eq!(finished.record.status, JobStatus::Ready);
        let after = jobs.board().snapshot();
        assert!(after.active.is_empty(), "the activity ended with the job");
        assert_eq!(after.recent[0].outcome, Outcome::Completed);
        assert_eq!(
            after.recent[0].entry.job_id.as_deref(),
            Some(running.job_id.as_str())
        );
        jobs.shutdown();
    }

    #[test]
    fn a_panicking_job_fails_and_the_lane_keeps_running_and_records_are_bounded() {
        let (mut jobs, completions) = jobs();
        let panicking = jobs
            .submit(
                job(JobKind::Activate, Admission::Bounded),
                JobControl::new(),
                Box::new(|| panic!("module bug")),
            )
            .unwrap();
        let (id, result) = receive(&completions);
        assert_eq!(id, panicking.job_id);
        let finished = jobs.complete(&id, result).unwrap();
        assert_eq!(finished.record.status, JobStatus::Failed);
        assert_eq!(finished.record.error.unwrap().code, "internal");
        for index in 0..FINISHED_RECORDS + 3 {
            jobs.submit(
                job(JobKind::Activate, Admission::Bounded),
                JobControl::new(),
                Box::new(move || Ok(json!({"index": index}))),
            )
            .unwrap();
            let (id, result) = receive(&completions);
            jobs.complete(&id, result).unwrap();
        }
        assert!(jobs.read(&panicking.job_id).is_none(), "the oldest is gone");
        assert_eq!(jobs.of_module("test.module").len(), FINISHED_RECORDS);
        assert_eq!(
            jobs.of_module("test.module").last().unwrap().result,
            Some(json!({"index": FINISHED_RECORDS + 2}))
        );
        jobs.shutdown();
    }
}
