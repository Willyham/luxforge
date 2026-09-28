//! The one job table. Every job the catalog owner runs is one record here — source preparation,
//! analysis, capability work and export — with one identity, one status vocabulary, one progress
//! model, one bounded retention of finished records and one read shape, which `job.read` answers
//! and `job.cancel` acts on. See `docs/design/modules-and-api.md#jobs`.
//!
//! Each kind keeps its own admission and scheduling:
//!
//! - **Source** preparation (`prepare`, `develop`, `artifacts`, `collect`) runs on the owner's
//!   source worker. A request joins the queued or running job for the same flight key, and the job
//!   belongs to the clients interested in it: a cancel releases the caller's interest, and only the
//!   last release stops the work.
//! - **Analysis** runs on its latest-wins worker, one running and one replaceable pending job. A
//!   request joins the job for the same identity, including a finished report, and cancels release
//!   interest as for source work. A newer request supersedes the pending one.
//! - **Capability** jobs run on this table's `transfer` and `module` lanes and **export** on its
//!   `export` lane. Each lane is one thread, spawned on its first job, that blocks on its channel
//!   while idle, runs one job at a time and posts the result into the catalog owner's own channel;
//!   nothing polls. The table keeps each lane's waiting jobs, at most [`LANE_QUEUE`] of them, so a
//!   queued job can be cancelled or superseded without touching the thread. A lane job belongs to
//!   no client: any client may read or cancel it, a cancel stops it for everyone, and a client's
//!   disconnect never touches it.
//!
//! Progress travels from a worker to the owner through a [`JobControl`] the worker writes and the
//! owner reads when a client asks.
use crate::{
    AssetId, Cancel, ClientId, EditorState, Error, ErrorKind, JobId,
    activity::{Activity, ActivityBoard, ActivitySpec, Outcome},
    analysis::{AnalysisIdentity, Report},
    api::{Origin, SourceFlightKey},
    capabilities::{
        host::{ACTIVATE, DEACTIVATE},
        resources::{INSTALL, REMOVE},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    io,
    net::{Shutdown, TcpStream},
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
/// Finished capability, export and analysis records the table keeps of each of those families; the
/// oldest of a family is forgotten first.
pub const FINISHED_RECORDS: usize = 32;
/// Finished source records the table keeps.
pub const FINISHED_SOURCE_RECORDS: usize = 64;
/// Ready analysis reports the table keeps; the oldest is forgotten first. A report is about 6 KiB.
pub const MAX_READY_REPORTS: usize = 8;

/// The job methods, which serve every kind.
pub const JOB_READ: &str = "job.read";
pub const JOB_CANCEL: &str = "job.cancel";

/// The reason a job is cancelled when a grant it depends on is revoked.
pub const PERMISSION_REVOKED: &str = "permission revoked";
/// The reason `job.cancel` gives, and the reason a shared job ends when its last client leaves it.
pub const CANCELLED: &str = "the job was cancelled";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lane {
    Transfer,
    Module,
    Export,
}

impl Lane {
    pub fn name(self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
            Self::Module => "module",
            Self::Export => "export",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Transfer => 0,
            Self::Module => 1,
            Self::Export => 2,
        }
    }
}

/// What a job does, which says how it is scheduled and cancelled ([`Family`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobKind {
    /// Read, verify and decode an original, developing a RAW, for an import or a reopen.
    Prepare,
    /// Develop an already decoded RAW at other gains.
    Develop,
    /// Read and verify an asset's artifacts.
    Artifacts,
    /// Remove the files an artifact collection left unrecorded.
    Collect,
    /// The exact histogram and clipping counts of one evaluated stack.
    Analysis,
    Activate,
    Deactivate,
    Install,
    Remove,
    Task,
    /// A JPEG export.
    Export,
}

/// How a kind is scheduled and cancelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    /// The source worker; shared by interest.
    Source,
    /// The analysis worker; shared by interest.
    Analysis,
    /// The capability lanes; a cancel stops the job for everyone.
    Capability,
    /// The export lane; a cancel stops the job for everyone.
    Export,
}

impl Family {
    /// How many finished records of this family the table keeps.
    fn retained(self) -> usize {
        match self {
            Self::Source => FINISHED_SOURCE_RECORDS,
            Self::Analysis | Self::Capability | Self::Export => FINISHED_RECORDS,
        }
    }
}

impl JobKind {
    pub(crate) fn family(self) -> Family {
        match self {
            Self::Prepare | Self::Develop | Self::Artifacts | Self::Collect => Family::Source,
            Self::Analysis => Family::Analysis,
            Self::Activate | Self::Deactivate | Self::Install | Self::Remove | Self::Task => {
                Family::Capability
            }
            Self::Export => Family::Export,
        }
    }

    /// The lane that runs a job of this kind, for the kinds this table schedules itself.
    pub fn lane(self) -> Option<Lane> {
        match self {
            Self::Install | Self::Remove => Some(Lane::Transfer),
            Self::Activate | Self::Deactivate | Self::Task => Some(Lane::Module),
            Self::Export => Some(Lane::Export),
            Self::Prepare | Self::Develop | Self::Artifacts | Self::Collect | Self::Analysis => {
                None
            }
        }
    }
}

/// The shared job lifecycle (`crate::JobStatus`): every kind reaches `queued`, `running`, `ready`,
/// `failed` and `cancelled`; `superseded` is a pending analysis a newer request replaced, or a
/// queued activation a deactivation replaced, before either ran.
pub use crate::JobStatus;

/// How far a job has come. The one progress model every activity publisher shares
/// (`crate::activity::ActivityProgress`): a lane job reports through the activity its lane begins
/// rather than keeping its own copy, and `job.read` answers with what the board carries.
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

/// One job as `job.read` answers it, whatever its kind. The subject fields say what the job is
/// about: `asset_id` for work on one photo, `module_id` and `resource_id` for capability work, and
/// `identity` for an analysis. `result` is the kind's result once it is `ready`: a prepared
/// asset's state, a collection's counts, an analysis report, a capability job's value or an
/// export's written file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: JobId,
    pub kind: JobKind,
    pub status: JobStatus,
    pub progress: JobProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_id: Option<String>,
    /// The resource an install or removal is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    /// Which evaluated image an analysis describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<AnalysisIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobError>,
    /// The request that started the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

impl JobRecord {
    /// The module a capability job belongs to; empty for any other kind.
    pub fn module(&self) -> &str {
        self.module_id.as_deref().unwrap_or_default()
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
    /// Cancelled with the job, so a render the job runs stops within one row or chunk without
    /// the job polling for it: the render's passes read this token themselves.
    render: Cancel,
    /// A clone of the socket of the network request the job is making, if any: its cancel shuts
    /// the socket down, so a read or write blocked on it returns at once.
    connection: Mutex<Option<TcpStream>>,
}

impl JobControl {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// The cancel flag itself, for work that checks a plain flag between its steps, as a source
    /// preparation's decode, development and artifact reads do.
    pub(crate) fn flag(&self) -> &AtomicBool {
        &self.cancelled
    }

    /// Ask the job to stop at its next checkpoint. The first reason given is the one reported.
    pub(crate) fn cancel(&self, reason: &str) {
        let mut held = self.reason.lock().expect("job cancel reason");
        if held.is_none() {
            *held = Some(reason.to_owned());
        }
        self.cancelled.store(true, Ordering::Release);
        self.render.cancel();
        if let Some(socket) = self.connection.lock().expect("job connection").as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    /// Keep a clone of `socket`, the job's network request's connection, for `cancel` to shut
    /// down; `false`, keeping nothing, if the job is already cancelled. The check and the
    /// registration share the lock `cancel` takes after setting its flag, so a cancel cannot fall
    /// between them.
    pub(crate) fn hold_connection(&self, socket: &TcpStream) -> io::Result<bool> {
        let mut held = self.connection.lock().expect("job connection");
        if self.is_cancelled() {
            return Ok(false);
        }
        *held = Some(socket.try_clone()?);
        Ok(true)
    }

    /// Drop the connection's clone once its request is over.
    pub(crate) fn release_connection(&self) {
        *self.connection.lock().expect("job connection") = None;
    }

    /// The render cancellation token this job's cancel also sets, for a job that renders.
    pub fn render_cancel(&self) -> &Cancel {
        &self.render
    }

    /// The `cancelled` error naming why the job was stopped.
    pub fn cancelled_error(&self) -> Error {
        let reason = self
            .reason
            .lock()
            .expect("job cancel reason")
            .clone()
            .unwrap_or_else(|| CANCELLED.to_owned());
        Error::cancelled(reason)
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

    /// Report the phase the job has reached, on its activity. Called from the job's own worker
    /// thread.
    pub fn set_phase(&self, phase: &'static str) {
        if let Some(activity) = self.activity.lock().expect("job activity").as_ref() {
            activity.phase(phase);
        }
    }

    /// The progress this job currently reports on the board, for `job.read` to answer with while it
    /// runs. A job with no activity yet reports nothing.
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
    fn finish_activity(&self, outcome: Outcome) {
        if let Some(activity) = self.activity.lock().expect("job activity").take() {
            activity.finish(outcome);
        }
    }
}

/// What a lane runs: everything the job needs, moved to the worker, returning the job's result.
pub(crate) type Work = Box<dyn FnOnce() -> Result<Value, Error> + Send>;
/// Posts one finished lane job back into the owner's channel. Called on the lane thread.
pub(crate) type Deliver = Arc<dyn Fn(JobId, Result<Value, Error>) + Send + Sync>;

/// What a finished job leaves for its clients to read. A prepared asset and a report stay typed,
/// so `job.adopt` takes the asset without a round trip through JSON and the table holds a report in
/// its own compact form; each is encoded only when a client reads it.
pub(crate) enum Output {
    Value(Value),
    Asset(Box<EditorState>),
    Report(Box<Report>),
}

impl Output {
    fn value(&self) -> Value {
        match self {
            Self::Value(value) => value.clone(),
            Self::Asset(state) => json!(state),
            Self::Report(report) => json!(report),
        }
    }
}

/// What makes two requests the same work, so the second joins the first: a source preparation's
/// flight, or an analysis identity. A source flight is joinable while its job is live; an analysis
/// identity also while its report or failure is kept, which is how a repeated request is a cache
/// hit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum JoinKey {
    Source(SourceFlightKey),
    Analysis(Box<AnalysisIdentity>),
}

impl JoinKey {
    /// Whether a job that ended in `status` still answers a request for its key.
    fn kept_after(&self, status: JobStatus) -> bool {
        match self {
            Self::Source(_) => false,
            Self::Analysis(_) => matches!(status, JobStatus::Ready | JobStatus::Failed),
        }
    }
}

/// The clients a shared job belongs to.
#[derive(Debug, Default)]
struct Interest {
    /// Every client that has requested the job. It decides who may read or cancel it, so a client
    /// that cancelled still reads the outcome.
    requesters: BTreeSet<ClientId>,
    /// The clients that still want the work. When the last leaves a live job, the work stops.
    interested: BTreeSet<ClientId>,
}

/// A source preparation or an analysis the owner is about to schedule on its own worker. Its
/// identity is chosen by the caller, so the worker's task can carry it before it is recorded.
pub(crate) struct Opened {
    pub job_id: JobId,
    pub kind: JobKind,
    /// The requesting client; `None` for a report the desktop's own preview worker produced.
    pub client: Option<ClientId>,
    pub key: Option<JoinKey>,
    pub asset_id: Option<AssetId>,
    pub control: Arc<JobControl>,
}

/// What a client's leaving a shared job means for its work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Release {
    /// Other clients still want it, or it had already finished: leave the work alone.
    Kept,
    /// Nobody is interested any more and the job is live: its control is cancelled and its key no
    /// longer joins, so the owner stops its work on the worker that holds it.
    Stopped,
}

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

/// A lane job the owner is about to queue. Its identity is chosen by the caller, so work that
/// names its own job, such as an install's staging directory, can be built before it is queued.
pub(crate) struct NewJob {
    pub job_id: JobId,
    pub kind: JobKind,
    pub module_id: Option<String>,
    pub resource_id: Option<String>,
    pub asset_id: Option<AssetId>,
    pub origin: Option<Origin>,
    /// Grants the job runs under; revoking one cancels it.
    pub grants: Vec<String>,
    pub admission: Admission,
    /// What the job publishes once its lane picks it up; its `job_id` is filled in then. `None`
    /// derives it from the capability job's kind, module and resource.
    pub activity: Option<ActivitySpec>,
}

struct Entry {
    /// The record's fixed fields and status; `progress`, `result` and `error` are filled in when a
    /// client reads it.
    record: JobRecord,
    output: Option<Output>,
    error: Option<Error>,
    control: Arc<JobControl>,
    /// Present for a shared job, which belongs to the clients interested in it.
    interest: Option<Interest>,
    key: Option<JoinKey>,
    /// Present while a lane job waits; taken when it is dispatched.
    work: Option<Work>,
    grants: Vec<String>,
    origin: Option<Origin>,
    activity: Option<ActivitySpec>,
}

impl Entry {
    fn new(record: JobRecord, control: Arc<JobControl>) -> Self {
        Self {
            record,
            output: None,
            error: None,
            control,
            interest: None,
            key: None,
            work: None,
            grants: Vec::new(),
            origin: None,
            activity: None,
        }
    }

    fn read(&self) -> JobRecord {
        let mut record = self.record.clone();
        if !record.status.is_finished() {
            record.progress = self.control.progress();
        }
        record.result = self.output.as_ref().map(Output::value);
        record.error = self.error.as_ref().map(JobError::from);
        record.request_id = self.origin.as_ref().map(|origin| origin.request_id.clone());
        record
    }

    fn family(&self) -> Family {
        self.record.kind.family()
    }

    fn is_live(&self) -> bool {
        !self.record.status.is_finished()
    }

    /// Whether `client` may read and cancel this job: any client for a lane job, and a client that
    /// requested it for a shared one.
    fn readable_by(&self, client: ClientId) -> bool {
        self.interest
            .as_ref()
            .is_none_or(|interest| interest.requesters.contains(&client))
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

/// A job that just finished, for the owner to act on.
pub(crate) struct Finished {
    pub record: JobRecord,
    pub origin: Option<Origin>,
}

/// What a lane job's cancel did.
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
        JobKind::Activate => (ACTIVATE, "Activating module"),
        JobKind::Deactivate => (DEACTIVATE, "Deactivating module"),
        JobKind::Install => (INSTALL, "Installing resource"),
        JobKind::Remove => (REMOVE, "Removing resource"),
        JobKind::Task => ("module.task", "Running task"),
        _ => ("export", "Exporting JPEG"),
    };
    let detail = match &record.resource_id {
        Some(resource_id) => format!("{}/{resource_id}", record.module()),
        None => record.module().to_owned(),
    };
    ActivitySpec {
        kind,
        label,
        detail: Some(detail),
        asset_id: None,
        job_id: Some(record.job_id.to_string()),
    }
}

fn unknown(job_id: &JobId) -> Error {
    Error::validation(format!("unknown job {job_id}"))
}

/// The one job table and its lanes. The catalog owner holds the only instance.
pub(crate) struct Jobs {
    entries: HashMap<JobId, Entry>,
    /// Finished jobs in the order they finished, every family together; [`Family::retained`] bounds
    /// each family's share.
    finished: VecDeque<JobId>,
    /// The live, or still answering, job for each join key.
    keys: HashMap<JoinKey, JobId>,
    lanes: [LaneState; 3],
    deliver: Deliver,
    /// Where every lane job publishes from the moment its lane picks it up, so `activity.list`
    /// shows capability work and export beside source preparation and analysis.
    board: Arc<ActivityBoard>,
}

impl Jobs {
    pub(crate) fn new(deliver: Deliver, board: Arc<ActivityBoard>) -> Self {
        Self {
            entries: HashMap::new(),
            finished: VecDeque::new(),
            keys: HashMap::new(),
            lanes: [
                LaneState::new(Lane::Transfer),
                LaneState::new(Lane::Module),
                LaneState::new(Lane::Export),
            ],
            deliver,
            board,
        }
    }

    /// The board this table publishes every running lane job to, for a test that wants to read what
    /// a job reported without going through `read`'s own `JobRecord` copy.
    #[cfg(test)]
    pub(crate) fn board(&self) -> &Arc<ActivityBoard> {
        &self.board
    }

    // Reading.

    /// One job as the owner reads it, with its live progress, whoever asked for it.
    pub(crate) fn read(&self, job_id: &JobId) -> Option<JobRecord> {
        self.entries.get(job_id).map(Entry::read)
    }

    /// One job as `client` reads it: any lane job, and a shared job this client requested. Any
    /// other job, or one the table no longer keeps, is `validation`.
    pub(crate) fn read_for(&self, job_id: &JobId, client: ClientId) -> Result<JobRecord, Error> {
        self.entries
            .get(job_id)
            .filter(|entry| entry.readable_by(client))
            .map(Entry::read)
            .ok_or_else(|| unknown(job_id))
    }

    /// The kind of a job `client` may read.
    pub(crate) fn kind_for(&self, job_id: &JobId, client: ClientId) -> Result<JobKind, Error> {
        self.entries
            .get(job_id)
            .filter(|entry| entry.readable_by(client))
            .map(|entry| entry.record.kind)
            .ok_or_else(|| unknown(job_id))
    }

    /// The kind of a job, whoever asked for it.
    pub(crate) fn kind(&self, job_id: &JobId) -> Option<JobKind> {
        self.entries.get(job_id).map(|entry| entry.record.kind)
    }

    /// Whether `client` still wants this live shared job.
    pub(crate) fn wanted_by(&self, job_id: &JobId, client: ClientId) -> bool {
        self.entries.get(job_id).is_some_and(|entry| {
            entry.is_live()
                && entry
                    .interest
                    .as_ref()
                    .is_some_and(|interest| interest.interested.contains(&client))
        })
    }

    /// Whether any client still wants this shared job.
    pub(crate) fn wanted(&self, job_id: &JobId) -> bool {
        self.entries.get(job_id).is_some_and(|entry| {
            entry
                .interest
                .as_ref()
                .is_some_and(|interest| !interest.interested.is_empty())
        })
    }

    /// Whether any job of this family is queued or running.
    pub(crate) fn any_live(&self, family: Family) -> bool {
        self.entries
            .values()
            .any(|entry| entry.family() == family && entry.is_live())
    }

    /// Whether a joinable source flight will allocate new planes: a live preparation of an
    /// original or a redevelopment nobody has left. A flight that only reads artifacts allocates
    /// none.
    pub(crate) fn development_in_flight(&self) -> bool {
        self.keys.iter().any(|(key, job_id)| {
            matches!(key, JoinKey::Source(_))
                && self.entries.get(job_id).is_some_and(|entry| {
                    matches!(entry.record.kind, JobKind::Prepare | JobKind::Develop)
                })
        })
    }

    /// The request that started the job, when one did.
    pub(crate) fn origin(&self, job_id: &JobId) -> Option<&Origin> {
        self.entries.get(job_id)?.origin.as_ref()
    }

    /// Name the request that started a job that has none yet, as an import does for the
    /// preparation it joined or opened.
    pub(crate) fn set_origin(&mut self, job_id: &JobId, origin: Origin) {
        if let Some(entry) = self.entries.get_mut(job_id)
            && entry.origin.is_none()
        {
            entry.origin = Some(origin);
        }
    }

    /// A finished shared job's outcome as `client` reads it: its status, what it left and its
    /// error, for `job.adopt` to take the prepared asset from.
    pub(crate) fn outcome_for(
        &self,
        job_id: &JobId,
        client: ClientId,
    ) -> Result<(JobStatus, Option<&Output>, Option<&Error>), Error> {
        let entry = self
            .entries
            .get(job_id)
            .filter(|entry| entry.readable_by(client))
            .ok_or_else(|| unknown(job_id))?;
        Ok((
            entry.record.status,
            entry.output.as_ref(),
            entry.error.as_ref(),
        ))
    }

    /// The job a key names, live or still answering.
    pub(crate) fn keyed(&self, key: &JoinKey) -> Option<&JobId> {
        self.keys.get(key)
    }

    // Shared jobs: source preparation and analysis.

    /// Join the job for this key: `client` becomes one of its requesters, and one of the clients
    /// that want it while it is live. A finished job needs no worker, so joining it never revives
    /// interest in work.
    pub(crate) fn join(&mut self, key: &JoinKey, client: ClientId) -> Option<JobId> {
        let job_id = self.keys.get(key)?.clone();
        let entry = self.entries.get_mut(&job_id)?;
        let live = entry.is_live();
        let interest = entry.interest.get_or_insert_with(Interest::default);
        interest.requesters.insert(client);
        if live {
            interest.interested.insert(client);
        }
        Some(job_id)
    }

    /// Record a shared job the owner has just handed to its own worker: `queued` until the worker
    /// starts it.
    pub(crate) fn open(&mut self, opened: Opened) {
        self.insert(opened);
    }

    /// Record a shared job that is already done, such as an import the verified cache answers or a
    /// report the desktop's preview worker produced.
    pub(crate) fn open_finished(&mut self, opened: Opened, output: Output) {
        let job_id = opened.job_id.clone();
        self.insert(opened);
        self.finish(&job_id, Ok(output));
    }

    fn insert(&mut self, opened: Opened) {
        let Opened {
            job_id,
            kind,
            client,
            key,
            asset_id,
            control,
        } = opened;
        let identity = match &key {
            Some(JoinKey::Analysis(identity)) => Some((**identity).clone()),
            _ => None,
        };
        let mut entry = Entry::new(
            JobRecord {
                job_id: job_id.clone(),
                kind,
                status: JobStatus::Queued,
                progress: JobProgress::default(),
                asset_id,
                module_id: None,
                resource_id: None,
                identity,
                result: None,
                error: None,
                request_id: None,
            },
            control,
        );
        entry.interest = Some(Interest {
            requesters: client.into_iter().collect(),
            interested: client.into_iter().collect(),
        });
        if let Some(key) = key {
            self.keys.insert(key.clone(), job_id.clone());
            entry.key = Some(key);
        }
        self.entries.insert(job_id, entry);
    }

    /// The worker has started a queued shared job.
    pub(crate) fn start(&mut self, job_id: &JobId) {
        if let Some(entry) = self.entries.get_mut(job_id)
            && entry.record.status == JobStatus::Queued
        {
            entry.record.status = JobStatus::Running;
        }
    }

    /// `client` leaves a shared job: it no longer wants the work but still reads the outcome. The
    /// last client leaving a live job cancels its control and stops its key from joining; the owner
    /// then stops the work where its worker holds it. `validation` when this client never
    /// requested the job, which is how a foreign or unknown job is refused.
    pub(crate) fn release(&mut self, job_id: &JobId, client: ClientId) -> Result<Release, Error> {
        let entry = self
            .entries
            .get_mut(job_id)
            .filter(|entry| entry.readable_by(client))
            .ok_or_else(|| unknown(job_id))?;
        let live = entry.is_live();
        let Some(interest) = entry.interest.as_mut() else {
            return Err(unknown(job_id));
        };
        let held = interest.interested.remove(&client);
        if held && live && interest.interested.is_empty() {
            self.stop(job_id);
            return Ok(Release::Stopped);
        }
        Ok(Release::Kept)
    }

    /// A client is gone: it leaves every shared job it wanted, exactly as a cancel does, and owns
    /// nothing any more. Returns the live jobs that lost their last interested client, whose work
    /// the owner then stops. Lane jobs are untouched: a disconnect never cancels them.
    pub(crate) fn disconnect(&mut self, client: ClientId) -> Vec<(JobId, JobKind)> {
        let mut orphaned = Vec::new();
        for (job_id, entry) in &mut self.entries {
            let live = entry.is_live();
            let Some(interest) = entry.interest.as_mut() else {
                continue;
            };
            interest.requesters.remove(&client);
            if interest.interested.remove(&client) && live && interest.interested.is_empty() {
                orphaned.push((job_id.clone(), entry.record.kind));
            }
        }
        orphaned.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (job_id, _) in &orphaned {
            self.stop(job_id);
        }
        orphaned
    }

    /// Nobody wants this live shared job: cancel its control and stop its key from joining.
    fn stop(&mut self, job_id: &JobId) {
        let Some(entry) = self.entries.get_mut(job_id) else {
            return;
        };
        entry.control.cancel(CANCELLED);
        if let Some(key) = &entry.key
            && self.keys.get(key) == Some(job_id)
        {
            self.keys.remove(key);
        }
    }

    // Finishing.

    /// Record a job's outcome and retire it. A shared job its last client left ends `cancelled`
    /// whatever its worker produced, and the result is discarded; otherwise success is `ready`, a
    /// `cancelled` error is `cancelled` with the reason the owner gave, and any other error is
    /// `failed`. A job that is not live is left as it is and `None` returned.
    pub(crate) fn finish(
        &mut self,
        job_id: &JobId,
        result: Result<Output, Error>,
    ) -> Option<Finished> {
        let entry = self.entries.get_mut(job_id)?;
        if !entry.is_live() {
            return None;
        }
        let cancel_requested = entry.control.is_cancelled();
        let abandoned = entry
            .interest
            .as_ref()
            .is_some_and(|interest| interest.interested.is_empty() && cancel_requested);
        // Captured from the still-live activity before it ends, so the record keeps the last
        // progress the board carried rather than an empty one.
        entry.record.progress = entry.control.progress();
        let result = if abandoned {
            Err(entry.control.cancelled_error())
        } else {
            result
        };
        match result {
            Ok(output) => {
                entry.record.status = JobStatus::Ready;
                entry.output = Some(output);
            }
            Err(error) if error.kind == ErrorKind::Cancelled => {
                entry.record.status = JobStatus::Cancelled;
                // A transport or module reports its own words for a stop; the reason the owner
                // gave is what a client needs to read.
                entry.error = Some(if cancel_requested {
                    entry.control.cancelled_error()
                } else {
                    error
                });
            }
            Err(error) => {
                entry.record.status = JobStatus::Failed;
                entry.error = Some(error);
            }
        }
        entry.control.finish_activity(match entry.record.status {
            JobStatus::Ready => Outcome::Completed,
            JobStatus::Failed => Outcome::Failed,
            _ => Outcome::Cancelled,
        });
        if let Some(interest) = entry.interest.as_mut() {
            interest.interested.clear();
        }
        let finished = Finished {
            record: entry.read(),
            origin: entry.origin.clone(),
        };
        self.retire(job_id);
        Some(finished)
    }

    /// A queued job a newer request replaced before it ran: a pending analysis another request
    /// displaced from the one slot, or a waiting activation a deactivation made pointless. A
    /// running or finished job is left as it is and `None` is returned.
    pub(crate) fn supersede(&mut self, job_id: &JobId) -> Option<JobRecord> {
        let entry = self.entries.get_mut(job_id)?;
        if entry.record.status != JobStatus::Queued {
            return None;
        }
        entry.work = None;
        entry.record.status = JobStatus::Superseded;
        if let Some(interest) = entry.interest.as_mut() {
            interest.interested.clear();
        }
        let record = entry.read();
        if let Some(lane) = entry.record.kind.lane() {
            self.lanes[lane.index()].waiting.retain(|id| id != job_id);
        }
        self.retire(job_id);
        Some(record)
    }

    /// Move a finished job to the finished records, stop its key answering unless its kind keeps
    /// it, and forget the oldest records past each bound: a family's own share, and for analysis
    /// the ready reports. A live job is never forgotten, because a worker still refers to it.
    fn retire(&mut self, job_id: &JobId) {
        let Some(entry) = self.entries.get(job_id) else {
            return;
        };
        let family = entry.family();
        if let Some(key) = &entry.key
            && !key.kept_after(entry.record.status)
            && self.keys.get(key) == Some(job_id)
        {
            self.keys.remove(key);
        }
        self.finished.push_back(job_id.clone());
        if family == Family::Analysis {
            let report = |entry: &Entry| matches!(entry.output, Some(Output::Report(_)));
            while self.count(|entry| entry.family() == Family::Analysis && report(entry))
                > MAX_READY_REPORTS
            {
                self.forget_oldest(|entry| entry.family() == Family::Analysis && report(entry));
            }
        }
        while self.count(|entry| entry.family() == family) > family.retained() {
            self.forget_oldest(|entry| entry.family() == family);
        }
    }

    /// How many finished records match.
    fn count(&self, matching: impl Fn(&Entry) -> bool) -> usize {
        self.finished
            .iter()
            .filter(|id| self.entries.get(*id).is_some_and(&matching))
            .count()
    }

    /// Forget the oldest finished record that matches.
    fn forget_oldest(&mut self, matching: impl Fn(&Entry) -> bool) {
        let Some(position) = self
            .finished
            .iter()
            .position(|id| self.entries.get(id).is_some_and(&matching))
        else {
            return;
        };
        let Some(job_id) = self.finished.remove(position) else {
            return;
        };
        if let Some(entry) = self.entries.remove(&job_id)
            && let Some(key) = &entry.key
            && self.keys.get(key) == Some(&job_id)
        {
            self.keys.remove(key);
        }
    }

    // Lane jobs: capability work and export.

    /// Queue a lane job, or refuse it with `resource-limit` when its lane is full. The job starts
    /// at once when its lane is idle.
    pub(crate) fn submit(
        &mut self,
        job: NewJob,
        control: Arc<JobControl>,
        work: Work,
    ) -> Result<JobRecord, Error> {
        let lane = job
            .kind
            .lane()
            .ok_or_else(|| Error::internal("a lane job needs a lane kind"))?;
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
            return Err(Error::resource_limit(format!(
                "the {} lane is full",
                lane.name()
            )));
        }
        let job_id = job.job_id;
        let mut entry = Entry::new(
            JobRecord {
                job_id: job_id.clone(),
                kind: job.kind,
                status: JobStatus::Queued,
                progress: JobProgress::default(),
                asset_id: job.asset_id,
                module_id: job.module_id,
                resource_id: job.resource_id,
                identity: None,
                result: None,
                error: None,
                request_id: None,
            },
            control,
        );
        entry.work = Some(work);
        entry.grants = job.grants;
        entry.origin = job.origin;
        entry.activity = job.activity;
        self.entries.insert(job_id.clone(), entry);
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
                    Error::resource_limit(format!("cannot start the {} lane: {error}", lane.name()))
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
        let spec = match entry.activity.take() {
            Some(spec) => ActivitySpec {
                job_id: Some(entry.record.job_id.to_string()),
                ..spec
            },
            None => capability_activity(&entry.record),
        };
        entry.control.begin_activity(self.board.begin(spec));
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
            self.finish(&job_id, Err(Error::internal("the lane stopped")));
        }
        Ok(())
    }

    /// Cancel a lane job for everyone: a waiting one is removed at once, a running one is asked to
    /// stop at its next checkpoint, and a finished one is left as it is.
    pub(crate) fn cancel(&mut self, job_id: &JobId, reason: &str) -> Option<Cancelled> {
        let entry = self.entries.get_mut(job_id)?;
        match entry.record.status {
            JobStatus::Queued => {
                entry.work = None;
                entry.control.cancel(reason);
                let lane = entry.record.kind.lane();
                let error = entry.control.cancelled_error();
                let record = self.finish(job_id, Err(error))?.record;
                if let Some(lane) = lane {
                    self.lanes[lane.index()].waiting.retain(|id| id != job_id);
                }
                Some(Cancelled::Removed(record))
            }
            JobStatus::Running => {
                entry.control.cancel(reason);
                Some(Cancelled::Requested(entry.read()))
            }
            _ => Some(Cancelled::Finished(entry.read())),
        }
    }

    /// A lane reported its running job: record the outcome and start the lane's next job. A result
    /// for a job that is not running is ignored.
    pub(crate) fn complete(
        &mut self,
        job_id: &JobId,
        result: Result<Value, Error>,
    ) -> Option<Finished> {
        let lane = self.entries.get(job_id)?.record.kind.lane()?;
        if self.lanes[lane.index()].running.as_ref() != Some(job_id) {
            return None;
        }
        let finished = self.finish(job_id, result.map(Output::Value))?;
        self.lanes[lane.index()].running = None;
        if let Err(error) = self.dispatch(lane) {
            // The lane thread exists already, so only a stopped thread gets here; every waiting job
            // fails rather than waiting forever.
            let waiting: Vec<JobId> = self.lanes[lane.index()].waiting.drain(..).collect();
            for job_id in waiting {
                self.finish(&job_id, Err(error.clone()));
            }
        }
        Some(finished)
    }

    /// Live and retained records of one module, oldest first.
    pub(crate) fn of_module(&self, module_id: &str) -> Vec<JobRecord> {
        let mut records: Vec<JobRecord> = self
            .entries
            .values()
            .filter(|entry| entry.record.module_id.as_deref() == Some(module_id))
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
                    && entry.record.module_id.as_deref() == Some(module_id)
                    && entry.record.resource_id.as_deref() == resource_id
                    && entry.is_live()
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
                    && entry.record.module_id.as_deref() == Some(module_id)
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
                entry.is_live() && entry.grants.iter().any(|grant| grant == grant_id)
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

    /// Stop everything: ask every live job to stop, including those on the owner's own workers,
    /// drop the lanes' waiting jobs, close the lanes and wait for their threads, which finish at
    /// their job's next checkpoint.
    pub(crate) fn shutdown(&mut self) {
        for entry in self.entries.values() {
            if entry.is_live() {
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
            panic::catch_unwind(AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err(Error::internal("the job stopped unexpectedly")))
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
            module_id: Some("test.module".into()),
            resource_id: None,
            asset_id: None,
            origin: Some(Origin::new("module.activate", "request")),
            grants: vec!["grant-a".into()],
            admission,
            activity: None,
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
            "the board carries the same progress `job.read` answers with"
        );
        assert_eq!(
            jobs.read(&running.job_id).unwrap().progress,
            midway.active[0].entry.progress.clone().unwrap(),
            "job.read reads the board's own progress, not a separate copy"
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

    /// An export job runs on its own lane, publishes the activity it was given with its phase,
    /// and its cancel reaches the render token the job renders under.
    #[test]
    fn an_export_job_publishes_its_own_activity_and_cancels_its_render() {
        let (mut jobs, completions) = jobs();
        let control = JobControl::new();
        let (work, _gate) = gated(&control);
        let asset = crate::AssetId::new();
        let running = jobs
            .submit(
                NewJob {
                    job_id: JobId::new(),
                    kind: JobKind::Export,
                    module_id: None,
                    resource_id: None,
                    asset_id: Some(asset.clone()),
                    origin: Some(Origin::new("export.jpeg", "request")),
                    grants: Vec::new(),
                    admission: Admission::Bounded,
                    activity: Some(ActivitySpec {
                        kind: "export",
                        label: "Exporting JPEG",
                        detail: Some("photo-edited.jpg".into()),
                        asset_id: Some(asset.clone()),
                        job_id: None,
                    }),
                },
                control.clone(),
                work,
            )
            .unwrap();
        assert_eq!(running.status, JobStatus::Running);
        assert_eq!(jobs.lanes_started(), 1, "only the export lane started");
        control.set_phase("encoding");
        let snapshot = jobs.board().snapshot();
        let entry = &snapshot.active[0].entry;
        assert_eq!(
            (entry.kind.as_ref(), entry.label.as_ref()),
            ("export", "Exporting JPEG")
        );
        assert_eq!(entry.detail.as_deref(), Some("photo-edited.jpg"));
        assert_eq!(entry.asset_id.as_ref(), Some(&asset));
        assert_eq!(entry.job_id.as_deref(), Some(running.job_id.as_str()));
        assert_eq!(entry.phase.as_deref(), Some("encoding"));
        assert!(!control.render_cancel().is_cancelled());
        jobs.cancel(&running.job_id, "stop");
        assert!(control.render_cancel().is_cancelled());
        let (id, result) = receive(&completions);
        let finished = jobs.complete(&id, result).unwrap();
        assert_eq!(finished.record.status, JobStatus::Cancelled);
        assert_eq!(finished.record.kind, JobKind::Export);
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

    /// An analysis identity that differs from others by its output width.
    fn identity(width: u32) -> AnalysisIdentity {
        AnalysisIdentity {
            asset_id: AssetId::new(),
            source_fingerprint: "sha256:test".into(),
            entry_id: crate::EntryId::new(),
            snapshot_id: crate::SnapshotId::new(),
            recipe_hash: "0".repeat(64),
            draft: None,
            width,
            height: 1,
            domain: crate::analysis::AnalysisDomain,
        }
    }

    fn report() -> Output {
        Output::Report(Box::new(
            crate::analysis::reduce(&[0, 0, 0, 255], 1, 1).unwrap(),
        ))
    }

    /// Open a shared analysis job for `client`, as the owner does before handing it to its worker.
    fn open(jobs: &mut Jobs, key: &JoinKey, client: ClientId) -> JobId {
        let job_id = JobId::new();
        jobs.open(Opened {
            job_id: job_id.clone(),
            kind: JobKind::Analysis,
            client: Some(client),
            key: Some(key.clone()),
            asset_id: None,
            control: JobControl::new(),
        });
        job_id
    }

    /// Two requests for one key share one job, a release leaves the caller's interest only, and
    /// the last release stops the work: its key no longer joins, and whatever its worker reports
    /// afterwards, the job ends `cancelled` with no result. Every requester still reads that
    /// outcome; nobody else can, and the key gets fresh work.
    #[test]
    fn a_shared_job_is_joined_by_key_and_only_its_last_release_stops_it() {
        let (mut jobs, _) = jobs();
        let key = JoinKey::Analysis(Box::new(identity(4)));
        let one = ClientId::testing(1);
        let two = ClientId::testing(2);
        let job = open(&mut jobs, &key, one);
        assert_eq!(
            jobs.join(&key, two),
            Some(job.clone()),
            "the same key joins"
        );
        assert!(jobs.wanted_by(&job, two));
        assert_eq!(jobs.release(&job, one).unwrap(), Release::Kept);
        assert!(jobs.wanted(&job), "the other client still wants it");
        assert_eq!(
            jobs.release(&job, one).unwrap(),
            Release::Kept,
            "a repeat changes nothing"
        );
        assert_eq!(jobs.release(&job, two).unwrap(), Release::Stopped);
        assert_eq!(
            jobs.read(&job).unwrap().status,
            JobStatus::Queued,
            "live until its worker reports"
        );
        assert_eq!(jobs.join(&key, one), None, "a stopped job is not joined");
        let finished = jobs.finish(&job, Ok(report())).unwrap();
        assert_eq!(finished.record.status, JobStatus::Cancelled);
        assert!(
            finished.record.result.is_none(),
            "an abandoned result is discarded"
        );
        assert_eq!(finished.record.error.unwrap().message, CANCELLED);
        for client in [one, two] {
            assert_eq!(
                jobs.read_for(&job, client).unwrap().status,
                JobStatus::Cancelled
            );
        }
        assert_eq!(
            jobs.read_for(&job, ClientId::testing(3)).unwrap_err().kind,
            ErrorKind::Validation
        );
        assert_eq!(
            jobs.release(&job, ClientId::testing(3)).unwrap_err().kind,
            ErrorKind::Validation
        );
        assert_ne!(open(&mut jobs, &key, one), job, "the key gets fresh work");
    }

    /// A ready analysis keeps answering its key, so a later request is a cache hit that joins it
    /// without reviving any interest in work; a superseded one stops answering at once, and its
    /// worker's late outcome is ignored.
    #[test]
    fn a_ready_report_answers_its_key_and_a_superseded_job_does_not() {
        let (mut jobs, _) = jobs();
        let one = ClientId::testing(1);
        let ready = JoinKey::Analysis(Box::new(identity(1)));
        let job = open(&mut jobs, &ready, one);
        jobs.start(&job);
        assert_eq!(jobs.read(&job).unwrap().status, JobStatus::Running);
        jobs.finish(&job, Ok(report())).unwrap();
        let record = jobs.read(&job).unwrap();
        assert_eq!(record.status, JobStatus::Ready);
        assert!(
            record.result.unwrap()["r"].is_array(),
            "the report is the result"
        );
        let two = ClientId::testing(2);
        assert_eq!(jobs.join(&ready, two), Some(job.clone()));
        assert!(!jobs.wanted(&job), "joining a finished job wants no work");
        assert_eq!(jobs.read_for(&job, two).unwrap().status, JobStatus::Ready);

        let replaced = JoinKey::Analysis(Box::new(identity(2)));
        let pending = open(&mut jobs, &replaced, one);
        assert_eq!(
            jobs.supersede(&pending).unwrap().status,
            JobStatus::Superseded
        );
        assert_eq!(
            jobs.join(&replaced, one),
            None,
            "superseded work is requested afresh"
        );
        assert!(
            jobs.finish(&pending, Ok(report())).is_none(),
            "a late outcome is ignored"
        );
    }

    /// A gone client leaves every shared job it wanted and owns nothing any more; the jobs that
    /// lost their last client are the ones returned for the owner to stop. A lane job belongs to
    /// no client, so a disconnect never touches it.
    #[test]
    fn a_disconnect_leaves_every_shared_job_and_never_touches_a_lane_job() {
        let (mut jobs, completions) = jobs();
        let one = ClientId::testing(1);
        let two = ClientId::testing(2);
        let alone = open(&mut jobs, &JoinKey::Analysis(Box::new(identity(1))), one);
        let shared_key = JoinKey::Analysis(Box::new(identity(2)));
        let shared = open(&mut jobs, &shared_key, one);
        jobs.join(&shared_key, two);
        let control = JobControl::new();
        let (work, open_lane) = gated(&control);
        let export = jobs
            .submit(
                NewJob {
                    module_id: None,
                    ..job(JobKind::Export, Admission::Bounded)
                },
                control.clone(),
                work,
            )
            .unwrap();
        assert_eq!(
            jobs.disconnect(one),
            vec![(alone.clone(), JobKind::Analysis)],
            "only the job nobody else wants is stopped"
        );
        assert_eq!(
            jobs.read_for(&alone, one).unwrap_err().kind,
            ErrorKind::Validation,
            "a gone client owns nothing"
        );
        assert!(jobs.wanted_by(&shared, two));
        assert!(!control.is_cancelled(), "the export runs on");
        assert_eq!(
            jobs.read_for(&export.job_id, one).unwrap().status,
            JobStatus::Running,
            "any client reads a lane job"
        );
        open_lane.send(()).unwrap();
        let (id, result) = receive(&completions);
        assert_eq!(
            jobs.complete(&id, result).unwrap().record.status,
            JobStatus::Ready
        );
        jobs.shutdown();
    }

    /// Each family keeps its own share of finished records, and analysis at most
    /// [`MAX_READY_REPORTS`] reports, the oldest forgotten first. A live job is never forgotten.
    #[test]
    fn each_familys_finished_records_and_the_reports_stay_bounded() {
        let (mut jobs, completions) = jobs();
        let client = ClientId::testing(1);
        let live = open(&mut jobs, &JoinKey::Analysis(Box::new(identity(0))), client);
        let mut reports = Vec::new();
        for width in 1..=MAX_READY_REPORTS as u32 + 4 {
            let key = JoinKey::Analysis(Box::new(identity(width)));
            let job = open(&mut jobs, &key, client);
            jobs.finish(&job, Ok(report())).unwrap();
            reports.push((job, key));
        }
        for (job, key) in &reports[..4] {
            assert!(jobs.read(job).is_none(), "the oldest reports are forgotten");
            assert_eq!(jobs.join(key, client), None, "and stop answering their key");
        }
        assert_eq!(jobs.read(&reports[4].0).unwrap().status, JobStatus::Ready);
        for width in 100..100 + 2 * FINISHED_RECORDS as u32 {
            let job = open(
                &mut jobs,
                &JoinKey::Analysis(Box::new(identity(width))),
                client,
            );
            jobs.supersede(&job);
        }
        let analyses = jobs
            .entries
            .values()
            .filter(|entry| entry.family() == Family::Analysis && !entry.is_live())
            .count();
        assert_eq!(analyses, FINISHED_RECORDS);
        assert!(jobs.read(&live).is_some(), "a live job is never forgotten");
        // Lane jobs are families of their own, untouched by the analyses above.
        jobs.submit(
            job(JobKind::Activate, Admission::Bounded),
            JobControl::new(),
            Box::new(|| Ok(json!({}))),
        )
        .unwrap();
        let (id, result) = receive(&completions);
        jobs.complete(&id, result).unwrap();
        assert_eq!(jobs.of_module("test.module").len(), 1);
        jobs.shutdown();
    }
}
