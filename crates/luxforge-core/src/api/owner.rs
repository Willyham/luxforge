//! One thread owns the catalog and every client session; all clients call it in turn. The owner
//! loop finds each request's method in the one method table, calls its handler and records the
//! events the call announced; the owner handlers the table names live here.
use super::{
    ApiEvent, ApiRequest, ApiResponse, ClientAuthority, ClientSession, EventsResult, Origin,
    announce_once,
    methods::{self, Changed, Planned, Retries, Route},
    params::{NoParams, host_params},
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    AnalysisPlan, AnalysisSelection, AssetId, DraftId, EditorService, EditorState, EntryId, Error,
    HostConfig, JobId, JobStatus, ModuleRegistry, Preparation, PreparationNeeds, PreviewJob,
    ProxyBounds,
    activity::{ActivityBoard, ActivitySpec, Outcome},
    analysis::{AnalysisIdentity, AnalysisJob, AnalysisQueue, Report},
    artifacts::{self, ArtifactId, ArtifactRead, Collected, Collection},
    capabilities::host::CapabilityHost,
    editor::{Prepared, Preparing, SourceSignature, SourceWork},
    jobs::{CANCELLED, Family, JobControl, JobKind, Jobs, JoinKey, Opened, Output, Release},
    source::PlaneGate,
};
use point::{POINT_QUEUE_CAPACITY, PointWorker};
use requests::{RequestKey, RequestTable};
use serde::Deserialize;
use serde_json::{Value, json};
#[cfg(test)]
use std::thread;
use std::{
    collections::{HashMap, VecDeque},
    ops::ControlFlow,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[cfg(test)]
mod artifact_tests;
mod catalog;
#[cfg(test)]
mod catalog_tests;
pub(super) mod export;
#[cfg(test)]
mod export_tests;
// The catalog lanes' owner-side homes, one file each ([`catalog`]).
pub(super) mod files;
pub(super) mod library;
mod point;
pub(super) mod previews;
mod requests;
pub(super) mod views;

const EVENT_CAPACITY: usize = 256;
/// The longest an `events.wait` may be asked to wait, and what it waits when it names no time.
const MAX_EVENT_WAIT_MS: i64 = 30_000;
const DEFAULT_EVENT_WAIT_MS: u64 = 10_000;
const SOURCE_QUEUE_CAPACITY: usize = 8;
/// Pending developments may pin one sensor mosaic identity; the cache can retain one more.
const MAX_QUEUED_MOSAICS: usize = 1;

/// Identifies one connected client; the owner keeps that client's session until it disconnects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(u64);

#[cfg(test)]
impl ClientId {
    /// A client identity for tests that drive the store without an owner loop.
    pub(crate) fn testing(id: u64) -> Self {
        Self(id)
    }
}

/// No registered client: [`OwnerHandle::register`] hands out `1` and up, so this id never
/// collides with one. Attached to the collection [`launch`] queues on its own, which no client
/// asked for and none can read through `job.read`.
const SYSTEM_CLIENT: ClientId = ClientId(0);

struct OwnerCall {
    client: ClientId,
    request: ApiRequest,
    response: SyncSender<ApiResponse>,
}

enum OwnerMessage {
    Call(OwnerCall),
    Preview {
        request: PreviewRequest,
        response: SyncSender<Result<PreviewJob, Error>>,
    },
    /// The analysis worker has an outcome for the owner to take. Nothing polls for this: the
    /// worker posts it into the owner's own channel, so the owner stays asleep until there is
    /// something to do.
    AnalysisReady,
    /// A report the desktop's preview worker already produced for this identity, so an API request
    /// for the same identity is a cache hit and no second render happens.
    AnalysisSubmitted {
        identity: Box<AnalysisIdentity>,
        report: Box<Report>,
    },
    SourceStarted(JobId),
    /// Boxed: a prepared source with its verified artifacts is several times larger than any
    /// other message, and every message on the owner's channel would otherwise carry that size.
    SourceComplete(JobId, Box<Result<SourceResult, Error>>),
    /// A client registered with more than edit authority. Sent by `register_with` before it
    /// returns, so the channel orders it before any call the client makes.
    Register {
        client: ClientId,
        authority: ClientAuthority,
    },
    /// A lane of the job table finished a capability or export job. Like the analysis worker, the
    /// lane posts it into this channel, so nothing polls.
    JobFinished {
        job_id: JobId,
        result: Result<Value, Error>,
    },
    /// How many lane threads have started, for tests that prove discovery is inert.
    #[cfg(test)]
    LaneThreads(SyncSender<usize>),
    /// Hold every export job accepted from now on as it begins each phase, or stop holding them.
    #[cfg(test)]
    HoldExports(Option<export::Hold>),
    /// Hold the point worker before each evaluation, or release that hold.
    #[cfg(test)]
    HoldPoints(Option<point::Hold>),
    /// How many planned samples wait behind the one the point worker is evaluating.
    #[cfg(test)]
    PointsWaiting(SyncSender<usize>),
    /// Call this where the owner serves a message, or stop calling it.
    #[cfg(test)]
    Fault(Option<Fault>),
    /// How many `events.wait` calls the owner holds unanswered.
    #[cfg(test)]
    EventWaits(SyncSender<usize>),
    /// Relocate an asset as the Locate command will, which no method exposes yet ([`relocate`]).
    #[cfg(test)]
    Relocate {
        origin: Origin,
        asset_id: AssetId,
        path: PathBuf,
        reply: SyncSender<Result<crate::MutationOutcome, Error>>,
    },
    /// Wake this client whenever another client's change lands in the event log.
    WatchEvents {
        client: ClientId,
        wake: EventWake,
    },
    /// Answer once the named source job of this client is no longer queued or running, or, with no
    /// job named, once any source job finishes; at once when there is nothing to wait for.
    AwaitSource {
        client: ClientId,
        job: Option<JobId>,
        reply: SyncSender<()>,
    },
    /// A catalog lane's worker has something for the owner, which hands it to that lane.
    Catalog(catalog::CatalogMessage),
    Disconnect(ClientId),
    Stop,
}

/// What the owner calls, on its own thread, when a change another client made reaches the event
/// log. It must only post a signal: the owner waits for it.
pub type EventWake = Arc<dyn Fn() + Send + Sync>;

/// What a test has the owner call, on its own thread, with what it is about to serve: a request's
/// method just before its handler runs, or `source.complete` just before a finished source job's
/// result is committed. A test that panics in it proves the owner contains the panic.
#[cfg(test)]
type Fault = Arc<dyn Fn(&str) + Send + Sync>;

/// One client blocked in [`OwnerHandle::wait_source`], answered by the completion it waits for.
struct SourceWaiter {
    client: ClientId,
    job: Option<JobId>,
    reply: SyncSender<()>,
}

/// What an `events.wait` that found nothing to report yet asks the owner to hold.
struct PendingWait {
    after: u64,
    asset_id: Option<AssetId>,
    deadline: Instant,
}

/// One `events.wait` call parked on the owner: the reply its caller is blocked on, answered when an
/// event past `after` (naming `asset_id`, when one is given) reaches the log or `deadline` passes.
struct EventWait {
    client: ClientId,
    id: String,
    wait: PendingWait,
    /// The newest event sequence this wait has already been checked against, so a wake rescans the
    /// log only when it has moved.
    seen: u64,
    response: SyncSender<ApiResponse>,
}

/// Whom one message owes an answer, kept aside before the owner serves it, so that a panic while
/// serving it still answers: a call or a preview is answered `internal`, a wait is released, and a
/// source job whose result was being committed fails `internal`.
enum Owed {
    Call(String, SyncSender<ApiResponse>),
    Preview(SyncSender<Result<PreviewJob, Error>>),
    Wait(SyncSender<()>),
    Source(JobId),
    Nobody,
}

impl Owed {
    fn of(message: &OwnerMessage) -> Self {
        match message {
            OwnerMessage::Call(call) => Self::Call(call.request.id.clone(), call.response.clone()),
            OwnerMessage::Preview { response, .. } => Self::Preview(response.clone()),
            OwnerMessage::AwaitSource { reply, .. } => Self::Wait(reply.clone()),
            OwnerMessage::SourceComplete(id, _) => Self::Source(id.clone()),
            _ => Self::Nobody,
        }
    }
}

/// What one preview job should render. The client identity travels with it because a draft belongs
/// to that client's session, which only the owner holds: a client can never ask for another's.
#[derive(Clone, Debug)]
pub struct PreviewRequest {
    pub client: ClientId,
    pub asset_id: AssetId,
    /// The entry to show; `None` is the current one.
    pub entry_id: Option<EntryId>,
    /// `Some(n)` renders the first `n` layers of the resulting stack only.
    pub layer_count: Option<usize>,
    /// Render this client's open draft instead of the stored stack.
    pub draft: Option<DraftId>,
    /// Also reduce the rendered frame into a histogram report, which the worker returns beside the
    /// raster. Refused together with `layer_count`: a truncated job renders a layer prefix its
    /// identity does not describe.
    pub analyse: bool,
    /// The physical pixels the photo area can show this frame in, when the caller wants the job to
    /// have a proxy phase. `None` asks for the exact path alone. The owner only copies it into the
    /// job; the preview queue decides whether a proxy is worthwhile and builds it on its worker.
    pub proxy: Option<ProxyBounds>,
}

impl PreviewRequest {
    /// The current entry of one asset, whole.
    pub fn new(client: ClientId, asset_id: AssetId) -> Self {
        Self {
            client,
            asset_id,
            entry_id: None,
            layer_count: None,
            draft: None,
            analyse: false,
            proxy: None,
        }
    }
    /// Show this entry instead of the current one.
    pub fn entry(mut self, entry_id: Option<EntryId>) -> Self {
        self.entry_id = entry_id;
        self
    }
    /// Render the first `count` layers only: the input stage of the layer at that index.
    pub fn layers(mut self, count: usize) -> Self {
        self.layer_count = Some(count);
        self
    }
    /// Render the effective recipe of this client's draft.
    pub fn draft(mut self, draft: DraftId) -> Self {
        self.draft = Some(draft);
        self
    }
    /// Reduce the rendered frame into a histogram report as well, so the displayed target needs no
    /// second render.
    pub fn analyse(mut self) -> Self {
        self.analyse = true;
        self
    }
    /// Offer this job a proxy phase at the display bounds the frame will be shown in.
    pub fn proxy(mut self, bounds: ProxyBounds) -> Self {
        self.proxy = Some(bounds);
        self
    }
}

/// What makes two source jobs the same work, so a second request joins the first. `signature` is
/// the original's file signature, absent for a job that reads artifacts only; `artifacts` are the
/// identities the job reads and verifies after any source work, sorted.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SourceFlightKey {
    path: PathBuf,
    signature: Option<SourceSignature>,
    expected_fingerprint: Option<String>,
    gains_bits: Option<[u32; 3]>,
    artifacts: Vec<ArtifactId>,
}

enum SourceTaskKind {
    /// Prepare an original, a RAW development or artifacts: the service's own source work, which
    /// the service completes ([`EditorService::complete_preparation`]).
    Prepare(SourceWork),
    /// Remove the object files the owner's collection left unrecorded, and stale staged files.
    Collect(Collection),
}

impl SourceTaskKind {
    /// The job kind this task is recorded as, and the asset it works on when one is known.
    fn job(&self) -> (JobKind, Option<AssetId>) {
        match self {
            Self::Prepare(SourceWork::File { .. }) => (JobKind::Prepare, None),
            Self::Prepare(SourceWork::Develop(request)) => {
                (JobKind::Develop, Some(request.asset_id.clone()))
            }
            Self::Prepare(SourceWork::Artifacts(asset_id)) => {
                (JobKind::Artifacts, Some(asset_id.clone()))
            }
            Self::Collect(_) => (JobKind::Collect, None),
        }
    }
}

enum SourceResult {
    Prepared(Box<Prepared>),
    Collected(Collected),
}

struct SourceTask {
    id: JobId,
    /// The job's control in the one job table: its flag is what the worker checks between steps.
    control: Arc<JobControl>,
    kind: SourceTaskKind,
    /// Read and verified after the source work of a preparation, so one job readies a whole stack.
    artifacts: Vec<ArtifactRead>,
}

/// What a completed source job leaves for its clients to read.
enum Completed {
    /// A prepared asset, and whether this job created it.
    Asset(Box<EditorState>, bool),
    Value(Value),
}

/// The source worker's admission: a bounded channel to the worker, and the sensor mosaic each
/// queued development pins until it completes, so developments pin at most
/// [`MAX_QUEUED_MOSAICS`] distinct mosaics between them. The jobs themselves, who wants them and
/// the flight they join by are records in the one job table.
struct SourceQueue {
    sender: SyncSender<SourceTask>,
    sensors: HashMap<JobId, Weak<luxforge_raw::RawSource>>,
    /// The worker's memory gate, woken whenever a job is stopped so a worker waiting on it for
    /// that job sees the stop.
    gate: Arc<PlaneGate>,
}

/// The identities a job reads, sorted, for its flight key.
fn flight_artifacts(reads: &[ArtifactRead]) -> Vec<ArtifactId> {
    let mut ids: Vec<ArtifactId> = reads.iter().map(|read| read.id.clone()).collect();
    ids.sort();
    ids
}

impl SourceQueue {
    fn new(sender: SyncSender<SourceTask>, gate: Arc<PlaneGate>) -> Self {
        Self {
            sender,
            sensors: HashMap::new(),
            gate,
        }
    }

    /// Queue one source job's work and the artifacts it reads after it, or join the queued or
    /// running job doing the same. A development is admitted only while queued developments pin
    /// fewer than [`MAX_QUEUED_MOSAICS`] distinct mosaics, or the one it pins already.
    fn enqueue(
        &mut self,
        jobs: &mut Jobs,
        client: ClientId,
        work: SourceWork,
        artifacts: Vec<ArtifactRead>,
    ) -> Result<JobId, Error> {
        let (key, sensor) = match &work {
            SourceWork::File {
                path,
                signature,
                target,
            } => (
                SourceFlightKey {
                    path: path.clone(),
                    signature: Some(signature.clone()),
                    expected_fingerprint: target.as_ref().map(|target| target.fingerprint.clone()),
                    gains_bits: target
                        .as_ref()
                        .and_then(|target| target.raw.as_ref())
                        .map(|raw| raw.gains.map(f32::to_bits)),
                    artifacts: flight_artifacts(&artifacts),
                },
                None,
            ),
            SourceWork::Develop(request) => {
                let key = SourceFlightKey {
                    path: request.asset_id.as_str().into(),
                    signature: Some(request.signature.clone()),
                    expected_fingerprint: Some(request.fingerprint.clone()),
                    gains_bits: Some(request.gains.map(f32::to_bits)),
                    artifacts: flight_artifacts(&artifacts),
                };
                if let Some(id) = jobs.join(&JoinKey::Source(key.clone()), client) {
                    return Ok(id);
                }
                let sensor = Arc::downgrade(&request.sensor);
                self.admit_mosaic(&sensor)?;
                (key, Some(sensor))
            }
            SourceWork::Artifacts(asset_id) => (
                SourceFlightKey {
                    path: asset_id.as_str().into(),
                    signature: None,
                    expected_fingerprint: None,
                    gains_bits: None,
                    artifacts: flight_artifacts(&artifacts),
                },
                None,
            ),
        };
        let kind = SourceTaskKind::Prepare(work);
        self.submit(jobs, client, key, kind, artifacts, sensor, true)
    }

    /// Refuse a development of a mosaic no queued development pins yet once they pin
    /// [`MAX_QUEUED_MOSAICS`] distinct mosaics between them.
    fn admit_mosaic(&self, sensor: &Weak<luxforge_raw::RawSource>) -> Result<(), Error> {
        let mut distinct = Vec::<&Weak<luxforge_raw::RawSource>>::new();
        for existing in self.sensors.values() {
            if existing.strong_count() > 0
                && !distinct.iter().any(|seen| Weak::ptr_eq(seen, existing))
            {
                distinct.push(existing);
            }
        }
        if distinct.len() >= MAX_QUEUED_MOSAICS
            && !distinct
                .iter()
                .any(|existing| Weak::ptr_eq(existing, sensor))
        {
            return Err(Error::source_queue_full(
                "RAW mosaic queue is full; retry after the active development",
            ));
        }
        Ok(())
    }

    /// A collection: either a client asked for it explicitly, or [`OwnerHandle::launch`] queued it
    /// on its own when the catalog opened. Never shared: another request never joins it.
    fn enqueue_maintenance(
        &mut self,
        jobs: &mut Jobs,
        client: ClientId,
        path: PathBuf,
        kind: SourceTaskKind,
    ) -> Result<JobId, Error> {
        let key = SourceFlightKey {
            path,
            signature: None,
            expected_fingerprint: None,
            gains_bits: None,
            artifacts: Vec::new(),
        };
        self.submit(jobs, client, key, kind, Vec::new(), None, false)
    }

    /// Queue one task on the source worker and record its job, or join the queued or running job
    /// for the same key when `shared`. A full queue is a `resource-limit` and changes nothing.
    #[allow(clippy::too_many_arguments)]
    fn submit(
        &mut self,
        jobs: &mut Jobs,
        client: ClientId,
        key: SourceFlightKey,
        kind: SourceTaskKind,
        artifacts: Vec<ArtifactRead>,
        sensor: Option<Weak<luxforge_raw::RawSource>>,
        shared: bool,
    ) -> Result<JobId, Error> {
        let joined = JoinKey::Source(key.clone());
        if shared && let Some(id) = jobs.join(&joined, client) {
            return Ok(id);
        }
        let id = JobId::new();
        let control = JobControl::new();
        let (job_kind, asset_id) = kind.job();
        let task = SourceTask {
            id: id.clone(),
            control: control.clone(),
            kind,
            artifacts,
        };
        match self.sender.try_send(task) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(Error::source_queue_full("source preparation queue is full"));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(Error::protocol("source preparation worker stopped"));
            }
        }
        jobs.open(Opened {
            job_id: id.clone(),
            kind: job_kind,
            client: Some(client),
            key: shared.then_some(joined),
            asset_id,
            control,
        });
        if let Some(sensor) = sensor {
            self.sensors.insert(id.clone(), sensor);
        }
        Ok(id)
    }

    /// A job that is ready at once: an import or preparation the verified cache already answers.
    fn ready(
        &mut self,
        jobs: &mut Jobs,
        client: ClientId,
        state: EditorState,
    ) -> Result<JobId, Error> {
        // The original must still be there with a readable signature, as for queued work.
        EditorService::request_signature(&state.asset.locator)?;
        let id = JobId::new();
        jobs.open_finished(
            Opened {
                job_id: id.clone(),
                kind: JobKind::Prepare,
                client: Some(client),
                key: None,
                asset_id: Some(state.asset.id.clone()),
                control: JobControl::new(),
            },
            Output::Asset(Box::new(state)),
        );
        Ok(id)
    }

    /// The worker reported a job, so a development it held pins nothing any more.
    fn complete(&mut self, id: &JobId) {
        self.sensors.remove(id);
    }
}

/// Queue the source job that prepares exactly what `needs` names ([`EditorService::preparation`]),
/// or answer with a job that is already ready when nothing is left to prepare.
fn queue_preparation(
    service: &EditorService,
    sources: &mut SourceQueue,
    jobs: &mut Jobs,
    client: ClientId,
    needs: &PreparationNeeds,
) -> Result<JobId, Error> {
    match service.preparation(needs)? {
        Preparing::Ready(state) => sources.ready(jobs, client, *state),
        Preparing::Work(work, reads) => queue_work(service, sources, jobs, client, work, reads),
    }
}

/// Queue one source job's work, and release the cache's float planes once work that allocates
/// planes of its own is queued, retaining the most recently used development for a redevelopment
/// of the same mosaic.
fn queue_work(
    service: &EditorService,
    sources: &mut SourceQueue,
    jobs: &mut Jobs,
    client: ClientId,
    work: SourceWork,
    reads: Vec<ArtifactRead>,
) -> Result<JobId, Error> {
    let (decodes, redevelops) = (work.decodes(), work.redevelops());
    let id = sources.enqueue(jobs, client, work, reads)?;
    if decodes {
        service.evict_development(redevelops);
    }
    Ok(id)
}

/// Where a test holds the source worker: after a task's activity has begun and before any of its
/// work, so the test can read the task as running for as long as it needs to, or panic there as the
/// task's work could. Outside tests it is empty and holds nothing.
#[derive(Clone, Default)]
struct SourceHold(#[cfg(test)] Option<Arc<dyn Fn() + Send + Sync>>);

impl SourceHold {
    fn wait(&self) {
        #[cfg(test)]
        if let Some(hold) = &self.0 {
            hold();
        }
    }
}

/// The activity one source task publishes, with the original's file name as its detail line.
fn source_activity(task: &SourceTask) -> ActivitySpec {
    match &task.kind {
        SourceTaskKind::Prepare(SourceWork::File { path, .. }) => ActivitySpec {
            kind: "source.prepare",
            label: "Preparing original",
            detail: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            asset_id: None,
            job_id: Some(task.id.to_string()),
        },
        SourceTaskKind::Prepare(SourceWork::Develop(request)) => ActivitySpec {
            kind: "source.develop",
            label: "Developing RAW",
            detail: request.file_name.clone(),
            asset_id: Some(request.asset_id.clone()),
            job_id: Some(task.id.to_string()),
        },
        SourceTaskKind::Prepare(SourceWork::Artifacts(asset_id)) => ActivitySpec {
            kind: "artifacts.read",
            label: "Verifying artifacts",
            detail: None,
            asset_id: Some(asset_id.clone()),
            job_id: Some(task.id.to_string()),
        },
        SourceTaskKind::Collect(_) => ActivitySpec {
            kind: "artifacts.collect",
            label: "Removing unused artifacts",
            detail: None,
            asset_id: None,
            job_id: Some(task.id.to_string()),
        },
    }
}

/// One source task's work on the worker: the service's own preparation ([`SourceWork::run`]), or a
/// collection's removal of files. A developed RAW's linear planes hold a lease on the worker's
/// memory gate until they are released.
fn run_source_task(
    kind: SourceTaskKind,
    reads: &[ArtifactRead],
    cancel: &AtomicBool,
    gate: &Arc<PlaneGate>,
) -> Result<SourceResult, Error> {
    match kind {
        SourceTaskKind::Prepare(work) => {
            let mut prepared = work.run(reads, cancel)?;
            if let Some(linear) = prepared.linear_mut() {
                linear.hold(gate.lease());
            }
            Ok(SourceResult::Prepared(Box::new(prepared)))
        }
        SourceTaskKind::Collect(collection) => {
            artifacts::collect_files(&collection, cancel).map(SourceResult::Collected)
        }
    }
}

fn source_worker(
    receiver: Receiver<SourceTask>,
    owner: SyncSender<OwnerMessage>,
    gate: Arc<PlaneGate>,
    board: Arc<ActivityBoard>,
    hold: SourceHold,
) {
    while let Ok(task) = receiver.recv() {
        if task.control.is_cancelled() {
            let _ = owner.send(OwnerMessage::SourceComplete(
                task.id,
                Box::new(Err(Error::conflict("source job cancelled"))),
            ));
            continue;
        }
        // A previous RAW result/cache or active/pending preview may still pin its large float
        // planes. Wait on the worker, never the catalog owner, before another source allocation:
        // the last release or a stop wakes it. Artifact work allocates no planes and never waits.
        if matches!(&task.kind, SourceTaskKind::Prepare(work) if work.decodes()) {
            gate.wait_released(task.control.flag());
        }
        if task.control.is_cancelled() {
            let _ = owner.send(OwnerMessage::SourceComplete(
                task.id,
                Box::new(Err(Error::conflict("source job cancelled"))),
            ));
            continue;
        }
        // The activity begins before the owner marks the job as preparing, so a client that reads
        // it as preparing always finds it listed. Leaving the loop drops the guard as cancelled.
        let activity = board.begin(source_activity(&task));
        if owner
            .send(OwnerMessage::SourceStarted(task.id.clone()))
            .is_err()
        {
            break;
        }
        // A task that panics fails `internal` like any other failed task, so its job, the clients
        // waiting on it and the board agree, and the worker lives on for the next task.
        let result = catch_unwind(AssertUnwindSafe(|| {
            hold.wait();
            run_source_task(task.kind, &task.artifacts, task.control.flag(), &gate)
        }))
        .unwrap_or_else(|_| Err(Error::internal("the source job stopped unexpectedly")));
        // The activity ends before the owner learns the result, so a client that reads the job as
        // ready or failed never still finds it listed as running. A stopped preparation fails with
        // a conflict rather than `Cancelled`, or finishes before it sees the stop; either way the
        // table records it cancelled, so the job's own flag says which it was.
        activity.finish(if task.control.is_cancelled() {
            Outcome::Cancelled
        } else {
            Outcome::of(&result)
        });
        if owner
            .send(OwnerMessage::SourceComplete(task.id, Box::new(result)))
            .is_err()
        {
            break;
        }
    }
}

host_params! {
    /// `catalog.import`.
    pub(super) struct Import {
        path: PathBuf = path().notes("the photo to import"),
        mutation: MutationRequest,
    }
}

host_params! {
    /// `job.read`, `job.cancel` and `job.adopt`.
    pub(super) struct JobParams {
        job_id: JobId = job(),
    }
}

host_params! {
    /// `source.prepare`.
    pub(super) struct SourcePrepare {
        asset_id: AssetId = asset(),
        entry_id: Option<EntryId> = entry().notes("historical entry; default current"),
    }
}

host_params! {
    /// `events.since`.
    pub(super) struct EventsSince {
        after: u64 = sequence().notes("the last event sequence the client has read; 0 for every retained event"),
    }
}

host_params! {
    /// `events.wait`.
    pub(super) struct EventsWait {
        after: u64 = sequence().notes("the last event sequence the client has read; 0 for every retained event"),
        timeout_ms: Option<u64> = integer(0, MAX_EVENT_WAIT_MS).notes("how long to wait for a change, in milliseconds; default 10000, and 0 answers at once as events.since does"),
        asset_id: Option<AssetId> = asset().notes("wake only for an event that names this asset"),
    }
}

host_params! {
    /// `analysis.request`.
    pub(super) struct AnalysisRequest {
        asset_id: AssetId = asset(),
        target: AnalysisTarget = json("{kind: current}, {kind: entry, entry_id} or {kind: draft, draft_id}: the evaluated stack to analyse"),
    }
}

host_params! {
    /// `artifact.collect`.
    pub(super) struct Collect {
        mutation: MutationRequest,
    }
}

/// Which evaluated stack the caller wants analysed.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum AnalysisTarget {
    /// The asset's current entry.
    Current,
    /// One frozen historical entry, which a later commit never relabels.
    Entry { entry_id: EntryId },
    /// This client's own open draft, at the `draft_revision` it holds now.
    Draft { draft_id: DraftId },
}

#[derive(Clone)]
pub struct OwnerHandle {
    sender: SyncSender<OwnerMessage>,
    next_client: Arc<AtomicU64>,
    activity: Arc<ActivityBoard>,
    render: crate::RenderContext,
}

impl OwnerHandle {
    pub fn start(catalog: &Path) -> Result<(Self, JoinHandle<()>), Error> {
        Self::start_with(catalog, Arc::new(ModuleRegistry::builtin()))
    }

    /// Own a catalog served by a specific set of providers, which is how a client registers a
    /// built-in wrapped as unavailable. Registration happens before any catalog work. The owner has
    /// no settings directory or secure store, so every settings method reports `not-ready`.
    pub fn start_with(
        catalog: &Path,
        registry: Arc<ModuleRegistry>,
    ) -> Result<(Self, JoinHandle<()>), Error> {
        Self::start_with_host(catalog, registry, HostConfig::unconfigured())
    }

    /// Own a catalog with a capability host: where module settings live and which secret store
    /// holds credentials. Starting touches neither; the first settings write creates the directory.
    pub fn start_with_host(
        catalog: &Path,
        registry: Arc<ModuleRegistry>,
        host: HostConfig,
    ) -> Result<(Self, JoinHandle<()>), Error> {
        Self::launch(
            catalog,
            registry,
            host,
            ActivityBoard::new(),
            SourceHold::default(),
        )
    }

    /// [`Self::start_with`] publishing to a board the test supplies, usually one whose recent
    /// threshold is zero so a small fixture's short work is kept, and with the source worker
    /// calling `hold` after each task's activity begins and before its work.
    #[cfg(test)]
    pub(crate) fn start_observed(
        catalog: &Path,
        registry: Arc<ModuleRegistry>,
        activity: Arc<ActivityBoard>,
        hold: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<(Self, JoinHandle<()>), Error> {
        Self::launch(
            catalog,
            registry,
            HostConfig::unconfigured(),
            activity,
            SourceHold(hold),
        )
    }

    fn launch(
        catalog: &Path,
        registry: Arc<ModuleRegistry>,
        host: HostConfig,
        activity: Arc<ActivityBoard>,
        hold: SourceHold,
    ) -> Result<(Self, JoinHandle<()>), Error> {
        let mut service = EditorService::open_with(catalog, registry)?;
        let render = service.render_context().clone();
        let (sender, receiver) = sync_channel(64);
        let (source_sender, source_receiver) = sync_channel(SOURCE_QUEUE_CAPACITY);
        let worker_sender = sender.clone();
        let gate = Arc::new(PlaneGate::default());
        // The job table's lanes post their finished capability and export jobs back through the
        // owner's own channel, like the source and analysis workers.
        let lane_sender = sender.clone();
        let mut jobs = Jobs::new(
            Arc::new(move |job_id, result| {
                let _ = lane_sender.send(OwnerMessage::JobFinished { job_id, result });
            }),
            activity.clone(),
        );
        let mut sources = SourceQueue::new(source_sender, gate.clone());
        // One collection, queued here rather than on the owner thread or in response to any
        // client: the row query and deletion happen now, on the thread opening the catalog, and
        // the source worker removes the files once it starts. Skipped when there is nothing to
        // find: no unreferenced row and no artifact root yet, the common case for a catalog that
        // has never published an artifact, so a fresh catalog queues no source job. A planning
        // error or a full queue (unreachable this early) simply leaves the objects for the next
        // open to collect; it never fails opening the catalog.
        if let Ok(collection) = service.plan_collection()
            && (collection.rows > 0 || collection.root.exists())
        {
            let root = collection.root.clone();
            let _ = sources.enqueue_maintenance(
                &mut jobs,
                SYSTEM_CLIENT,
                root,
                SourceTaskKind::Collect(collection),
            );
        }
        let worker_activity = activity.clone();
        let worker = std::thread::spawn(move || {
            source_worker(source_receiver, worker_sender, gate, worker_activity, hold)
        });
        // The analysis worker posts its results back through this same channel, so the owner needs
        // one clone of its own sender. The loop ends on `Stop`, never on the senders dropping.
        let completions = sender.clone();
        let host = CapabilityHost::new(host);
        let owner_activity = activity.clone();
        // The catalog lanes post their workers' results through the same channel.
        let catalog =
            catalog::CatalogLanes::new(catalog::Poster::new(sender.clone()), activity.clone());
        let join = std::thread::spawn(move || {
            owner_loop(
                service,
                host,
                jobs,
                sources,
                catalog,
                completions,
                receiver,
                worker,
                owner_activity,
            )
        });
        Ok((
            Self {
                sender,
                next_client: Arc::new(AtomicU64::new(1)),
                activity,
                render,
            },
            join,
        ))
    }

    /// The board this owner's workers publish their long-running work to, which `activity.list`
    /// answers from. The desktop hands it to its preview queue, so a preview job is listed beside
    /// the owner's own work without passing through the owner.
    pub fn activity(&self) -> Arc<ActivityBoard> {
        self.activity.clone()
    }

    /// The render context every evaluation this owner plans reads, so a render the desktop runs
    /// itself shares its budgets and a diagnostic reads the same figures `resources.read` reports.
    pub fn render_context(&self) -> crate::RenderContext {
        self.render.clone()
    }

    /// Allocate an edit client's identity; its session starts as default on first use.
    pub fn register(&self) -> ClientId {
        ClientId(self.next_client.fetch_add(1, Ordering::Relaxed))
    }

    /// Allocate a client identity with a fixed authority. The owner learns it before any call the
    /// client makes and forgets it when the client disconnects. Only the desktop's own client and
    /// an explicitly started local setup process register with more than `Edit`.
    pub fn register_with(&self, authority: ClientAuthority) -> ClientId {
        let client = self.register();
        if authority != ClientAuthority::Edit {
            let _ = self
                .sender
                .send(OwnerMessage::Register { client, authority });
        }
        client
    }

    /// How many lane threads the owner's job table has started.
    #[cfg(test)]
    pub(crate) fn lane_threads(&self) -> usize {
        let (reply, answer) = sync_channel(1);
        self.sender
            .send(OwnerMessage::LaneThreads(reply))
            .expect("the owner is running");
        answer.recv().expect("the owner answered")
    }

    /// Have every export accepted from now on call `hold` as it begins each phase, or stop.
    #[cfg(test)]
    pub(crate) fn hold_exports(&self, hold: Option<export::Hold>) {
        self.sender
            .send(OwnerMessage::HoldExports(hold))
            .expect("the owner is running");
    }

    /// Have the point worker call `hold` before each evaluation, or stop holding it.
    #[cfg(test)]
    pub(crate) fn hold_points(&self, hold: Option<point::Hold>) {
        self.sender
            .send(OwnerMessage::HoldPoints(hold))
            .expect("the owner is running");
    }

    /// Relocate `asset_id` to `path` on the owner under `origin` ([`relocate`]), which no method
    /// exposes yet.
    #[cfg(test)]
    pub(crate) fn relocate(
        &self,
        origin: Origin,
        asset_id: AssetId,
        path: PathBuf,
    ) -> Result<crate::MutationOutcome, Error> {
        let (reply, answer) = sync_channel(1);
        self.sender
            .send(OwnerMessage::Relocate {
                origin,
                asset_id,
                path,
                reply,
            })
            .expect("the owner is running");
        answer.recv().expect("the owner answered")
    }

    /// Have the owner call `fault` with what it is about to serve ([`Fault`]), or stop calling it.
    #[cfg(test)]
    pub(crate) fn fault(&self, fault: Option<Fault>) {
        self.sender
            .send(OwnerMessage::Fault(fault))
            .expect("the owner is running");
    }

    /// How many `events.wait` calls the owner holds unanswered.
    #[cfg(test)]
    pub(crate) fn event_waits(&self) -> usize {
        let (reply, answer) = sync_channel(1);
        self.sender
            .send(OwnerMessage::EventWaits(reply))
            .expect("the owner is running");
        answer.recv().expect("the owner answered")
    }

    /// How many planned samples wait behind the one the point worker is evaluating.
    #[cfg(test)]
    pub(crate) fn points_waiting(&self) -> usize {
        let (reply, answer) = sync_channel(1);
        self.sender
            .send(OwnerMessage::PointsWaiting(reply))
            .expect("the owner is running");
        answer.recv().expect("the owner answered")
    }

    /// Wake `client` whenever another client's change reaches the event log: a request of another
    /// client's that recorded an event, or a change no request made — an import committed on the
    /// source worker, a capability job's result. The client's own requests do not wake it, because
    /// their answers already say what they changed. `wake` runs on the owner thread once per
    /// message that recorded such an event and must only post a signal; the client then reads
    /// `events.since` as it would have on a timer, so a client nothing happens to does no work at
    /// all. A later call replaces the waker, and disconnecting the client drops it.
    pub fn watch_events(&self, client: ClientId, wake: EventWake) {
        let _ = self.sender.send(OwnerMessage::WatchEvents { client, wake });
    }

    /// Block the calling thread until `job`, a source job of `client`'s, is no longer queued or
    /// running — it finished, failed, or `client` left it through `job.cancel` — or, with no job
    /// named, until any source job finishes, which is what makes room after a full source queue.
    /// It answers at once when there is nothing to wait for and reports nothing about the job: the
    /// caller reads `job.read` afterwards. A blocking receive on the caller's thread rather than
    /// a poll, so a client waiting on a decode costs nothing until it ends. Never call it on the
    /// owner thread or on an async executor's thread.
    pub fn wait_source(&self, client: ClientId, job: Option<&JobId>) -> Result<(), Error> {
        let (reply, answer) = sync_channel(1);
        self.sender
            .send(OwnerMessage::AwaitSource {
                client,
                job: job.cloned(),
                reply,
            })
            .map_err(|_| Error::protocol("catalog owner is unavailable"))?;
        answer
            .recv()
            .map_err(|_| Error::protocol("catalog owner stopped before the source job ended"))
    }

    /// Forget a client's session. Its committed edits and jobs are unaffected.
    pub fn disconnect(&self, client: ClientId) {
        let _ = self.sender.send(OwnerMessage::Disconnect(client));
    }

    pub fn call(&self, client: ClientId, request: ApiRequest) -> Result<ApiResponse, Error> {
        let (sender, receiver) = sync_channel(1);
        self.sender
            .send(OwnerMessage::Call(OwnerCall {
                client,
                request,
                response: sender,
            }))
            .map_err(|_| Error::protocol("catalog owner is unavailable"))?;
        receiver
            .recv()
            .map_err(|_| Error::protocol("catalog owner stopped before responding"))
    }

    pub fn stop(&self) {
        let _ = self.sender.send(OwnerMessage::Stop);
    }

    /// A preview job from the catalog owner. The owner resolves a named draft from the requesting
    /// client's own session; this is a desktop-internal path, not a JSON method.
    pub fn preview_job(&self, request: PreviewRequest) -> Result<PreviewJob, Error> {
        let (response, receiver) = sync_channel(1);
        self.sender
            .send(OwnerMessage::Preview { request, response })
            .map_err(|_| Error::protocol("catalog owner is unavailable"))?;
        receiver
            .recv()
            .map_err(|_| Error::protocol("catalog owner stopped before preview"))?
    }

    /// Hand the owner a report the caller's own preview worker produced for this identity. The next
    /// `analysis.request` for the same identity is then a cache hit, so a displayed target is never
    /// rendered twice. Fire and forget: it answers nothing and emits no event.
    pub fn submit_analysis(&self, identity: AnalysisIdentity, report: Report) {
        let _ = self.sender.send(OwnerMessage::AnalysisSubmitted {
            identity: Box::new(identity),
            report: Box::new(report),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn owner_loop(
    service: EditorService,
    host: CapabilityHost,
    jobs: Jobs,
    sources: SourceQueue,
    catalog: catalog::CatalogLanes,
    completions: SyncSender<OwnerMessage>,
    receiver: Receiver<OwnerMessage>,
    worker: JoinHandle<()>,
    activity: Arc<ActivityBoard>,
) {
    // One analysis worker with one active and one replaceable pending job, globally. The worker
    // wakes this loop when it has an outcome and starts its pending job itself; nothing here waits
    // on it or polls for it.
    let mut queue = AnalysisQueue::new(Arc::new(move || {
        let _ = completions.send(OwnerMessage::AnalysisReady);
    }));
    queue.set_activity(activity.clone());
    let mut owner = Owner {
        service,
        host,
        jobs,
        #[cfg(test)]
        export_hold: None,
        sources,
        sessions: HashMap::new(),
        queue,
        activity,
        latest_import: HashMap::new(),
        log: EventLog::default(),
        requests: RequestTable::default(),
        announced: Vec::new(),
        points: PointWorker::new(POINT_QUEUE_CAPACITY),
        watchers: HashMap::new(),
        notified: 0,
        source_waiters: Vec::new(),
        event_waits: Vec::new(),
        parking: None,
        catalog,
        #[cfg(test)]
        fault: None,
    };
    loop {
        // A wait past its deadline is answered before anything else is read, so a busy owner still
        // answers it on time; with none held the owner sleeps on a plain receive.
        owner.expire_event_waits();
        let message = match owner.next_event_deadline() {
            None => match receiver.recv() {
                Ok(message) => message,
                Err(_) => break,
            },
            Some(deadline) => {
                match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(message) => message,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        };
        // The client whose request this message is: the events it records do not wake that client.
        let caller = match &message {
            OwnerMessage::Call(call) => Some(call.client),
            _ => None,
        };
        // A panic while serving one message is contained: whoever the message owed an answer is
        // answered `internal`, and the owner serves the next message. Every durable write is one
        // transaction and the entry cache moves only after a commit, so nothing is left half done.
        let owed = Owed::of(&message);
        let served = catch_unwind(AssertUnwindSafe(|| {
            match message {
                OwnerMessage::Stop => return ControlFlow::Break(()),
                OwnerMessage::Call(call) => owner.call(call),
                #[cfg(test)]
                OwnerMessage::HoldPoints(hold) => owner.points.hold(hold),
                #[cfg(test)]
                OwnerMessage::PointsWaiting(reply) => {
                    let _ = reply.send(owner.points.waiting());
                }
                #[cfg(test)]
                OwnerMessage::Fault(fault) => owner.fault = fault,
                #[cfg(test)]
                OwnerMessage::EventWaits(reply) => {
                    let _ = reply.send(owner.event_waits.len());
                }
                #[cfg(test)]
                OwnerMessage::Relocate {
                    origin,
                    asset_id,
                    path,
                    reply,
                } => {
                    let _ = reply.send(relocate(&mut owner, &origin, &asset_id, &path));
                    owner.record_announced();
                }
                OwnerMessage::Preview { request, response } => {
                    let _ = response.send(owner.preview(request));
                }
                OwnerMessage::Register { client, authority } => {
                    owner.sessions.entry(client).or_default().authority = authority;
                }
                OwnerMessage::JobFinished { job_id, result } => {
                    if owner.jobs.kind(&job_id) == Some(JobKind::Export) {
                        export::finished(&mut owner, &job_id, result);
                    } else {
                        owner.host.finished(
                            &mut owner.jobs,
                            &mut owner.service,
                            &job_id,
                            result,
                            &mut owner.announced,
                        );
                    }
                    owner.record_announced();
                }
                #[cfg(test)]
                OwnerMessage::LaneThreads(reply) => {
                    let _ = reply.send(owner.jobs.lanes_started());
                }
                #[cfg(test)]
                OwnerMessage::HoldExports(hold) => owner.export_hold = hold,
                OwnerMessage::WatchEvents { client, wake } => {
                    owner.watchers.insert(client, wake);
                }
                OwnerMessage::AwaitSource { client, job, reply } => {
                    owner.await_source(client, job, reply);
                }
                OwnerMessage::Catalog(message) => owner.catalog_message(message),
                OwnerMessage::Disconnect(client) => owner.disconnect(client),
                OwnerMessage::SourceStarted(id) => owner.jobs.start(&id),
                OwnerMessage::SourceComplete(id, result) => owner.source_complete(&id, *result),
                OwnerMessage::AnalysisReady => {
                    // An outcome for a job that is no longer live — superseded, or stopped and
                    // already recorded — is discarded by the table.
                    while let Some(outcome) = owner.queue.poll() {
                        owner.jobs.finish(
                            &outcome.job_id,
                            outcome
                                .result
                                .map(|report| Output::Report(Box::new(report))),
                        );
                    }
                }
                OwnerMessage::AnalysisSubmitted { identity, report } => {
                    owner.analysis_submitted(*identity, *report);
                }
            }
            ControlFlow::Continue(())
        }));
        match served {
            Ok(ControlFlow::Break(())) => break,
            Ok(ControlFlow::Continue(())) => {}
            Err(_) => owner.contained(owed),
        }
        owner.notify_watchers(caller);
        owner.wake_event_waits();
    }
    // The sample being evaluated finishes; the ones waiting are dropped with every other call.
    owner.points.stop();
    let Owner {
        mut jobs,
        sources,
        catalog,
        ..
    } = owner;
    catalog.shutdown();
    // The lanes post into the receiver, so it goes first: a lane finishing as it stops is never
    // left waiting on a full channel while the owner waits for it. Every live job is asked to
    // stop, the source worker's included, and the lanes are joined; a running export stops at its
    // next row or block and removes its temporary file.
    drop(receiver);
    jobs.shutdown();
    // The source worker sees its stop on the memory gate, and its channel closes behind it.
    sources.gate.wake();
    drop(sources);
    let _ = worker.join();
}

/// The event log: the last [`EVENT_CAPACITY`] changes and the sequence of the newest. Every event
/// the owner emits is appended by [`EventLog::record`].
#[derive(Default)]
struct EventLog {
    events: VecDeque<ApiEvent>,
    sequence: u64,
}

impl EventLog {
    /// Append one event for a committed change, naming what it changed, and drop the oldest beyond
    /// the log's capacity.
    fn record(&mut self, origin: &Origin) {
        self.sequence = self.sequence.saturating_add(1);
        if self.events.len() == EVENT_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(ApiEvent {
            sequence: self.sequence,
            method: origin.method.clone(),
            request_id: origin.request_id.clone(),
            asset_id: origin.asset_id.clone(),
            revision: origin.revision,
        });
    }

    /// The events after `after`, and whether the client may have missed some: the log no longer
    /// holds them, or `after` is past the newest sequence, which a cursor kept from an earlier
    /// owner process is, so none of this process's events can be told apart from ones it read.
    fn since(&self, after: u64) -> EventsResult {
        self.since_naming(after, None)
    }

    /// [`EventLog::since`] keeping only the events that name `asset_id`, when one is given. A gap
    /// is reported as it is for every event, since the missed ones may have named it.
    fn since_naming(&self, after: u64, asset_id: Option<&AssetId>) -> EventsResult {
        let oldest = self
            .events
            .front()
            .map_or(self.sequence.saturating_add(1), |event| event.sequence);
        EventsResult {
            events: self
                .events
                .iter()
                .filter(|event| event.sequence > after)
                .filter(|event| asset_id.is_none_or(|asset| event.asset_id.as_ref() == Some(asset)))
                .cloned()
                .collect(),
            current_sequence: self.sequence,
            gap: after.saturating_add(1) < oldest || after > self.sequence,
        }
    }
}

/// The asset a service method's request changes, if it changes one: the asset its parameters name,
/// or the asset of the draft it names, which only this client's own session can hold. Read before
/// the method runs, because a commit ends the draft.
fn subject(session: &ClientSession, params: &Value) -> Option<AssetId> {
    if let Some(asset_id) = params.get("asset_id") {
        return serde_json::from_value(asset_id.clone()).ok();
    }
    let draft_id: DraftId = serde_json::from_value(params.get("draft_id")?.clone()).ok()?;
    session
        .held_draft(&draft_id)
        .ok()
        .map(|draft| draft.asset_id.clone())
}

/// One request an owner handler answers.
pub(super) struct Call<'a> {
    pub client: ClientId,
    pub request: &'a ApiRequest,
    /// The method and request identity an event this call announces names.
    pub origin: Origin,
}

/// Everything the catalog owner holds: the catalog, every client's session, the one job table and
/// the workers that schedule its source and analysis jobs, the capability host, the activity
/// board, the event log and the request table. The owner loop hands it every message; the owner
/// handlers of the method table read and change it.
pub(super) struct Owner {
    pub(super) service: EditorService,
    pub(super) host: CapabilityHost,
    /// Every job this owner runs, of every kind, and the lanes that run capability work and export.
    pub(super) jobs: Jobs,
    /// What every export accepted from now on calls as it begins each phase.
    #[cfg(test)]
    export_hold: Option<export::Hold>,
    /// The source worker's admission.
    sources: SourceQueue,
    sessions: HashMap<ClientId, ClientSession>,
    /// The analysis worker: one running job and one replaceable pending job.
    queue: AnalysisQueue,
    activity: Arc<ActivityBoard>,
    latest_import: HashMap<ClientId, JobId>,
    log: EventLog,
    requests: RequestTable,
    /// What the message being handled changed, each change once. The owner records them as events
    /// when the message is handled, before it answers.
    pub(super) announced: Vec<Origin>,
    /// Evaluates the samples through a spatial layer this owner planned, off its thread.
    points: PointWorker,
    /// The clients that asked to be woken by other clients' changes ([`OwnerHandle::watch_events`]).
    watchers: HashMap<ClientId, EventWake>,
    /// The newest event sequence the watchers have been woken for.
    notified: u64,
    /// Clients blocked until a source job ends ([`OwnerHandle::wait_source`]).
    source_waiters: Vec<SourceWaiter>,
    /// The `events.wait` calls the owner holds unanswered, at most one per client.
    event_waits: Vec<EventWait>,
    /// Set by the `events.wait` handler when it cannot answer yet: the wait [`Owner::call`] parks
    /// with the call's own reply channel, which the handler does not hold. Empty between calls.
    parking: Option<PendingWait>,
    /// The catalog lanes' owner-side state, one field per lane, each in its own file.
    catalog: catalog::CatalogLanes,
    #[cfg(test)]
    fault: Option<Fault>,
}

impl Owner {
    /// A client's authority, part of its session and fixed when it registered.
    pub(super) fn authority(&self, client: ClientId) -> ClientAuthority {
        self.sessions
            .get(&client)
            .map_or(ClientAuthority::Edit, |session| session.authority)
    }

    /// Answer one request: find its method, answer a retry from the request table when the method
    /// declares the owner answers its retries, otherwise call its handler, then record the events
    /// its changes announced. A sample through a spatial layer is only planned here: the point
    /// worker evaluates it and answers on the call's own channel, with the sequence the owner had
    /// now, while the owner moves on.
    fn call(&mut self, call: OwnerCall) {
        let OwnerCall {
            client,
            request,
            response,
        } = call;
        let result = self.answer(client, &request);
        self.record_announced();
        // A wait the handler could not answer yet is held with this call's reply channel.
        if let Some(wait) = self.parking.take()
            && result.is_ok()
        {
            self.park_event_wait(client, request.id, wait, response);
            return;
        }
        let reply = point::Reply {
            id: request.id,
            sequence: self.log.sequence,
            response,
        };
        match result {
            Ok(Planned::Sample(plan)) => self.points.submit(point::PointCall {
                client,
                evaluate: Box::new(move || Planned::Sample(plan).answer()),
                reply,
            }),
            Ok(Planned::Value(value)) => reply.answer(Ok(value)),
            Err(error) => reply.answer(Err(error)),
        }
    }

    fn answer(&mut self, client: ClientId, request: &ApiRequest) -> Result<Planned, Error> {
        let method = methods::find(&self.service, &request.method)
            .ok_or_else(|| Error::protocol(format!("unknown method {}", request.method)))?;
        // Every mutation envelope is checked here, once, before any handler runs.
        method.envelope().check(&request.params)?;
        // A retried mutation whose method declares the owner answers it is answered from the
        // request table, and its handler does not run again; the catalog answers the others.
        let key = match method.retries() {
            Retries::Owner => RequestKey::of(&request.method, &request.params),
            Retries::Catalog | Retries::None => None,
        };
        if let Some(key) = &key
            && let Some(first) = self.requests.answered(key)?
        {
            return Ok(Planned::Value(first));
        }
        let call = Call {
            client,
            request,
            origin: Origin::new(&request.method, &request.id),
        };
        #[cfg(test)]
        if let Some(fault) = &self.fault {
            fault(&request.method);
        }
        let result = match method.route() {
            Route::Owner(handler) => handler(self, &call).map(Planned::Value),
            // A task queues a capability job and announces nothing: the task is announced when it
            // succeeds. It samples its asset's current entry before it is queued, so an
            // unprepared source or artifact is refused naming what it needs, as below.
            Route::Task(task_id) => self
                .host
                .task(
                    &mut self.jobs,
                    &self.service,
                    task_id,
                    &request.params,
                    &call.origin,
                )
                .map(Planned::Value),
            Route::Service => {
                let session = self.sessions.entry(client).or_default();
                // Read only for a method that can change something, so a read or a draft's
                // `draft.set` pays nothing for it.
                let subject = method
                    .mutates()
                    .then(|| subject(session, &request.params))
                    .flatten();
                // The handler reports what it changed: a no-op and a retry answered from a
                // store's request log changed nothing, and an asset's change names its revision.
                method
                    .plan(&mut self.service, session, &request.params)
                    .map(|(planned, changed)| {
                        if let Changed::Something { revision } = changed {
                            let origin = match subject {
                                Some(asset_id) => call.origin.clone().changed(asset_id, revision),
                                None => call.origin.clone(),
                            };
                            announce_once(&mut self.announced, &origin);
                        }
                        planned
                    })
            }
        };
        // A stack whose source or artifacts are not prepared queues exactly what the refusal
        // names and answers with the job to wait for, whichever handler evaluated it.
        let result = result.map_err(|error| self.prepare(client, error));
        match (result, key) {
            (Ok(Planned::Value(mut value)), Some(key)) => {
                self.requests.record(key, &mut value);
                Ok(Planned::Value(value))
            }
            (result, _) => result,
        }
    }

    /// Wake every watcher but `caller` when the message just handled recorded an event: once per
    /// message however many it recorded, since a watcher reads them all in one `events.since`.
    fn notify_watchers(&mut self, caller: Option<ClientId>) {
        if self.log.sequence == self.notified {
            return;
        }
        self.notified = self.log.sequence;
        for (client, wake) in &self.watchers {
            if Some(*client) != caller {
                wake();
            }
        }
    }

    /// Answer a wait at once when nothing it names is queued or running, and hold it otherwise.
    fn await_source(&mut self, client: ClientId, job: Option<JobId>, reply: SyncSender<()>) {
        let pending = match &job {
            Some(id) => {
                self.jobs
                    .kind(id)
                    .is_some_and(|kind| kind.family() == Family::Source)
                    && self.jobs.wanted_by(id, client)
            }
            None => self.jobs.any_live(Family::Source),
        };
        if pending {
            self.source_waiters
                .push(SourceWaiter { client, job, reply });
        } else {
            let _ = reply.send(());
        }
    }

    /// Hold one `events.wait` until an event reaches it or its deadline passes. A client has at most
    /// one held: a second answers the first now with what the log holds, which is nothing it was
    /// waiting for, so a client cannot accumulate held state however it calls.
    fn park_event_wait(
        &mut self,
        client: ClientId,
        id: String,
        wait: PendingWait,
        response: SyncSender<ApiResponse>,
    ) {
        if let Some(earlier) = self
            .event_waits
            .iter()
            .position(|held| held.client == client)
        {
            let earlier = self.event_waits.swap_remove(earlier);
            self.answer_event_wait(earlier);
        }
        self.event_waits.push(EventWait {
            client,
            id,
            wait,
            seen: self.log.sequence,
            response,
        });
    }

    /// Answer one held wait with what `events.since` would answer now.
    fn answer_event_wait(&self, held: EventWait) {
        let since = self
            .log
            .since_naming(held.wait.after, held.wait.asset_id.as_ref());
        let response = match methods::value(since) {
            Ok(value) => ApiResponse::success(held.id, self.log.sequence, value),
            Err(error) => ApiResponse::failure(held.id, self.log.sequence, error),
        };
        // A caller that has gone has nobody to answer.
        let _ = held.response.send(response);
    }

    /// The soonest deadline among the held waits: how long the owner may sleep on its channel.
    fn next_event_deadline(&self) -> Option<Instant> {
        self.event_waits.iter().map(|held| held.wait.deadline).min()
    }

    /// Answer every held wait whose deadline has passed, with no events when none arrived.
    fn expire_event_waits(&mut self) {
        if self.event_waits.is_empty() {
            return;
        }
        let now = Instant::now();
        let (due, held): (Vec<_>, Vec<_>) = std::mem::take(&mut self.event_waits)
            .into_iter()
            .partition(|held| held.wait.deadline <= now);
        self.event_waits = held;
        for held in due {
            self.answer_event_wait(held);
        }
    }

    /// Answer every held wait the log now answers: an event past its cursor that names its asset,
    /// or a gap. Rescans the log only for a wait the log has moved past since it last looked.
    fn wake_event_waits(&mut self) {
        let sequence = self.log.sequence;
        if self.event_waits.iter().all(|held| held.seen == sequence) {
            return;
        }
        let log = &self.log;
        let (ready, held): (Vec<_>, Vec<_>) = std::mem::take(&mut self.event_waits)
            .into_iter()
            .partition(|held| {
                held.seen != sequence && {
                    let since = log.since_naming(held.wait.after, held.wait.asset_id.as_ref());
                    since.gap || !since.events.is_empty()
                }
            });
        self.event_waits = held;
        for held in &mut self.event_waits {
            held.seen = sequence;
        }
        for held in ready {
            self.answer_event_wait(held);
        }
    }

    /// Answer every wait `ended` says is over.
    fn release_waiters(&mut self, ended: impl Fn(&SourceWaiter) -> bool) {
        self.source_waiters.retain(|waiter| {
            if ended(waiter) {
                let _ = waiter.reply.send(());
                false
            } else {
                true
            }
        });
    }

    /// Answer what a message that panicked owed. A change it committed before the panic is
    /// durable, so its event is recorded. A caller reads one answer, so where the message already
    /// answered, that answer stands; a source job it already settled keeps its state.
    fn contained(&mut self, owed: Owed) {
        self.record_announced();
        let error = || Error::internal("the catalog owner failed while serving this message");
        match owed {
            Owed::Call(id, response) => {
                let _ = response.try_send(ApiResponse::failure(id, self.log.sequence, error()));
            }
            Owed::Preview(response) => {
                let _ = response.try_send(Err(error()));
            }
            Owed::Wait(reply) => {
                let _ = reply.try_send(());
            }
            Owed::Source(id) => {
                self.jobs.finish(&id, Err(error()));
                self.sources.complete(&id);
                self.release_waiters(|waiter| waiter.job.as_ref().is_none_or(|job| job == &id));
            }
            Owed::Nobody => {}
        }
    }

    /// Record every change the message just handled announced, once each.
    fn record_announced(&mut self) {
        for origin in std::mem::take(&mut self.announced) {
            self.log.record(&origin);
        }
    }

    /// A preview job for the requesting client. A draft is session state, so the owner looks it up
    /// in that client's own session: naming another client's draft, or one that has ended, is a
    /// validation error rather than a preview of someone else's gesture.
    fn preview(&mut self, request: PreviewRequest) -> Result<PreviewJob, Error> {
        // A whole preview of the entry the session selected is rendered as the session frames it,
        // so every frame of a framed selection — its first, a zoom, a refresh after another
        // client's commit — shows the same framing without each request repeating it.
        let framing = match (&request.entry_id, &request.draft, request.layer_count) {
            (Some(entry_id), None, None) => self
                .sessions
                .get(&request.client)
                .and_then(|session| session.preview.framing_of(&request.asset_id, entry_id))
                .cloned(),
            _ => None,
        };
        let draft = match &request.draft {
            None => None,
            Some(draft_id) => Some(
                self.sessions
                    .entry(request.client)
                    .or_default()
                    .held_draft(draft_id)?,
            ),
        };
        if request.analyse && request.layer_count.is_some() {
            return Err(Error::validation(
                "a truncated preview renders a layer prefix its identity does not describe, so it cannot be analysed",
            ));
        }
        let job = match (&request.entry_id, &framing) {
            (Some(entry_id), Some(geometry)) => self.service.framed_preview_job(
                &request.asset_id,
                entry_id,
                geometry,
                request.proxy,
            ),
            _ => self.service.preview_job(
                &request.asset_id,
                request.entry_id.as_ref(),
                request.layer_count,
                draft,
                request.proxy,
            ),
        };
        let job = job.map(|mut job| {
            job.analyse = request.analyse;
            job
        });
        // A stack whose source is not prepared queues that preparation and answers with the job to
        // wait for, exactly as a JSON request does.
        job.map_err(|error| self.prepare(request.client, error))
    }

    /// The one answer to a refusal that names what it needs prepared: queue exactly that as one
    /// source job ([`queue_preparation`]) and answer `preparation-required` naming the job to wait
    /// for, or the reason it could not be queued. Every other error, including a refusal that
    /// already names its job, is answered as it is.
    fn prepare(&mut self, client: ClientId, refused: Error) -> Error {
        let Some(needs) = refused.needs() else {
            return refused;
        };
        match queue_preparation(
            &self.service,
            &mut self.sources,
            &mut self.jobs,
            client,
            needs,
        ) {
            Ok(job) => refused.with_preparation(Preparation::Queued(job)),
            Err(error) => error,
        }
    }

    /// Forget a client's session. A gone client leaves every source and analysis job it wanted
    /// exactly as a cancel does, and the work nobody else wants is stopped; its capability and
    /// export jobs run on, since they belong to no client.
    fn disconnect(&mut self, client: ClientId) {
        self.sessions.remove(&client);
        for (job_id, kind) in self.jobs.disconnect(client) {
            self.stop(&job_id, kind);
        }
        self.latest_import.remove(&client);
        self.points.disconnect(client);
        self.watchers.remove(&client);
        // A gone client's waits are dropped unanswered: each receiver reports the owner gone.
        self.source_waiters.retain(|waiter| waiter.client != client);
        self.event_waits.retain(|held| held.client != client);
        self.catalog.disconnect(client);
    }

    /// A source job finished on the worker: commit what it prepared for the clients still waiting,
    /// and record the import event when it created an asset.
    fn source_complete(&mut self, id: &JobId, result: Result<SourceResult, Error>) {
        let interested = self.jobs.wanted(id);
        #[cfg(test)]
        let result = result.inspect(|_| {
            if let Some(fault) = &self.fault {
                fault("source.complete");
            }
        });
        let service = &mut self.service;
        let outcome = if interested {
            result.and_then(|prepared| match prepared {
                SourceResult::Prepared(prepared) => {
                    let (state, created) = service.complete_preparation(*prepared)?;
                    Ok(Completed::Asset(Box::new(state), created))
                }
                SourceResult::Collected(collected) => serde_json::to_value(collected)
                    .map(Completed::Value)
                    .map_err(|error| Error::internal(error.to_string())),
            })
        } else {
            Err(Error::conflict("source job cancelled"))
        };
        // An import is announced when it commits an asset, under the request that asked for it,
        // naming the asset it created.
        if let Ok(Completed::Asset(state, true)) = &outcome
            && let Some(origin) = self.jobs.origin(id)
        {
            let origin = origin
                .clone()
                .changed(state.asset.id.clone(), Some(state.revision));
            self.log.record(&origin);
        }
        self.jobs.finish(
            id,
            outcome.map(|completed| match completed {
                Completed::Asset(state, _) => Output::Asset(state),
                Completed::Value(value) => Output::Value(value),
            }),
        );
        self.sources.complete(id);
        // A queued preparation or redevelopment waits on the memory gate for these planes; an
        // artifact read allocates none and leaves them cached. Only a redevelopment of this same
        // mosaic keeps one development retained beside the one it builds.
        if let Some(kind) = self.jobs.development_in_flight() {
            self.service.evict_development(kind == JobKind::Develop);
        }
        // A wait for this job is over, and so is every wait for room on the source worker.
        self.release_waiters(|waiter| waiter.job.as_ref().is_none_or(|job| job == id));
    }

    /// Stop the work of a shared job whose last client just left it, on the worker that holds it.
    /// A source task sees its cancelled control between steps, and on the memory gate should it
    /// wait there. An analysis still in the pending slot is dropped and recorded `cancelled` now;
    /// a running one is abandoned, stops within a chunk and is recorded when its outcome arrives.
    fn stop(&mut self, job_id: &JobId, kind: JobKind) {
        match kind.family() {
            Family::Source => self.sources.gate.wake(),
            Family::Analysis => {
                let pending = self.queue.is_pending(job_id);
                let held = self.queue.withdraw(job_id);
                if pending || !held {
                    self.jobs.finish(job_id, Err(Error::cancelled(CANCELLED)));
                }
            }
            Family::Capability | Family::Export | Family::Catalog => {}
        }
    }

    /// One job as `client` reads it. An analysis the table records as live is `queued` while it
    /// waits in the worker's one replaceable slot and `running` once the worker has taken it, which
    /// only the worker knows.
    fn read_job(&self, job_id: &JobId, client: ClientId) -> Result<Value, Error> {
        let mut record = self.jobs.read_for(job_id, client)?;
        if record.kind == JobKind::Analysis && !record.status.is_finished() {
            record.status = if self.queue.is_pending(job_id) {
                JobStatus::Queued
            } else {
                JobStatus::Running
            };
        }
        methods::value(record)
    }

    /// A report the desktop's preview worker produced: it answers a live job for the same identity
    /// at once, and otherwise is kept as a finished job, so the next request for the identity is a
    /// cache hit. A job that already finished keeps its own outcome.
    fn analysis_submitted(&mut self, identity: AnalysisIdentity, report: Report) {
        let asset_id = identity.asset_id.clone();
        let key = JoinKey::Analysis(Box::new(identity));
        match self.jobs.keyed(&key).cloned() {
            Some(job_id) => {
                self.jobs
                    .finish(&job_id, Ok(Output::Report(Box::new(report))));
            }
            None => {
                self.jobs.open_finished(
                    Opened {
                        job_id: JobId::new(),
                        kind: JobKind::Analysis,
                        client: None,
                        key: Some(key),
                        asset_id: Some(asset_id),
                        control: JobControl::new(),
                    },
                    Output::Report(Box::new(report)),
                );
            }
        }
    }
}

/// `catalog.import`: queue the preparation, or answer from the verified cache. The import is
/// announced when it commits an asset, not here.
pub(super) fn catalog_import(
    owner: &mut Owner,
    call: &Call<'_>,
    params: Import,
) -> Result<Value, Error> {
    let (id, status) = match owner.service.importing(&params.path)? {
        Preparing::Ready(state) => (
            owner.sources.ready(&mut owner.jobs, call.client, *state)?,
            JobStatus::Ready,
        ),
        Preparing::Work(work, reads) => (
            queue_work(
                &owner.service,
                &mut owner.sources,
                &mut owner.jobs,
                call.client,
                work,
                reads,
            )?,
            JobStatus::Queued,
        ),
    };
    owner.jobs.set_origin(&id, call.origin.clone());
    owner.latest_import.insert(call.client, id.clone());
    Ok(json!({"job_id": id, "status": status}))
}

/// `job.read`: any job of any kind, in the one shape. A source or analysis job is read by the
/// clients that requested it; a capability or export job by any client.
pub(super) fn job_read(
    owner: &mut Owner,
    call: &Call<'_>,
    params: JobParams,
) -> Result<Value, Error> {
    owner.read_job(&params.job_id, call.client)
}

/// `job.cancel`, by the job's kind. A source or analysis job belongs to the clients that want it:
/// the caller leaves it, and the work stops only when no other client wants it. A capability or
/// export job belongs to no client: its cancel stops it for everyone. Answers the job as `job.read`
/// does afterwards.
///
/// Every cancel converges, so it carries no mutation envelope and a retry needs no stored answer:
/// a left job stays left, a cancelled one stays cancelled and a finished one is answered as it is.
pub(super) fn job_cancel(
    owner: &mut Owner,
    call: &Call<'_>,
    params: JobParams,
) -> Result<Value, Error> {
    let job_id = &params.job_id;
    let client = call.client;
    let kind = owner.jobs.kind_for(job_id, client)?;
    match kind.family() {
        Family::Source | Family::Analysis => {
            if owner.latest_import.get(&client) == Some(job_id) {
                owner.latest_import.remove(&client);
            }
            if owner.jobs.release(job_id, client)? == Release::Stopped {
                owner.stop(job_id, kind);
            }
            // The client left the job, so a wait of its own for it is over, whatever becomes of
            // the work.
            owner.release_waiters(|waiter| {
                waiter.client == client && waiter.job.as_ref() == Some(job_id)
            });
        }
        Family::Capability => owner
            .host
            .cancel(&mut owner.jobs, job_id, &mut owner.announced)?,
        Family::Export => {
            owner.jobs.cancel(job_id, export::CANCELLED);
        }
        Family::Catalog => {
            owner.jobs.cancel(job_id, CANCELLED);
            owner.catalog.cancelled(job_id, kind);
        }
    }
    owner.read_job(job_id, client)
}

/// `job.adopt`: the ready result of this client's latest import becomes its current asset.
pub(super) fn job_adopt(
    owner: &mut Owner,
    call: &Call<'_>,
    params: JobParams,
) -> Result<Value, Error> {
    if owner.latest_import.get(&call.client) != Some(&params.job_id) {
        return Err(Error::conflict("a newer import superseded this job"));
    }
    let state = match owner.jobs.outcome_for(&params.job_id, call.client)? {
        (JobStatus::Ready, Some(Output::Asset(state)), _) => (**state).clone(),
        (JobStatus::Queued | JobStatus::Running, ..) => {
            return Err(
                Error::preparation_required("the import is still being prepared")
                    .with_preparation(Preparation::Queued(params.job_id.clone())),
            );
        }
        (_, _, Some(error)) => return Err(error.clone()),
        _ => return Err(Error::validation("this job is not an import")),
    };
    let session = owner.sessions.entry(call.client).or_default();
    session.preview.return_current();
    session.touch();
    owner.latest_import.remove(&call.client);
    Ok(json!({"asset": state, "session": session}))
}

pub(super) fn source_prepare(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SourcePrepare,
) -> Result<Value, Error> {
    let needs = owner
        .service
        .entry_needs(&params.asset_id, params.entry_id.as_ref())?;
    let id = queue_preparation(
        &owner.service,
        &mut owner.sources,
        &mut owner.jobs,
        call.client,
        &needs,
    )?;
    Ok(json!({"job_id": id, "status": JobStatus::Queued}))
}

/// Point an asset at the file its original is now found at ([`EditorService::relocate`]) and
/// announce the change under `origin`, naming the asset and no revision, since its history did not
/// move — as naming a version is announced. A client watching the log reads the asset again to see
/// its new locator. A relocation to where the asset already is announces nothing.
///
/// The owner's half of the internal write the Locate milestone's command will make; no method
/// exposes it yet.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn relocate(
    owner: &mut Owner,
    origin: &Origin,
    asset_id: &AssetId,
    path: &Path,
) -> Result<crate::MutationOutcome, Error> {
    let outcome = owner.service.relocate(asset_id, path)?;
    if outcome == crate::MutationOutcome::Applied {
        announce_once(
            &mut owner.announced,
            &origin.clone().changed(asset_id.clone(), None),
        );
    }
    Ok(outcome)
}

/// `artifact.collect`: remove the collectable rows now, in one catalog transaction, and queue a
/// source job that removes their files, orphan files and stale staged files. Should the queue be
/// full, the removed rows' files are simply orphans the next collection removes. The collection is
/// a mutation the moment it is accepted, so other clients learn of it from the event log then.
pub(super) fn artifact_collect(
    owner: &mut Owner,
    call: &Call<'_>,
    _: Collect,
) -> Result<Value, Error> {
    let collection = owner.service.plan_collection()?;
    let root = collection.root.clone();
    let id = owner.sources.enqueue_maintenance(
        &mut owner.jobs,
        call.client,
        root,
        SourceTaskKind::Collect(collection),
    )?;
    announce_once(&mut owner.announced, &call.origin);
    Ok(json!({"job_id": id, "status": JobStatus::Queued}))
}

/// `activity.list`: one lock and a copy of at most 80 small entries.
pub(super) fn activity_list(owner: &mut Owner, _: &Call<'_>, _: NoParams) -> Result<Value, Error> {
    methods::value(owner.activity.snapshot())
}

/// `events.since`.
pub(super) fn events_since(
    owner: &mut Owner,
    _: &Call<'_>,
    params: EventsSince,
) -> Result<Value, Error> {
    methods::value(owner.log.since(params.after))
}

/// `events.wait`: answers as `events.since` does when the log already has something to report for
/// this cursor, and otherwise asks [`Owner::call`] to hold the reply until an event arrives or the
/// timeout passes. A timeout of 0 never holds.
pub(super) fn events_wait(
    owner: &mut Owner,
    _: &Call<'_>,
    params: EventsWait,
) -> Result<Value, Error> {
    let since = owner
        .log
        .since_naming(params.after, params.asset_id.as_ref());
    let timeout = Duration::from_millis(params.timeout_ms.unwrap_or(DEFAULT_EVENT_WAIT_MS));
    if since.gap || !since.events.is_empty() || timeout.is_zero() {
        return methods::value(since);
    }
    owner.parking = Some(PendingWait {
        after: params.after,
        asset_id: params.asset_id,
        deadline: Instant::now() + timeout,
    });
    Ok(Value::Null)
}

/// `analysis.request`. Everything the owner does here is bookkeeping and `O(layers)` planning: a
/// state read, the cached verified source, the draft's plan and one compile to learn the output
/// stage. No frame is allocated and nothing is rasterized on this thread. Answers the job as
/// `job.read` does.
pub(super) fn analysis_request(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AnalysisRequest,
) -> Result<Value, Error> {
    let selection = match &params.target {
        AnalysisTarget::Current => AnalysisSelection::Current,
        AnalysisTarget::Entry { entry_id } => AnalysisSelection::Entry(entry_id),
        AnalysisTarget::Draft { draft_id } => AnalysisSelection::Draft(
            owner
                .sessions
                .entry(call.client)
                .or_default()
                .held_draft(draft_id)?,
        ),
    };
    // A stack whose source or artifacts are not prepared is refused naming what it needs, which
    // the owner queues and answers with the job to wait for, as for every other evaluating request.
    let AnalysisPlan {
        identity,
        evaluation,
    } = owner.service.analysis_plan(&params.asset_id, selection)?;
    // An identical identity joins the job that already covers it, whether it is still running or
    // already holds a report: the same work is never done twice.
    let key = JoinKey::Analysis(Box::new(identity.clone()));
    let job_id = match owner.jobs.join(&key, call.client) {
        Some(job_id) => job_id,
        None => {
            let job_id = JobId::new();
            owner.jobs.open(Opened {
                job_id: job_id.clone(),
                kind: JobKind::Analysis,
                client: Some(call.client),
                key: Some(key),
                asset_id: Some(identity.asset_id.clone()),
                control: JobControl::new(),
            });
            match evaluation {
                // The effective recipe resolved but has no output stage the host can evaluate, so
                // no worker is started: the job fails at once and carries the reason.
                Err(error) => {
                    owner.jobs.finish(&job_id, Err(error));
                }
                Ok(evaluation) => {
                    if let Some(displaced) = owner.queue.submit(AnalysisJob {
                        job_id: job_id.clone(),
                        identity,
                        evaluation,
                    }) {
                        owner.jobs.supersede(&displaced);
                    }
                }
            }
            job_id
        }
    };
    owner.read_job(&job_id, call.client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewSource;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    use luxforge_testbase::paths::{jpeg as fixture, temp_path as temp};

    /// One JSON call against the owner, as an independent client would make it.
    fn send(
        owner: &OwnerHandle,
        client: ClientId,
        id: &str,
        method: &str,
        params: Value,
    ) -> ApiResponse {
        owner
            .call(
                client,
                ApiRequest {
                    id: id.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(owner: &OwnerHandle, client: ClientId, id: &str, method: &str, params: Value) -> Value {
        let response = send(owner, client, id, method, params);
        assert!(response.error.is_none(), "{id}: {:?}", response.error);
        response.result.expect("a result")
    }

    /// A fresh request envelope, so every call is a new request.
    fn envelope() -> Value {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        json!({
            "request_id": format!("owner-test-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            "actor": "test",
        })
    }

    /// `catalog.import` of one file, as a new request.
    fn import_params(path: impl serde::Serialize) -> Value {
        json!({"path": path, "mutation": envelope()})
    }

    /// Import a fixture the way every client must now: the owner acknowledges with a job id, the
    /// bounded source worker prepares the original, and `job.adopt` hands back the asset state.
    fn import_asset(owner: &OwnerHandle, client: ClientId, path: &std::path::Path) -> Value {
        let queued = ok(
            owner,
            client,
            "import",
            "catalog.import",
            import_params(path),
        );
        let job_id = queued["job_id"].as_str().expect("a job id").to_owned();
        luxforge_testbase::wait_until("the import to become ready", || {
            let status = ok(
                owner,
                client,
                "status",
                "job.read",
                json!({"job_id": job_id}),
            );
            match status["status"].as_str() {
                Some("ready") => true,
                Some("queued" | "running") => false,
                other => panic!("unexpected import job {other:?}: {status}"),
            }
        });
        ok(
            owner,
            client,
            "adopt",
            "job.adopt",
            json!({"job_id": job_id}),
        )["asset"]
            .clone()
    }

    fn failure(
        owner: &OwnerHandle,
        client: ClientId,
        id: &str,
        method: &str,
        params: Value,
    ) -> super::super::ApiFailure {
        send(owner, client, id, method, params)
            .error
            .unwrap_or_else(|| panic!("{id} was expected to fail"))
    }

    /// Poll `job.read` until the job leaves `queued` or `running`. Nothing in the owner
    /// polls: this is the test standing in for a client that would rather be told.
    fn settled(owner: &OwnerHandle, client: ClientId, job_id: &Value) -> Value {
        luxforge_testbase::wait_for("the analysis job to settle", || {
            let read = ok(owner, client, "read", "job.read", json!({"job_id": job_id}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    /// The exact report the contract says a target must produce: render that entry's own stack from
    /// the verified source and reduce the resulting raster. This is the independent reference the
    /// API answer is compared against, not a copy of the API's own arithmetic.
    fn expected_report(owner: &OwnerHandle, request: PreviewRequest) -> Value {
        let job = owner.preview_job(request).expect("a preview job");
        let raster = job
            .evaluation
            .source()
            .render(
                job.evaluation.registry(),
                job.evaluation.entry().snapshot.id.clone(),
                job.evaluation.recipe(),
            )
            .expect("a rendered frame");
        serde_json::to_value(
            crate::analysis::reduce(
                &raster.rgba,
                raster.width,
                raster.height,
                &crate::Cancel::never(),
            )
            .expect("a reduction"),
        )
        .expect("an encodable report")
    }

    fn pixel_edit(
        owner: &OwnerHandle,
        client: ClientId,
        asset: &Value,
        revision: u64,
        request: &str,
        rgb: [u8; 3],
    ) -> Value {
        ok(
            owner,
            client,
            request,
            "edit.set-pixel",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": revision, "request_id": request, "actor": "test"},
                "x": 0, "y": 0, "rgb": rgb,
            }),
        )
    }

    fn request(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> ApiResponse {
        owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap()
    }

    fn wait_source(owner: &OwnerHandle, client: ClientId, id: &str) -> Value {
        luxforge_testbase::wait_for("the source job to complete", || {
            let status = request(owner, client, "job.read", json!({"job_id":id}));
            assert!(status.error.is_none(), "{:?}", status.error);
            let status = status.result.unwrap();
            match status["status"].as_str() {
                Some("ready" | "failed") => Some(status),
                Some("queued" | "running") => None,
                other => panic!("source job did not complete: {other:?}: {status}"),
            }
        })
    }

    /// A source job's settled status, waiting as long as a photo-sized RAW development takes.
    fn wait_development(owner: &OwnerHandle, client: ClientId, id: &str) -> Value {
        luxforge_testbase::wait_for("the source job to settle", || {
            let status = ok(owner, client, "status", "job.read", json!({"job_id": id}));
            (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
        })
    }

    /// On a real RAW file: `render.sample` names no entry, so it samples the session's selection.
    /// A historical entry whose white balance neither development the owner holds has is refused
    /// naming that entry's development rather than the current one's, and the sample converges
    /// after the one job that prepares it. The development that job replaces waits in the second
    /// slot, so going back to its entry, and then to the Original again, samples at once with no
    /// job. Run with LUXFORGE_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_sample_of_a_historical_raw_entry_prepares_that_entry_and_converges_after_one_job() {
        let path =
            PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("LUXFORGE_RAW_FIXTURE"));
        let catalog = temp("historical-raw-sample.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let state = import_asset(&owner, client, &path);
        let asset = state["asset"]["id"].clone();
        let original = state["current_entry"]["id"].clone();
        let sample = |id: &str| {
            send(
                &owner,
                client,
                id,
                "render.sample",
                json!({"asset_id": asset, "x": 100, "y": 100}),
            )
        };
        let select = |id: &str, entry: &Value| {
            ok(
                &owner,
                client,
                id,
                "preview.select",
                json!({"asset_id": asset, "entry_id": entry}),
            );
        };

        // Two custom white balances in turn become current, and each development is prepared:
        // the current one holds the second, the second slot the first, and neither the Original's.
        let mut current = Value::Null;
        for (revision, temperature) in [(0, 3500.0), (1, 6500.0)] {
            let request = format!("temperature-{revision}");
            current = ok(
                &owner,
                client,
                &request,
                "edit.set-raw",
                json!({
                    "asset_id": asset,
                    "mutation": {"expected_revision": revision, "request_id": request, "actor": "test"},
                    "temperature": temperature,
                }),
            )["current_entry_id"]
                .clone();
            let prepared = ok(
                &owner,
                client,
                "prepare",
                "source.prepare",
                json!({"asset_id": asset}),
            );
            let job = prepared["job_id"].as_str().unwrap();
            assert_eq!(wait_development(&owner, client, job)["status"], "ready");
            assert!(
                sample("current").error.is_none(),
                "the current entry samples"
            );
        }

        // The Original, selected, holds the as-shot white balance neither development holds.
        select("select", &original);
        let refused = sample("historical").error.expect("a refusal");
        assert_eq!(refused.code, "preparation-required", "{refused:?}");
        let job = refused.job_id.expect("the job preparing the Original");
        assert_eq!(wait_development(&owner, client, &job)["status"], "ready");
        let sampled = sample("again");
        assert!(sampled.error.is_none(), "{:?}", sampled.error);
        assert_eq!(sampled.result.unwrap()["entry_id"], original);

        // Back and forth between the two: each is answered from the development the owner holds,
        // with no refusal and so no job.
        for (id, entry) in [("current", &current), ("original", &original)] {
            select(id, entry);
            let sampled = sample(id);
            assert!(sampled.error.is_none(), "{id}: {:?}", sampled.error);
            assert_eq!(&sampled.result.unwrap()["entry_id"], entry);
        }
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    #[ignore = "requires two private photo-sized RAW fixtures"]
    fn queued_distinct_raw_imports_release_first_float_before_second_development() {
        let first_path =
            PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE_A").expect("first RAW fixture"));
        let second_path =
            PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE_B").expect("second RAW fixture"));
        let catalog = temp("queued-two-raw.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let first = owner.register();
        let second = owner.register();
        let a = request(&owner, first, "catalog.import", import_params(&first_path))
            .result
            .unwrap();
        let b = request(
            &owner,
            second,
            "catalog.import",
            import_params(&second_path),
        )
        .result
        .unwrap();
        let a_id = a["job_id"].as_str().unwrap();
        let b_id = b["job_id"].as_str().unwrap();
        luxforge_testbase::wait_until(
            "both RAW imports, the second not stalled behind the retained first float",
            || {
                let a_status = request(&owner, first, "job.read", json!({"job_id":a_id}))
                    .result
                    .unwrap();
                let b_status = request(&owner, second, "job.read", json!({"job_id":b_id}))
                    .result
                    .unwrap();
                assert_ne!(a_status["status"], "failed", "{a_status}");
                assert_ne!(b_status["status"], "failed", "{b_status}");
                a_status["status"] == "ready" && b_status["status"] == "ready"
            },
        );
        let assets = request(&owner, first, "catalog.list", json!({}))
            .result
            .unwrap()["assets"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(assets.len(), 2);
        for (client, asset) in [(first, &assets[0]), (second, &assets[1])] {
            let asset_id = &asset["id"];
            luxforge_testbase::wait_until("the evicted RAW to become sampleable", || {
                let sample = request(
                    &owner,
                    client,
                    "render.sample",
                    json!({"asset_id":asset_id,"x":100,"y":100}),
                );
                match sample.error {
                    None => true,
                    Some(error) if error.code == "preparation-required" => {
                        let prepared = luxforge_testbase::wait_for("the RAW preparation", || {
                            let status =
                                request(&owner, client, "job.read", json!({"job_id":error.job_id}))
                                    .result
                                    .unwrap();
                            match status["status"].as_str() {
                                Some("ready" | "failed") => Some(status),
                                Some("queued" | "running") => None,
                                other => panic!("invalid source state: {other:?}"),
                            }
                        });
                        assert_eq!(prepared["status"], "ready", "{prepared}");
                        false
                    }
                    Some(error) => panic!("RAW sample failed: {error:?}"),
                }
            });
        }
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    #[ignore = "requires two private photo-sized RAW fixtures"]
    fn distinct_mosaic_admission_rejects_then_accepts_after_completion() {
        let first_path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE_A").unwrap());
        let second_path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE_B").unwrap());
        let cancel = AtomicBool::new(false);
        let first_sensor = Arc::new(
            luxforge_raw::RawSource::decode(
                Arc::from(std::fs::read(&first_path).unwrap()),
                &cancel,
            )
            .unwrap(),
        );
        let second_sensor = Arc::new(
            luxforge_raw::RawSource::decode(
                Arc::from(std::fs::read(&second_path).unwrap()),
                &cancel,
            )
            .unwrap(),
        );
        let request =
            |path: &Path, sensor: Arc<luxforge_raw::RawSource>| crate::editor::RawDevelopment {
                asset_id: AssetId::new(),
                signature: EditorService::request_signature(path).unwrap().1,
                fingerprint: "test".into(),
                gains: sensor.metadata().as_shot_gains,
                sensor,
                capture: Arc::default(),
                file_name: None,
            };
        let a = request(&first_path, first_sensor);
        let b = request(&second_path, second_sensor);
        let (mut sources, mut jobs, receiver) = source_queue();
        let first = sources
            .enqueue(
                &mut jobs,
                ClientId(1),
                SourceWork::Develop(Box::new(a.clone())),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            sources
                .enqueue(
                    &mut jobs,
                    ClientId(2),
                    SourceWork::Develop(Box::new(a)),
                    Vec::new()
                )
                .unwrap(),
            first
        );
        let refusal = sources
            .enqueue(
                &mut jobs,
                ClientId(3),
                SourceWork::Develop(Box::new(b.clone())),
                Vec::new(),
            )
            .unwrap_err();
        assert_eq!(refusal.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            refusal.detail,
            "RAW mosaic queue is full; retry after the active development"
        );
        assert!(refusal.retries_after_source_job(), "{:?}", refusal.data);
        drop(receiver.try_recv().unwrap());
        jobs.finish(&first, Err(Error::conflict("finished")));
        sources.complete(&first);
        assert!(
            sources
                .enqueue(
                    &mut jobs,
                    ClientId(3),
                    SourceWork::Develop(Box::new(b)),
                    Vec::new()
                )
                .is_ok()
        );
    }

    /// A full source preparation queue refuses the next job as `resource-limit` whose data says a
    /// source job ending makes room, with its message unchanged; once the worker takes one task,
    /// the same request is admitted.
    #[test]
    fn a_full_source_queue_refuses_with_data_that_says_retry_after_a_source_job() {
        let (mut sources, mut jobs, receiver) = source_queue();
        for _ in 0..SOURCE_QUEUE_CAPACITY {
            sources
                .enqueue(
                    &mut jobs,
                    ClientId(1),
                    SourceWork::Artifacts(AssetId::new()),
                    Vec::new(),
                )
                .unwrap();
        }
        let asset = AssetId::new();
        let refusal = sources
            .enqueue(
                &mut jobs,
                ClientId(1),
                SourceWork::Artifacts(asset.clone()),
                Vec::new(),
            )
            .unwrap_err();
        assert_eq!(refusal.kind, ErrorKind::ResourceLimit);
        assert_eq!(refusal.detail, "source preparation queue is full");
        assert_eq!(
            refusal.data.as_deref(),
            Some(&json!({"retry": "after-source-job"}))
        );
        assert!(refusal.retries_after_source_job());
        drop(receiver.try_recv().unwrap());
        assert!(
            sources
                .enqueue(
                    &mut jobs,
                    ClientId(1),
                    SourceWork::Artifacts(asset),
                    Vec::new()
                )
                .is_ok()
        );
    }

    /// The source worker's queue over a job table of its own, and the worker's end of its channel.
    fn source_queue() -> (SourceQueue, Jobs, Receiver<SourceTask>) {
        let (sender, receiver) = sync_channel(SOURCE_QUEUE_CAPACITY);
        let jobs = Jobs::new(Arc::new(|_, _| {}), ActivityBoard::new());
        (SourceQueue::new(sender, Arc::default()), jobs, receiver)
    }

    /// A second request for the same flight joins the queued job; one client leaving it leaves the
    /// work for the other, and the last leaving stops it, whose flight then starts afresh.
    #[test]
    fn pending_source_jobs_deduplicate_and_cancellation_keeps_other_waiters() {
        let (mut sources, mut jobs, receiver) = source_queue();
        let first = ClientId(1);
        let second = ClientId(2);
        let id = sources
            .enqueue(
                &mut jobs,
                first,
                SourceWork::file(&fixture(), None).unwrap(),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            sources
                .enqueue(
                    &mut jobs,
                    second,
                    SourceWork::file(&fixture(), None).unwrap(),
                    Vec::new()
                )
                .unwrap(),
            id
        );
        assert_eq!(jobs.release(&id, first).unwrap(), Release::Kept);
        assert_eq!(
            jobs.read_for(&id, second).unwrap().status,
            JobStatus::Queued
        );
        assert_eq!(
            jobs.disconnect(second),
            vec![(id.clone(), JobKind::Prepare)]
        );
        let task = receiver.try_recv().unwrap();
        assert_eq!(task.id, id);
        assert!(task.control.is_cancelled(), "the worker sees the stop");
        assert_ne!(
            sources
                .enqueue(
                    &mut jobs,
                    first,
                    SourceWork::file(&fixture(), None).unwrap(),
                    Vec::new()
                )
                .unwrap(),
            id
        );
    }

    /// A completion evicts the cached development only while a queued job will allocate planes of
    /// its own: a preparation of an original or a redevelopment. A flight that only reads
    /// artifacts allocates none, so a completion beside it leaves the development cached.
    #[test]
    fn only_a_queued_preparation_or_redevelopment_evicts_the_cached_development() {
        let (mut sources, mut jobs, _receiver) = source_queue();
        let client = ClientId(1);
        assert_eq!(jobs.development_in_flight(), None, "nothing is queued");
        let reads = sources
            .enqueue(
                &mut jobs,
                client,
                SourceWork::Artifacts(AssetId::new()),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            jobs.development_in_flight(),
            None,
            "an artifact read allocates no planes"
        );
        let file = sources
            .enqueue(
                &mut jobs,
                client,
                SourceWork::file(&fixture(), None).unwrap(),
                Vec::new(),
            )
            .unwrap();
        assert_eq!(
            jobs.development_in_flight(),
            Some(JobKind::Prepare),
            "a preparation of an original allocates planes"
        );
        jobs.finish(&file, Err(Error::conflict("finished")));
        assert_eq!(
            jobs.development_in_flight(),
            None,
            "a finished preparation is no longer in flight, and the artifact read never was"
        );
        jobs.finish(&reads, Err(Error::conflict("finished")));
        assert_eq!(jobs.development_in_flight(), None);
    }

    #[test]
    fn changed_signature_starts_a_new_flight_instead_of_attaching_to_old_bytes() {
        let (mut sources, mut jobs, _receiver) = source_queue();
        let path = temp("source-flight-changed.jpg");
        std::fs::copy(fixture(), &path).unwrap();
        let first = sources
            .enqueue(
                &mut jobs,
                ClientId(1),
                SourceWork::file(&path.clone(), None).unwrap(),
                Vec::new(),
            )
            .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = 0;
        std::fs::write(&path, bytes).unwrap();
        let second = sources
            .enqueue(
                &mut jobs,
                ClientId(2),
                SourceWork::file(&path.clone(), None).unwrap(),
                Vec::new(),
            )
            .unwrap();
        assert_ne!(first, second);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_newer_import_refuses_adoption_of_an_older_completed_job() {
        let catalog = temp("source-adopt.sqlite");
        let older = temp("source-adopt-older.jpg");
        let newer = temp("source-adopt-newer.jpg");
        let _ = std::fs::remove_file(&catalog);
        std::fs::copy(fixture(), &older).unwrap();
        std::fs::copy(fixture(), &newer).unwrap();
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let a = request(&owner, client, "catalog.import", import_params(&older))
            .result
            .unwrap();
        let b = request(&owner, client, "catalog.import", import_params(&newer))
            .result
            .unwrap();
        let first = a["job_id"].as_str().unwrap();
        let second = b["job_id"].as_str().unwrap();
        assert_eq!(wait_source(&owner, client, first)["status"], "ready");
        let expected = wait_source(&owner, client, second)["result"]["asset"]["id"].clone();
        let stale = request(&owner, client, "job.adopt", json!({"job_id":first}));
        assert_eq!(stale.error.unwrap().code, "conflict");
        let adopted = request(&owner, client, "job.adopt", json!({"job_id":second}))
            .result
            .unwrap();
        assert_eq!(adopted["asset"]["asset"]["id"], expected);
        assert_eq!(adopted["session"]["preview"]["selections"], json!({}));
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(older).unwrap();
        std::fs::remove_file(newer).unwrap();
    }

    /// One `events.wait` on its own thread, as a client blocked on a connection makes it: the
    /// channel yields the response, or `None` when the owner dropped the call unanswered.
    fn wait_on_thread(
        owner: &OwnerHandle,
        client: ClientId,
        id: &str,
        params: Value,
    ) -> std::sync::mpsc::Receiver<Option<ApiResponse>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let owner = owner.clone();
        let request = ApiRequest {
            id: id.into(),
            method: "events.wait".into(),
            params,
            token: None,
        };
        thread::spawn(move || {
            let _ = sender.send(owner.call(client, request).ok());
        });
        receiver
    }

    /// A wait the log has nothing for is parked on the owner, which does not block: another client's
    /// change past its cursor answers it with what `events.since` would, and one that names another
    /// asset than its filter does not, so it answers empty at its timeout, with the sequence the
    /// log had by then.
    #[test]
    fn a_parked_wait_wakes_on_the_next_event_past_its_cursor() {
        let catalog = temp("event-wait-wake.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let waiter = owner.register();
        let agent = owner.register();
        let asset = import_asset(&owner, agent, &fixture())["asset"]["id"].clone();
        let cursor =
            ok(&owner, agent, "since", "events.since", json!({"after": 0}))["current_sequence"]
                .as_u64()
                .unwrap();
        assert!(cursor > 0, "the import recorded an event");

        // Parked: the owner answers other calls while it holds the wait.
        let woken = wait_on_thread(
            &owner,
            waiter,
            "wake",
            json!({"after": cursor, "timeout_ms": 30_000, "asset_id": asset}),
        );
        luxforge_testbase::wait_until("the wait to be parked", || owner.event_waits() == 1);
        assert!(woken.try_recv().is_err(), "nothing has happened yet");
        let started = Instant::now();
        let stranger = wait_on_thread(
            &owner,
            agent,
            "stranger",
            json!({"after": cursor, "timeout_ms": 400, "asset_id": AssetId::new()}),
        );
        luxforge_testbase::wait_until("both waits to be parked", || owner.event_waits() == 2);

        ok(
            &owner,
            agent,
            "version",
            "version.create",
            json!({"asset_id": asset, "name": "Woken", "mutation": envelope()}),
        );
        let response = woken
            .recv_timeout(luxforge_testbase::HANG)
            .expect("the wake answered")
            .expect("an answer");
        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(response.id, "wake");
        let answer = response.result.unwrap();
        assert_eq!(answer["gap"], json!(false));
        assert_eq!(answer["current_sequence"], json!(cursor + 1));
        assert_eq!(answer["events"].as_array().unwrap().len(), 1);
        assert_eq!(answer["events"][0]["method"], json!("version.create"));
        assert_eq!(answer["events"][0]["asset_id"], asset);
        assert_eq!(owner.event_waits(), 1, "only the stranger's wait is left");

        // The other asset's filter saw the same event and stayed parked until its deadline.
        let response = stranger
            .recv_timeout(luxforge_testbase::HANG)
            .expect("the timeout answered")
            .expect("an answer");
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "answered before its deadline"
        );
        let answer = response.result.unwrap();
        assert_eq!(answer["events"], json!([]));
        assert_eq!(answer["current_sequence"], json!(cursor + 1));
        assert_eq!(owner.event_waits(), 0);
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// With nothing to report a wait answers empty at its timeout; a zero timeout answers at once
    /// as `events.since` does; a cursor the log is already past, or beyond, answers at once with
    /// what `events.since` would; and a timeout above the limit is refused, not clamped.
    #[test]
    fn an_events_wait_answers_at_its_timeout_and_at_once_when_the_log_already_has_something() {
        let catalog = temp("event-wait-timeout.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let wait = |params: Value| request(&owner, client, "events.wait", params);

        let started = Instant::now();
        let empty = wait(json!({"after": 0, "timeout_ms": 150})).result.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(150));
        assert_eq!(empty["events"], json!([]));
        assert_eq!(empty["gap"], json!(false));
        assert_eq!(owner.event_waits(), 0, "nothing stays parked");

        let started = Instant::now();
        let now = wait(json!({"after": 0, "timeout_ms": 0})).result.unwrap();
        assert!(started.elapsed() < Duration::from_millis(150));
        assert_eq!(now, empty);

        let refused = wait(json!({"after": 0, "timeout_ms": 30_001}))
            .error
            .unwrap();
        assert_eq!(refused.code, "validation");
        assert_eq!(
            wait(json!({"after": 0, "timeout_ms": 30_000, "typo": 1}))
                .error
                .unwrap()
                .code,
            "validation"
        );
        assert_eq!(wait(json!({})).error.unwrap().code, "validation");

        import_asset(&owner, client, &fixture());
        // The log is already past 0, so there is nothing to wait for, whatever the timeout.
        let started = Instant::now();
        let ready = wait(json!({"after": 0, "timeout_ms": 30_000}))
            .result
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        let since = ok(&owner, client, "since", "events.since", json!({"after": 0}));
        assert_eq!(ready, since);
        assert_eq!(ready["events"].as_array().unwrap().len(), 1);
        // A cursor beyond the log is a gap, answered at once as events.since answers it.
        let ahead = wait(json!({"after": 99, "timeout_ms": 30_000}))
            .result
            .unwrap();
        assert_eq!(ahead["gap"], json!(true));
        assert_eq!(owner.event_waits(), 0);
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A client holds at most one wait: its next answers the earlier one at once with what it has,
    /// and takes its place; other clients' waits are unaffected; and a disconnect drops the client's
    /// wait unanswered while the owner keeps serving.
    #[test]
    fn a_client_holds_one_parked_wait_and_a_disconnect_drops_it() {
        let catalog = temp("event-wait-bound.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let first = owner.register();
        let second = owner.register();
        let long = json!({"after": 0, "timeout_ms": 30_000});

        let earlier = wait_on_thread(&owner, first, "earlier", long.clone());
        luxforge_testbase::wait_until("the first wait", || owner.event_waits() == 1);
        let other = wait_on_thread(&owner, second, "other", long.clone());
        luxforge_testbase::wait_until("the second client's wait", || owner.event_waits() == 2);
        let later = wait_on_thread(&owner, first, "later", long);
        // The earlier wait answers at once, empty, and the later one takes its place.
        let response = earlier
            .recv_timeout(luxforge_testbase::HANG)
            .expect("the earlier wait was answered")
            .expect("an answer");
        assert_eq!(response.id, "earlier");
        assert_eq!(response.result.unwrap()["events"], json!([]));
        luxforge_testbase::wait_until("the later wait to replace it", || owner.event_waits() == 2);
        assert!(other.try_recv().is_err() && later.try_recv().is_err());

        owner.disconnect(first);
        assert_eq!(
            later
                .recv_timeout(luxforge_testbase::HANG)
                .unwrap()
                .map(|_| ()),
            None,
            "a disconnected client's wait is dropped unanswered"
        );
        assert_eq!(owner.event_waits(), 1, "the other client's wait stays");
        owner.disconnect(second);
        assert_eq!(
            other
                .recv_timeout(luxforge_testbase::HANG)
                .unwrap()
                .map(|_| ()),
            None
        );
        assert_eq!(owner.event_waits(), 0);
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A watcher is woken once for each change another client makes — a request of theirs that
    /// records an event, or an import committed on the source worker — and never by its own
    /// requests, by a read, or once it has disconnected. Every wake follows the message that
    /// recorded the event, so a call made after it is answered only once the wake has run.
    #[test]
    fn a_watcher_is_woken_by_other_clients_changes_and_never_by_its_own() {
        let catalog = temp("event-watch.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let desktop = owner.register();
        let agent = owner.register();
        let wakes = Arc::new(AtomicU64::new(0));
        let counted = wakes.clone();
        owner.watch_events(
            desktop,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::Relaxed);
            }),
        );
        // Every wake a message causes has run once a later call is answered.
        let woken = || {
            ok(&owner, agent, "fence", "events.since", json!({"after": 0}));
            wakes.load(Ordering::Relaxed)
        };
        assert_eq!(woken(), 0, "nothing has changed");

        // The agent's import commits on the source worker, under no request of the desktop's.
        let queued = ok(
            &owner,
            agent,
            "import",
            "catalog.import",
            import_params(fixture()),
        );
        let job = JobId::parse(queued["job_id"].as_str().unwrap()).unwrap();
        owner.wait_source(agent, Some(&job)).unwrap();
        assert_eq!(woken(), 1, "another client's import wakes the watcher");
        let asset =
            ok(&owner, agent, "adopt", "job.adopt", json!({"job_id": job}))["asset"]["asset"]["id"]
                .clone();

        let version = |client: ClientId, name: &str| {
            ok(
                &owner,
                client,
                name,
                "version.create",
                json!({"asset_id": asset, "name": name, "mutation": envelope()}),
            );
        };
        version(desktop, "Mine");
        assert_eq!(woken(), 1, "the desktop's own change does not wake it");
        version(agent, "Theirs");
        assert_eq!(woken(), 2, "another client's change does");
        ok(
            &owner,
            agent,
            "read",
            "asset.state",
            json!({"asset_id": asset}),
        );
        assert_eq!(woken(), 2, "a read changes nothing and wakes nobody");

        owner.disconnect(desktop);
        version(agent, "After");
        assert_eq!(woken(), 2, "a disconnected client is not woken");
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A wait for a source job blocks while the job is held on the worker and answers when the
    /// client leaves it; a wait for room answers when any source job ends; and a wait with nothing
    /// to wait for answers at once. Nothing polls: each answer is the owner handling the message
    /// that ended the wait.
    #[test]
    fn a_source_wait_answers_when_its_job_ends_or_its_client_leaves_it() {
        let catalog = temp("source-wait.sqlite");
        let photo = temp("source-wait.jpg");
        let _ = std::fs::remove_file(&catalog);
        std::fs::copy(fixture(), &photo).unwrap();
        let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let hold = gate.clone();
        let (owner, join) = OwnerHandle::start_observed(
            &catalog,
            Arc::new(ModuleRegistry::builtin()),
            ActivityBoard::new(),
            Some(Arc::new(move || hold.pass())),
        )
        .unwrap();
        let client = owner.register();
        owner
            .wait_source(client, None)
            .expect("nothing is queued, so a wait for room answers at once");
        let unknown = JobId::new();
        owner
            .wait_source(client, Some(&unknown))
            .expect("a job the client does not hold is nothing to wait for");

        gate.shut();
        let queued = ok(
            &owner,
            client,
            "import",
            "catalog.import",
            import_params(&photo),
        );
        let job = JobId::parse(queued["job_id"].as_str().unwrap()).unwrap();
        let wait = |job: Option<JobId>| {
            let owner = owner.clone();
            let (done, answered) = sync_channel(1);
            thread::spawn(move || {
                let _ = done.send(owner.wait_source(client, job.as_ref()));
            });
            answered
        };
        let for_job = wait(Some(job.clone()));
        let for_room = wait(None);
        assert!(
            for_job.recv_timeout(Duration::from_millis(100)).is_err(),
            "the job is held on the worker"
        );
        ok(
            &owner,
            client,
            "cancel",
            "job.cancel",
            json!({"job_id": job}),
        );
        for_job
            .recv_timeout(luxforge_testbase::HANG)
            .expect("leaving the job ends the client's wait for it")
            .unwrap();
        assert!(
            for_room.recv_timeout(Duration::from_millis(100)).is_err(),
            "the cancelled job still holds the worker"
        );
        gate.open();
        for_room
            .recv_timeout(luxforge_testbase::HANG)
            .expect("the worker finishing the job makes room")
            .unwrap();
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
        std::fs::remove_file(photo).unwrap();
    }

    #[test]
    fn a_single_source_flight_is_client_scoped_and_reuses_the_verified_cache() {
        let catalog = temp("source-flight.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let first = owner.register();
        let second = owner.register();
        let a = request(&owner, first, "catalog.import", import_params(fixture()))
            .result
            .unwrap();
        let b = request(&owner, second, "catalog.import", import_params(fixture()))
            .result
            .unwrap();
        let first_id = a["job_id"].as_str().unwrap();
        let second_id = b["job_id"].as_str().unwrap();
        assert_ne!(
            request(&owner, first, "job.cancel", json!({"job_id":first_id}))
                .result
                .unwrap()["status"],
            "cancelled",
            "leaving a job never cancels it while another client wants it"
        );
        assert_eq!(
            request(&owner, first, "job.read", json!({"job_id":first_id}))
                .result
                .unwrap()["job_id"],
            first_id,
            "the client that left still reads the job it requested"
        );
        assert_eq!(
            request(
                &owner,
                owner.register(),
                "job.read",
                json!({"job_id":first_id})
            )
            .error
            .unwrap()
            .code,
            "validation",
            "a client that never requested it does not"
        );
        let ready = wait_source(&owner, second, second_id);
        assert_eq!(ready["status"], "ready");
        let asset = ready["result"]["asset"]["id"].clone();
        let again = request(&owner, second, "catalog.import", import_params(fixture()))
            .result
            .unwrap();
        assert_eq!(
            again["status"], "ready",
            "a matching signature uses cached pixels"
        );
        assert_eq!(
            wait_source(&owner, second, again["job_id"].as_str().unwrap())["result"]["asset"]["id"],
            asset
        );
        assert_eq!(
            request(&owner, second, "catalog.list", json!({}))
                .result
                .unwrap()["assets"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        owner.stop();
        join.join().unwrap();
    }

    #[test]
    fn reopened_sample_prepares_off_owner_and_failed_import_never_creates_an_asset() {
        let catalog = temp("source-reopen.sqlite");
        let source = temp("source-reopen.jpg");
        let invalid = temp("source-invalid.jpg");
        let _ = std::fs::remove_file(&catalog);
        std::fs::copy(fixture(), &source).unwrap();
        std::fs::write(&invalid, b"not a JPEG").unwrap();
        let asset = {
            let mut service = EditorService::open(&catalog).unwrap();
            service.import(&source).unwrap().asset.id
        };
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let missing = request(
            &owner,
            client,
            "render.sample",
            json!({"asset_id":asset,"x":0,"y":0}),
        );
        let error = missing.error.unwrap();
        assert_eq!(error.code, "preparation-required");
        assert_eq!(error.message, "source preparation required");
        let job = error
            .job_id
            .expect("the refusal names the job preparing it");
        assert!(
            request(&owner, client, "session.state", json!({}))
                .error
                .is_none(),
            "the owner answers while decode runs"
        );
        assert_eq!(wait_source(&owner, client, &job)["status"], "ready");
        assert_eq!(
            request(&owner, client, "events.since", json!({"after":0}))
                .result
                .unwrap()["current_sequence"],
            0
        );
        assert!(
            request(
                &owner,
                client,
                "render.sample",
                json!({"asset_id":asset,"x":0,"y":0})
            )
            .error
            .is_none()
        );
        let queued = request(&owner, client, "catalog.import", import_params(&invalid))
            .result
            .unwrap();
        let failed = wait_source(&owner, client, queued["job_id"].as_str().unwrap());
        assert_eq!(failed["status"], "failed");
        assert_eq!(
            request(&owner, client, "catalog.list", json!({}))
                .result
                .unwrap()["assets"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        std::fs::remove_file(&source).unwrap();
        assert_eq!(
            request(
                &owner,
                client,
                "render.sample",
                json!({"asset_id":asset,"x":0,"y":0})
            )
            .error
            .unwrap()
            .code,
            "source-unavailable"
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(invalid).unwrap();
    }

    #[test]
    fn an_owner_started_with_a_registry_serves_exactly_those_providers() {
        let catalog = temp("registry.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(crate::ModuleRegistry::new())).unwrap();
        let client = owner.register();
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: "modules".into(),
                    method: "module.list".into(),
                    params: json!({}),
                    token: None,
                },
            )
            .unwrap();
        assert_eq!(
            response.result.unwrap()["modules"],
            json!([]),
            "the owner serves the registry it was given, not the built-ins"
        );
        let missing = owner
            .call(
                client,
                ApiRequest {
                    id: "pixel".into(),
                    method: "edit.set-pixel".into(),
                    params: json!({}),
                    token: None,
                },
            )
            .unwrap();
        assert_eq!(
            missing.error.expect("no provider, no method").message,
            "unknown method edit.set-pixel"
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn historical_preview_stays_selected_during_another_clients_commit() {
        let catalog = temp("preview-live.sqlite");
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let viewer = owner.register();
        let agent = owner.register();
        assert_ne!(viewer, agent);
        let call = |client: ClientId, id: &str, method: &str, params: Value| {
            let response = owner
                .call(
                    client,
                    ApiRequest {
                        id: id.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .unwrap();
            assert!(response.error.is_none(), "{id}: {:?}", response.error);
            response.result.unwrap()
        };
        let queued = call(viewer, "import", "catalog.import", import_params(fixture()));
        let job_id = queued["job_id"].as_str().unwrap();
        let imported = luxforge_testbase::wait_for("the import job to finish", || {
            let status = call(viewer, "status", "job.read", json!({"job_id":job_id}));
            match status["status"].as_str() {
                Some("ready") => Some(status["result"].clone()),
                Some("queued" | "running") => None,
                other => panic!("unexpected import job {other:?}: {status}"),
            }
        });
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();
        let baseline = call(
            viewer,
            "baseline",
            "render.sample",
            json!({"asset_id":asset,"x":0,"y":0}),
        )["rgba"]
            .clone();
        let edited = call(
            viewer,
            "a",
            "edit.set-pixel",
            json!({"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"a","actor":"viewer"},"x":0,"y":0,"rgb":[1,2,3]}),
        );
        // Selecting the current entry is the live state, not a historical preview.
        let latest = call(
            viewer,
            "latest",
            "preview.select",
            json!({"asset_id":asset,"entry_id":edited["current_entry_id"]}),
        );
        assert_eq!(latest["session"]["preview"]["selections"], json!({}));
        assert!(latest["generation"].is_u64());
        let selected = call(
            viewer,
            "preview",
            "preview.select",
            json!({"asset_id":asset,"entry_id":original}),
        );
        assert_eq!(selected["session"]["revision"], json!(2));
        assert_eq!(
            selected["session"]["preview"]["selections"][asset.as_str().unwrap()],
            json!({"entry_id": original, "geometry_from": null})
        );
        // The other client's session is independent and unaffected.
        assert_eq!(
            call(agent, "agent-session", "session.state", json!({}))["revision"],
            json!(0)
        );
        call(
            agent,
            "b",
            "edit.set-pixel",
            json!({"asset_id":asset,"mutation":{"expected_revision":1,"request_id":"b","actor":"agent"},"x":0,"y":0,"rgb":[9,8,7]}),
        );
        let preview = call(
            viewer,
            "sample-old",
            "render.sample",
            json!({"asset_id":asset,"x":0,"y":0}),
        );
        assert_eq!(preview["rgba"], baseline);
        let returned = call(viewer, "current", "preview.return-current", json!({}));
        assert_eq!(returned["session"]["revision"], json!(3));
        let current = call(
            viewer,
            "sample-current",
            "render.sample",
            json!({"asset_id":asset,"x":0,"y":0}),
        );
        assert_eq!(current["rgba"], json!([9, 8, 7, 255]));
        // Disconnecting forgets the session; a fresh registration starts from default.
        call(
            viewer,
            "zoom",
            "view.set",
            json!({"zoom":{"mode":"percent","value":200.0}}),
        );
        owner.disconnect(viewer);
        let fresh = owner.register();
        assert_eq!(
            call(fresh, "fresh", "session.state", json!({}))["preview"]["view"]["zoom"]["mode"],
            json!("fit")
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A preview job of an open draft renders what committing it would produce, and only for the
    /// client that holds it. The owner resolves the draft from that client's own session.
    #[test]
    fn a_preview_job_renders_the_requesting_clients_draft_and_nobody_elses() {
        let catalog = temp("preview-draft.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let mut registry = crate::ModuleRegistry::builtin();
        registry
            .register(crate::modules::PatchModule::shared())
            .unwrap();
        let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
        let client = owner.register();
        let other = owner.register();
        let call = |client: ClientId, id: &str, method: &str, params: Value| {
            let response = owner
                .call(
                    client,
                    ApiRequest {
                        id: id.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .unwrap();
            assert!(response.error.is_none(), "{id}: {:?}", response.error);
            response.result.unwrap()
        };
        let imported = import_asset(&owner, client, &fixture());
        let asset_value = imported["asset"]["id"].clone();
        let asset = crate::AssetId::parse(asset_value.as_str().unwrap()).unwrap();
        let begun = call(
            client,
            "begin",
            "draft.begin",
            json!({"asset_id": asset_value, "action": crate::modules::PATCH_ACTION}),
        );
        let draft_value = begun["draft_id"].clone();
        let draft = crate::DraftId::parse(draft_value.as_str().unwrap()).unwrap();
        call(
            client,
            "set",
            "draft.set",
            json!({"draft_id": draft_value, "fields": {"red": 200.0}}),
        );

        let job = owner
            .preview_job(PreviewRequest::new(client, asset.clone()).draft(draft.clone()))
            .expect("the drafted preview");
        assert_eq!(job.evaluation.draft_revision(), Some(1));
        assert_eq!(
            job.evaluation.recipe().layers.len(),
            1,
            "the draft's planned layer"
        );
        let rendered = job
            .evaluation
            .source()
            .render(
                job.evaluation.registry(),
                job.evaluation.entry().snapshot.id.clone(),
                job.evaluation.recipe(),
            )
            .expect("a drafted frame");
        assert_eq!(rendered.pixel(0, 0), Some([200, 0, 0, 255]));

        // The stored stack is untouched, and a truncated job still applies to what is rendered.
        let stored = owner
            .preview_job(PreviewRequest::new(client, asset.clone()))
            .expect("the committed preview");
        assert!(stored.evaluation.recipe().layers.is_empty());
        assert_eq!(stored.evaluation.draft_revision(), None);
        assert_ne!(
            stored
                .evaluation
                .source()
                .render(
                    stored.evaluation.registry(),
                    stored.evaluation.entry().snapshot.id.clone(),
                    stored.evaluation.recipe(),
                )
                .unwrap()
                .pixel(0, 0),
            rendered.pixel(0, 0)
        );
        assert_eq!(
            owner
                .preview_job(
                    PreviewRequest::new(client, asset.clone())
                        .draft(draft.clone())
                        .layers(0)
                )
                .expect("the draft layer's input stage")
                .layer_count,
            Some(0)
        );
        let too_many = owner
            .preview_job(
                PreviewRequest::new(client, asset.clone())
                    .draft(draft.clone())
                    .layers(2),
            )
            .expect_err("the drafted stack has one layer");
        assert_eq!(too_many.kind, ErrorKind::Validation);

        // Another client cannot preview this gesture: a draft belongs to one session.
        let foreign = owner
            .preview_job(PreviewRequest::new(other, asset.clone()).draft(draft.clone()))
            .expect_err("a draft is not shared");
        assert_eq!(foreign.kind, ErrorKind::Validation);
        assert!(foreign.detail.contains("unknown draft"), "{foreign}");

        // Cancelling ends it, so the same request is refused for its own client too.
        call(
            client,
            "cancel",
            "draft.cancel",
            json!({"draft_id": draft_value}),
        );
        assert_eq!(
            owner
                .preview_job(PreviewRequest::new(client, asset).draft(draft))
                .expect_err("the draft has ended")
                .kind,
            ErrorKind::Validation
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// Two independent JSON clients ask for the exact histogram of the same evaluated image, share
    /// one job, and keep correct independent results while edits continue: the frozen historical
    /// result stays attached to its entry and is never relabelled current.
    #[test]
    fn two_clients_share_one_job_and_keep_independent_current_and_historical_results() {
        let catalog = temp("analysis-share.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let viewer = owner.register();
        let agent = owner.register();
        let imported = import_asset(&owner, viewer, &fixture());
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();
        let asset_id = crate::AssetId::parse(asset.as_str().unwrap()).unwrap();

        // Both clients ask for the current composition. The identities are equal, so this is one
        // job and one render, not two.
        let first = ok(
            &owner,
            viewer,
            "request",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        );
        let second = ok(
            &owner,
            agent,
            "request",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        );
        assert_eq!(
            first["job_id"], second["job_id"],
            "identical identities share one job"
        );
        assert_eq!(first["identity"], second["identity"]);
        let identity = first["identity"].clone();
        assert_eq!(identity["asset_id"], asset);
        assert_eq!(identity["entry_id"], original);
        assert_eq!(identity["domain"], json!("srgb-8bit-output"));
        assert_eq!(identity["width"], json!(480));
        assert_eq!(identity["height"], json!(320));
        assert_eq!(
            identity["recipe_hash"].as_str().unwrap().len(),
            64,
            "SHA-256 as hex"
        );
        assert!(identity.get("draft").is_none(), "no draft is involved");

        let settled_viewer = settled(&owner, viewer, &first["job_id"]);
        assert_eq!(settled_viewer["status"], json!("ready"));
        assert_eq!(settled_viewer["identity"], identity);
        let reference = expected_report(&owner, PreviewRequest::new(viewer, asset_id.clone()));
        assert_eq!(
            settled_viewer["result"], reference,
            "the counts are the exact reduction of that entry's rendered frame"
        );
        assert!(settled_viewer.get("error").is_none());
        // The other client reads the very same shared result.
        assert_eq!(
            settled(&owner, agent, &second["job_id"])["result"],
            reference
        );

        // The agent commits while the viewer holds a report of the original entry.
        pixel_edit(&owner, agent, &asset, 0, "edit", [255, 255, 255]);

        // Asking for that historical entry answers from its own immutable stack: same identity,
        // same counts, still named by the original entry rather than the new current one.
        let historical = ok(
            &owner,
            viewer,
            "historical",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "entry", "entry_id": original}}),
        );
        assert_eq!(
            historical["status"],
            json!("ready"),
            "the stored report answers without a second render"
        );
        assert_eq!(historical["identity"], identity);
        assert_eq!(historical["result"], reference);
        assert_eq!(historical["identity"]["entry_id"], original);

        // The current composition is now different work with a different identity and counts.
        let current = ok(
            &owner,
            viewer,
            "current",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        );
        assert_ne!(current["job_id"], first["job_id"]);
        assert_ne!(current["identity"]["entry_id"], original);
        assert_ne!(current["identity"]["recipe_hash"], identity["recipe_hash"]);
        let settled_current = settled(&owner, viewer, &current["job_id"]);
        assert_eq!(settled_current["status"], json!("ready"));
        assert_ne!(
            settled_current["result"], reference,
            "one replaced pixel moves the counts"
        );
        assert_eq!(
            settled_current["result"],
            expected_report(&owner, PreviewRequest::new(viewer, asset_id))
        );
        // The earlier result is untouched by the commit.
        assert_eq!(
            settled(&owner, viewer, &first["job_id"])["result"],
            reference
        );

        // A job this client never requested is not its own, and neither is one that does not exist.
        let foreign = owner.register();
        for (id, method) in [("foreign", "job.read"), ("fc", "job.cancel")] {
            assert_eq!(
                failure(
                    &owner,
                    foreign,
                    id,
                    method,
                    json!({"job_id": first["job_id"]})
                )
                .code,
                "validation"
            );
        }
        assert_eq!(
            failure(
                &owner,
                viewer,
                "missing",
                "job.read",
                json!({"job_id": crate::JobId::new()}),
            )
            .code,
            "validation"
        );
        assert_eq!(
            failure(
                &owner,
                viewer,
                "malformed",
                "job.read",
                json!({"job_id": "not-a-job"}),
            )
            .code,
            "validation"
        );
        assert_eq!(
            failure(
                &owner,
                viewer,
                "unknown-asset",
                "analysis.request",
                json!({"asset_id": crate::AssetId::new(), "target": {"kind": "current"}}),
            )
            .code,
            "validation"
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A caller-owned draft is analysed at the revision it holds now, its effective recipe is never
    /// persisted, and no other client can name it.
    #[test]
    fn a_draft_target_analyses_the_drafted_recipe_and_belongs_to_one_session() {
        let catalog = temp("analysis-draft.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let client = owner.register();
        let other = owner.register();
        let imported = import_asset(&owner, client, &fixture());
        let asset = imported["asset"]["id"].clone();
        let asset_id = crate::AssetId::parse(asset.as_str().unwrap()).unwrap();

        let current = ok(
            &owner,
            client,
            "current",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        );
        let baseline = settled(&owner, client, &current["job_id"])["result"].clone();

        let begun = ok(
            &owner,
            client,
            "begin",
            "draft.begin",
            json!({"asset_id": asset, "action": "set-pixel"}),
        );
        let draft_id = begun["draft_id"].clone();
        let draft = ok(
            &owner,
            client,
            "set",
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"x": 0, "y": 0, "rgb": [255, 255, 255]}}),
        );
        assert_eq!(draft["draft_revision"], json!(1));

        let drafted = ok(
            &owner,
            client,
            "drafted",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "draft", "draft_id": draft_id}}),
        );
        assert_eq!(drafted["identity"]["draft"]["draft_id"], draft_id);
        assert_eq!(drafted["identity"]["draft"]["draft_revision"], json!(1));
        let settled_draft = settled(&owner, client, &drafted["job_id"]);
        assert_eq!(settled_draft["status"], json!("ready"));
        assert_ne!(
            settled_draft["result"], baseline,
            "the drafted white pixel moves the counts"
        );
        assert_eq!(
            settled_draft["result"],
            expected_report(
                &owner,
                PreviewRequest::new(client, asset_id)
                    .draft(crate::DraftId::parse(draft_id.as_str().unwrap()).unwrap()),
            )
        );
        // Exactly one pixel changed: the red channel's population moves by one at two codes.
        let moved: u64 = settled_draft["result"]["r"]
            .as_array()
            .unwrap()
            .iter()
            .zip(baseline["r"].as_array().unwrap())
            .map(|(after, before)| after.as_u64().unwrap().abs_diff(before.as_u64().unwrap()))
            .sum();
        assert_eq!(moved, 2, "one pixel left one bin and joined another");

        // A draft belongs to one session.
        assert_eq!(
            failure(
                &owner,
                other,
                "foreign-draft",
                "analysis.request",
                json!({"asset_id": asset, "target": {"kind": "draft", "draft_id": draft_id}}),
            )
            .code,
            "validation"
        );
        // Cancelling the draft ends it; the same request is then refused for its own client too.
        ok(
            &owner,
            client,
            "cancel-draft",
            "draft.cancel",
            json!({"draft_id": draft_id}),
        );
        assert_eq!(
            failure(
                &owner,
                client,
                "ended-draft",
                "analysis.request",
                json!({"asset_id": asset, "target": {"kind": "draft", "draft_id": draft_id}}),
            )
            .code,
            "validation"
        );
        // Nothing was persisted: the current composition is still the baseline.
        assert_eq!(
            settled(&owner, client, &current["job_id"])["result"],
            baseline
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// One active job plus one replaceable pending job, globally: a third request displaces the
    /// pending one, which reads `superseded` and carries no counts. Cancel and disconnect release
    /// only the withdrawing client's interest.
    ///
    /// Every entry here carries a held colour layer and the gate is shut for the whole first half,
    /// so the job the worker picked up cannot finish while the test fills, displaces and empties
    /// the one pending slot: what each request does to that slot is the queue's rule, not a race
    /// with the renderer. The gate opens for the second half, where the work actually completes.
    #[test]
    fn racing_requests_supersede_the_pending_job_and_withdrawal_releases_only_its_own_interest() {
        let catalog = temp("analysis-race.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let mut registry = ModuleRegistry::developer();
        registry
            .register(crate::modules::HeldModule::shared(gate.clone()))
            .expect("a valid holding module");
        let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
        let viewer = owner.register();
        let agent = owner.register();
        let partner = owner.register();
        let imported = import_asset(&owner, viewer, &fixture());
        let asset = imported["asset"]["id"].clone();
        // The held layer is committed first, so every entry after it carries one.
        ok(
            &owner,
            viewer,
            "hold",
            &format!("edit.{}", crate::modules::HELD_ACTION),
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 0, "request_id": "hold", "actor": "test"},
            }),
        );
        let mut entries = Vec::new();
        for (index, channel) in [10u8, 20, 30, 40, 50, 60, 70].into_iter().enumerate() {
            let edited = pixel_edit(
                &owner,
                viewer,
                &asset,
                index as u64 + 1,
                &format!("edit-{channel}"),
                [channel, channel, channel],
            );
            entries.push(edited["current_entry_id"].clone());
        }
        let entry_request = |client: ClientId, id: &str, entry: &Value| {
            ok(
                &owner,
                client,
                id,
                "analysis.request",
                json!({"asset_id": asset, "target": {"kind": "entry", "entry_id": entry}}),
            )
        };
        let read = |client: ClientId, id: &str, job: &Value| {
            ok(&owner, client, id, "job.read", json!({"job_id": job}))
        };

        // From here until the gate opens, whichever job the worker started stays on it.
        gate.shut();

        // Three requests: the first takes the worker, the second the one pending slot and the
        // third displaces the second out of it.
        let active = entry_request(viewer, "a", &entries[0]);
        assert_eq!(active["status"], json!("running"), "the worker holds it");
        let displaced = entry_request(viewer, "b", &entries[1]);
        assert_eq!(
            displaced["status"],
            json!("queued"),
            "waits in the one slot"
        );
        let winner = entry_request(viewer, "c", &entries[2]);
        assert_eq!(winner["status"], json!("queued"), "took the slot from b");
        let superseded = read(viewer, "read-b", &displaced["job_id"]);
        assert_eq!(
            superseded["status"],
            json!("superseded"),
            "the single pending slot was taken by the newer request"
        );
        assert!(
            superseded.get("result").is_none(),
            "a superseded job carries no counts"
        );
        assert!(superseded.get("error").is_none());

        // Re-requesting a superseded identity is allowed and gets fresh work, which takes the slot
        // from the request that displaced it: the newest request always holds it.
        let again = entry_request(viewer, "b-again", &entries[1]);
        assert_ne!(again["job_id"], displaced["job_id"]);
        assert_eq!(again["status"], json!("queued"), "took the slot from c");
        assert_eq!(
            read(viewer, "read-c", &winner["job_id"])["status"],
            json!("superseded"),
            "the third request lost the slot to the fourth"
        );

        // The last interest withdrawing drops a job that had not started yet: it reads `cancelled`
        // and carries no counts, and the requester may still read the outcome it asked for.
        assert_eq!(
            ok(
                &owner,
                viewer,
                "cancel-queued",
                "job.cancel",
                json!({"job_id": again["job_id"]}),
            )["status"],
            json!("cancelled"),
            "the last interest leaving drops the job from the slot at once"
        );
        let withdrawn = read(viewer, "read-queued", &again["job_id"]);
        assert_eq!(withdrawn["status"], json!("cancelled"), "{withdrawn}");
        assert!(
            withdrawn.get("result").is_none(),
            "a cancelled job carries no counts"
        );

        // A disconnect releases every interest that client held, exactly as a cancel does: the
        // identity becomes re-requestable and the gone client owns no job.
        let before = entry_request(agent, "before", &entries[6]);
        assert_eq!(before["status"], json!("queued"), "the slot was free again");
        owner.disconnect(agent);
        assert_eq!(
            failure(
                &owner,
                agent,
                "gone",
                "job.read",
                json!({"job_id": before["job_id"]}),
            )
            .code,
            "validation",
            "a disconnected client owns no job"
        );

        // Two clients on one identity share one job. Cancelling one interest leaves the work for
        // the other, so the job stays where it is instead of being dropped from the slot.
        let shared_viewer = entry_request(viewer, "shared-v", &entries[3]);
        let shared_partner = entry_request(partner, "shared-p", &entries[3]);
        assert_eq!(shared_viewer["job_id"], shared_partner["job_id"]);
        assert_eq!(
            ok(
                &owner,
                viewer,
                "cancel",
                "job.cancel",
                json!({"job_id": shared_viewer["job_id"]}),
            )["status"],
            json!("queued"),
            "the canceller reads the shared job, which the other client keeps"
        );
        assert_eq!(
            read(partner, "read-shared-held", &shared_partner["job_id"])["status"],
            json!("queued"),
            "the other client still wants it, still waiting for the worker"
        );

        // The gate opens: the held job finishes, the queue starts the one in the slot, and the
        // requests below are made one at a time, so nothing displaces anything from here on.
        gate.open();
        let finished = settled(&owner, viewer, &active["job_id"]);
        assert_eq!(finished["status"], json!("ready"), "{finished}");
        assert!(finished["result"].is_object());
        let kept = settled(&owner, partner, &shared_partner["job_id"]);
        assert_eq!(kept["status"], json!("ready"), "{kept}");
        assert!(kept["result"].is_object());
        assert_eq!(
            read(viewer, "read-shared", &shared_viewer["job_id"])["result"],
            kept["result"],
            "one client's cancel did not invalidate the shared result"
        );

        // Every identity the first half superseded or cancelled is re-requestable and runs.
        for (id, entry) in [("retry-b", 1), ("retry-c", 2)] {
            let requested = entry_request(viewer, id, &entries[entry]);
            let ready = settled(&owner, viewer, &requested["job_id"]);
            assert_eq!(ready["status"], json!("ready"), "{ready}");
            assert!(ready["result"].is_object());
        }
        let reconnected = owner.register();
        let after = entry_request(reconnected, "after", &entries[6]);
        assert_ne!(
            after["job_id"], before["job_id"],
            "the released identity is re-requestable"
        );
        let ready = settled(&owner, reconnected, &after["job_id"]);
        assert_eq!(ready["status"], json!("ready"), "{ready}");
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A stack whose provider is missing cannot be evaluated at all: the job reads `failed` with the
    /// structured error and never a report, and its identity carries no output stage, so nothing can
    /// be mistaken for a valid but empty histogram. The stored layer is kept, not rewritten.
    #[test]
    fn a_stack_with_an_unavailable_provider_reads_failed_with_its_error_and_no_counts() {
        let catalog = temp("analysis-unavailable.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let asset;
        {
            let (owner, join) =
                OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
            let client = owner.register();
            let imported = import_asset(&owner, client, &fixture());
            asset = imported["asset"]["id"].clone();
            pixel_edit(&owner, client, &asset, 0, "edit", [1, 2, 3]);
            owner.stop();
            join.join().unwrap();
        }
        // The same catalog reopened with the pixel provider registered as unavailable.
        let mut registry = ModuleRegistry::new();
        registry
            .register(crate::modules::TestModule::shared(
                "luxforge.pixel",
                crate::PIXEL_EFFECT,
                "set-pixel",
                crate::Availability::Unavailable {
                    reason: "test: the pixel provider is not installed".into(),
                },
            ))
            .unwrap();
        let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
        let client = owner.register();
        let requested = ok(
            &owner,
            client,
            "request",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        );
        assert_eq!(requested["status"], json!("failed"));
        assert!(requested.get("result").is_none());
        assert_eq!(requested["identity"]["width"], json!(0));
        assert_eq!(requested["identity"]["height"], json!(0));
        let read = ok(
            &owner,
            client,
            "read",
            "job.read",
            json!({"job_id": requested["job_id"]}),
        );
        assert_eq!(read["status"], json!("failed"));
        assert!(
            read.get("result").is_none(),
            "a failed job carries no counts"
        );
        assert_eq!(read["error"]["code"], json!("incompatible"));
        assert!(
            read["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unavailable effect"),
            "{}",
            read["error"]["message"]
        );
        // The layer is still there: nothing was discarded to make the stack renderable.
        assert_eq!(
            ok(
                &owner,
                client,
                "describe",
                "recipe.describe",
                json!({"asset_id": asset}),
            )["layers"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// The desktop's own preview evaluation is reused: an analysing preview job returns the exact
    /// reduction of the frame it rendered, submitting it makes the matching API request a cache hit,
    /// and no path decodes the original a second time.
    #[test]
    fn a_submitted_preview_report_answers_the_matching_request_without_a_second_render() {
        let catalog = temp("analysis-preview.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let desktop = owner.register();
        let agent = owner.register();
        let imported = import_asset(&owner, desktop, &fixture());
        let asset = imported["asset"]["id"].clone();
        let asset_id = crate::AssetId::parse(asset.as_str().unwrap()).unwrap();

        // A truncated job renders a layer prefix its identity does not describe, so it is refused.
        assert_eq!(
            owner
                .preview_job(
                    PreviewRequest::new(desktop, asset_id.clone())
                        .layers(0)
                        .analyse()
                )
                .expect_err("a truncated analysing preview")
                .kind,
            ErrorKind::Validation
        );

        let job = owner
            .preview_job(PreviewRequest::new(desktop, asset_id.clone()).analyse())
            .expect("an analysing preview job");
        assert!(job.analyse);
        let PreviewSource::Jpeg(jpeg) = job.evaluation.source() else {
            panic!("a JPEG preview source")
        };
        let source = jpeg.rgba.clone();
        let mut queue = crate::PreviewQueue::default();
        queue.request(job);
        let result = luxforge_testbase::wait_for("a preview", || queue.poll());
        let exact = result.exact().expect("the exact phase");
        let raster = exact.result.as_ref().expect("a frame");
        let report = exact.report.clone().expect("the job asked for a report");
        assert_eq!(
            report,
            crate::analysis::reduce(
                &raster.rgba,
                raster.width,
                raster.height,
                &crate::Cancel::never()
            )
            .unwrap(),
            "the report is the exact reduction of the frame that was rendered"
        );
        let encoded = serde_json::to_value(&report).unwrap();
        owner.submit_analysis(result.identity.clone(), report);

        // The API request for that identity is now answered from the store, ready, without work.
        let requested = ok(
            &owner,
            agent,
            "request",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        );
        assert_eq!(
            requested["status"],
            json!("ready"),
            "the submitted report is a cache hit"
        );
        assert_eq!(
            requested["identity"],
            serde_json::to_value(&result.identity).unwrap()
        );
        assert_eq!(requested["result"], encoded);

        // Nothing re-read or re-decoded the original: every path served the one cached decode.
        let after = owner
            .preview_job(PreviewRequest::new(desktop, asset_id))
            .expect("another preview job");
        assert!(
            matches!(after.evaluation.source(), PreviewSource::Jpeg(jpeg) if Arc::ptr_eq(&source, &jpeg.rgba)),
            "the verified source cache served every request; no duplicate decode"
        );
        assert!(!after.analyse, "a plain preview asks for no reduction");
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A session framed by `preview.select {keep_geometry}` frames every whole preview of its
    /// selection, so the desktop's first frame, a zoom and a refresh all show the same crop; a
    /// truncated job, and a job of another client, are never framed.
    #[test]
    fn a_framed_selection_frames_the_preview_jobs_of_that_selection() {
        let catalog = temp("framed-selection.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let desktop = owner.register();
        let agent = owner.register();
        let imported = import_asset(&owner, desktop, &fixture());
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();
        let asset_id = crate::AssetId::parse(asset.as_str().unwrap()).unwrap();
        let original_id: EntryId = serde_json::from_value(original.clone()).unwrap();
        ok(
            &owner,
            desktop,
            "crop",
            "edit.crop",
            json!({"asset_id": asset, "mutation": {"expected_revision": 0, "request_id": "crop", "actor": "test"}, "x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5}),
        );
        ok(
            &owner,
            desktop,
            "compare",
            "preview.select",
            json!({"asset_id": asset, "entry_id": original, "keep_geometry": true}),
        );
        let size = |request: PreviewRequest| {
            let job = owner.preview_job(request).expect("a preview job");
            assert_eq!(job.evaluation.entry().id, original_id);
            (job.identity.width, job.identity.height)
        };
        let entry =
            |client| PreviewRequest::new(client, asset_id.clone()).entry(Some(original_id.clone()));
        assert_eq!(size(entry(desktop)), (240, 160), "framed by the crop");
        assert_eq!(
            size(entry(desktop).analyse()),
            (240, 160),
            "an analysing job is framed alike"
        );
        assert_eq!(
            size(entry(agent)),
            (480, 320),
            "another client's session frames nothing"
        );
        let prefix = owner
            .preview_job(entry(desktop).layers(0))
            .expect("a truncated job");
        assert_eq!(
            prefix.evaluation.recipe().layers.len(),
            0,
            "a truncated job renders the entry's own prefix"
        );
        ok(
            &owner,
            desktop,
            "whole",
            "preview.select",
            json!({"asset_id": asset, "entry_id": original}),
        );
        assert_eq!(size(entry(desktop)), (480, 320), "the whole Original");
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// Read `activity.list` until `wanted` holds. The work under test is held at a gate, so what
    /// this waits for is a worker reaching that gate, never a race with how fast it works.
    fn listed(owner: &OwnerHandle, client: ClientId, wanted: impl Fn(&Value) -> bool) -> Value {
        luxforge_testbase::wait_for("activity.list to show the work", || {
            Some(ok(owner, client, "list", "activity.list", json!({}))).filter(|list| wanted(list))
        })
    }

    fn active_kind(list: &Value, kind: &str) -> bool {
        list["active"]
            .as_array()
            .is_some_and(|active| active.iter().any(|entry| entry["kind"] == json!(kind)))
    }

    /// Every worker of one owner publishes to one board, and `activity.list` answers from it. A
    /// source preparation and an analysis job are listed while they run, each held at a gate the
    /// test controls, and as recent work once they end; a preview job from a queue given the same
    /// board, as the desktop's is, is listed beside them. The board's recent threshold is zero, so
    /// this small fixture's short work is kept. The method needs no asset, refuses parameters and
    /// emits no event, and discovery lists it.
    #[test]
    fn workers_publish_to_the_owners_board_and_activity_list_answers_from_it() {
        let catalog = temp("activity.sqlite");
        let photo = temp("activity-photo.jpg");
        let _ = std::fs::remove_file(&catalog);
        std::fs::copy(fixture(), &photo).unwrap();
        let source_gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let render_gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(crate::modules::HeldModule::shared(render_gate.clone()))
            .expect("a valid holding module");
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let hold = source_gate.clone();
        let (owner, join) = OwnerHandle::start_observed(
            &catalog,
            Arc::new(registry),
            board.clone(),
            Some(Arc::new(move || hold.pass())),
        )
        .unwrap();
        assert!(
            Arc::ptr_eq(&owner.activity(), &board),
            "the handle shares the owner's board"
        );
        let client = owner.register();

        // Nothing has run, and no asset is needed to ask.
        assert_eq!(
            ok(&owner, client, "idle", "activity.list", json!({})),
            json!({"sequence": 0, "active": [], "recent": [], "untracked": 0})
        );
        assert!(
            send(&owner, client, "omitted", "activity.list", Value::Null)
                .error
                .is_none(),
            "omitted parameters are no parameters"
        );
        assert_eq!(
            failure(
                &owner,
                client,
                "filtered",
                "activity.list",
                json!({"kind": "source.prepare"}),
            )
            .code,
            "validation",
            "a parameter is refused, not ignored"
        );

        // A source preparation, held after its activity began.
        source_gate.shut();
        let queued = ok(
            &owner,
            client,
            "import",
            "catalog.import",
            import_params(&photo),
        );
        let job_id = queued["job_id"].clone();
        let running = listed(&owner, client, |list| active_kind(list, "source.prepare"));
        let entry = &running["active"][0];
        assert_eq!(entry["label"], json!("Preparing original"));
        assert_eq!(
            entry["detail"],
            json!(photo.file_name().unwrap().to_str().unwrap()),
            "the detail is the file name"
        );
        assert_eq!(entry["job_id"], job_id);
        assert!(
            entry.get("asset_id").is_none(),
            "a new import has no asset yet"
        );
        assert!(entry["elapsed_ms"].is_u64());
        // The worker begins the activity and then tells the owner it started, so the job reads as
        // running a moment after it is listed; it is held there, and still listed, until the
        // gate opens.
        luxforge_testbase::wait_until("the held job to read as running", || {
            request(&owner, client, "job.read", json!({"job_id": job_id}))
                .result
                .unwrap()["status"]
                == json!("running")
        });
        assert!(active_kind(
            &ok(&owner, client, "held", "activity.list", json!({})),
            "source.prepare"
        ));
        source_gate.open();
        assert_eq!(
            wait_source(&owner, client, job_id.as_str().unwrap())["status"],
            json!("ready")
        );
        // The entry ended before the owner learned the result, so it is recent already.
        let prepared = ok(&owner, client, "prepared", "activity.list", json!({}));
        assert_eq!(prepared["active"], json!([]));
        assert_eq!(prepared["recent"][0]["kind"], json!("source.prepare"));
        assert_eq!(prepared["recent"][0]["outcome"], json!("completed"));
        assert_eq!(prepared["recent"][0]["job_id"], job_id);
        let asset = ok(
            &owner,
            client,
            "adopt",
            "job.adopt",
            json!({"job_id": job_id}),
        )["asset"]["asset"]["id"]
            .clone();

        // An analysis job, held on its worker by a committed colour layer.
        ok(
            &owner,
            client,
            "hold",
            &format!("edit.{}", crate::modules::HELD_ACTION),
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 0, "request_id": "hold", "actor": "test"},
            }),
        );
        render_gate.shut();
        let requested = ok(
            &owner,
            client,
            "request",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        );
        let running = listed(&owner, client, |list| {
            active_kind(list, "analysis.histogram")
        });
        let entry = &running["active"][0];
        assert_eq!(entry["label"], json!("Measuring histogram"));
        assert_eq!(entry["job_id"], requested["job_id"]);
        assert_eq!(entry["asset_id"], asset);
        render_gate.open();
        assert_eq!(
            settled(&owner, client, &requested["job_id"])["status"],
            json!("ready")
        );
        let measured = ok(&owner, client, "measured", "activity.list", json!({}));
        assert_eq!(measured["active"], json!([]));
        assert_eq!(measured["recent"][0]["kind"], json!("analysis.histogram"));
        assert_eq!(measured["recent"][0]["outcome"], json!("completed"));
        assert_eq!(measured["recent"][0]["job_id"], requested["job_id"]);

        // A preview job from a queue given the owner's board, as the desktop's is.
        let asset_id = AssetId::parse(asset.as_str().unwrap()).unwrap();
        let job = owner
            .preview_job(PreviewRequest::new(client, asset_id).proxy(ProxyBounds {
                width: 64,
                height: 64,
            }))
            .expect("a preview job");
        let mut queue = crate::PreviewQueue::default();
        queue.set_activity(owner.activity());
        queue.request(job);
        luxforge_testbase::wait_until("the preview queue to go idle", || {
            let _ = queue.poll();
            !queue.is_busy()
        });

        let events = ok(
            &owner,
            client,
            "events",
            "events.since",
            json!({"after": 0}),
        );
        let captured = ok(&owner, client, "captured", "activity.list", json!({}));
        // The answer a client reads, printed for the record under `--nocapture`.
        println!("activity.list: {captured}");
        let kinds: Vec<&str> = captured["recent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            ["preview.render", "analysis.histogram", "source.prepare"],
            "newest first"
        );
        assert_eq!(captured["active"], json!([]));
        assert_eq!(captured["recent"][0]["phase"], json!("exact"));
        assert_eq!(captured["recent"][0]["asset_id"], asset);
        assert_eq!(
            ok(
                &owner,
                client,
                "events-after",
                "events.since",
                json!({"after": 0})
            ),
            events,
            "reading the board emitted no event"
        );

        let schema = ok(&owner, client, "schema", "schema.list", json!({}));
        let method = &schema["methods"]["activity.list"];
        assert_eq!(method["mutates"], json!(false));
        assert_eq!(method["required"], json!([]));
        assert_eq!(method["optional"], json!({}));
        let notes = method["notes"].as_str().unwrap();
        assert!(notes.contains("250 ms"), "{notes}");
        assert!(notes.contains("needs no asset"), "{notes}");
        assert!(notes.contains("emits no event"), "{notes}");
        assert!(notes.contains("job.read"), "{notes}");
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
        std::fs::remove_file(photo).unwrap();
    }

    /// The board entry that names this job, running or recent, if the board holds one.
    fn entry_of(list: &Value, job_id: &Value) -> Option<(Value, bool)> {
        for (key, running) in [("active", true), ("recent", false)] {
            if let Some(entry) = list[key]
                .as_array()
                .and_then(|entries| entries.iter().find(|entry| entry["job_id"] == *job_id))
            {
                return Some((entry.clone(), running));
            }
        }
        None
    }

    /// What `job.read` and `activity.list` say about one job, read back to back, and whether they
    /// agree: a running job is listed as active, and a finished one as recent with the outcome its
    /// status stands for. Answers the job as `job.read` answered it.
    fn agreeing(owner: &OwnerHandle, client: ClientId, job_id: &Value, kind: &str) -> Value {
        let job = ok(owner, client, "read", "job.read", json!({"job_id": job_id}));
        let list = ok(owner, client, "list", "activity.list", json!({}));
        let (entry, running) =
            entry_of(&list, job_id).unwrap_or_else(|| panic!("{kind} {job_id} is not listed"));
        assert_eq!(entry["kind"], json!(kind), "{entry}");
        let status = job["status"].as_str().unwrap();
        match status {
            "running" => assert!(running, "{kind} reads running but is listed {entry}"),
            finished => {
                assert!(
                    !running,
                    "{kind} reads {finished} but is still listed active"
                );
                let outcome = match finished {
                    "ready" => "completed",
                    "cancelled" => "cancelled",
                    _ => "failed",
                };
                assert_eq!(entry["outcome"], json!(outcome), "{kind} reads {finished}");
            }
        }
        job
    }

    /// `job.read` and `activity.list` agree about a job of every worker, while it runs and once it
    /// ended: a source preparation and an export that complete, and an analysis whose one client
    /// cancelled it while it ran. Each is held on its own worker at a gate the test controls; the
    /// board keeps work of any length as recent. A capability job publishes through the same lane
    /// path as an export, which `jobs::tests::a_capability_jobs_progress_and_activity_are_on_the_board`
    /// proves at the table.
    #[test]
    fn activity_list_and_job_read_agree_about_a_job_of_every_kind() {
        let catalog = temp("agree.sqlite");
        let photo = temp("agree-photo.jpg");
        let exports = temp("agree-exports");
        let _ = std::fs::remove_file(&catalog);
        std::fs::copy(fixture(), &photo).unwrap();
        std::fs::create_dir_all(&exports).unwrap();
        let source_gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let render_gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let export_gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(crate::modules::HeldModule::shared(render_gate.clone()))
            .expect("a valid holding module");
        let hold = source_gate.clone();
        let (owner, join) = OwnerHandle::start_observed(
            &catalog,
            Arc::new(registry),
            ActivityBoard::with_recent_threshold(Duration::ZERO),
            Some(Arc::new(move || hold.pass())),
        )
        .unwrap();
        let client = owner.register();
        let until_listed = |job_id: &Value| {
            listed(&owner, client, |list| {
                entry_of(list, job_id).is_some_and(|(_, running)| running)
            });
        };
        let until_running = |job_id: &Value| {
            luxforge_testbase::wait_until("the job to run", || {
                ok(
                    &owner,
                    client,
                    "poll",
                    "job.read",
                    json!({"job_id": job_id}),
                )["status"]
                    == json!("running")
            });
        };

        // Source: listed as its worker begins it, and running once the owner hears so.
        source_gate.shut();
        let prepare = ok(
            &owner,
            client,
            "import",
            "catalog.import",
            import_params(&photo),
        )["job_id"]
            .clone();
        until_listed(&prepare);
        until_running(&prepare);
        assert_eq!(
            agreeing(&owner, client, &prepare, "source.prepare")["kind"],
            "prepare"
        );
        source_gate.open();
        wait_source(&owner, client, prepare.as_str().unwrap());
        let prepared = agreeing(&owner, client, &prepare, "source.prepare");
        assert_eq!(prepared["status"], "ready");
        let asset = prepared["result"]["asset"]["id"].clone();
        ok(
            &owner,
            client,
            "adopt",
            "job.adopt",
            json!({"job_id": prepare}),
        );

        // Analysis: running on its worker, then cancelled by its only client while it runs. It
        // reads running until the worker stops, and cancelled with the board once it has.
        ok(
            &owner,
            client,
            "hold",
            &format!("edit.{}", crate::modules::HELD_ACTION),
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 0, "request_id": "hold", "actor": "test"},
            }),
        );
        render_gate.shut();
        let analysis = ok(
            &owner,
            client,
            "request",
            "analysis.request",
            json!({"asset_id": asset, "target": {"kind": "current"}}),
        )["job_id"]
            .clone();
        until_listed(&analysis);
        assert_eq!(
            agreeing(&owner, client, &analysis, "analysis.histogram")["status"],
            "running"
        );
        let left = ok(
            &owner,
            client,
            "cancel",
            "job.cancel",
            json!({"job_id": analysis}),
        );
        assert_eq!(left["status"], "running", "stopping within a chunk");
        agreeing(&owner, client, &analysis, "analysis.histogram");
        render_gate.open();
        assert_eq!(settled(&owner, client, &analysis)["status"], "cancelled");
        assert_eq!(
            agreeing(&owner, client, &analysis, "analysis.histogram")["status"],
            "cancelled"
        );

        // Export: held as it begins rendering, on the job table's export lane.
        export_gate.shut();
        let held = export_gate.clone();
        owner.hold_exports(Some(Arc::new(move |_| held.pass())));
        let export = ok(
            &owner,
            client,
            "export",
            "export.jpeg",
            json!({
                "asset_id": asset,
                "destination": exports.join("agree.jpg"),
                "mutation": envelope(),
            }),
        )["job_id"]
            .clone();
        until_listed(&export);
        let running = agreeing(&owner, client, &export, "export");
        assert_eq!(
            (running["kind"].clone(), running["status"].clone()),
            (json!("export"), json!("running"))
        );
        export_gate.open();
        owner.hold_exports(None);
        luxforge_testbase::wait_until("the export to end", || {
            ok(
                &owner,
                client,
                "poll",
                "job.read",
                json!({"job_id": export}),
            )["status"]
                != json!("running")
        });
        assert_eq!(
            agreeing(&owner, client, &export, "export")["status"],
            "ready"
        );

        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
        std::fs::remove_file(photo).unwrap();
        std::fs::remove_dir_all(exports).unwrap();
    }

    /// Block in [`OwnerHandle::wait_source`] on a thread of its own, as the desktop's refresh and
    /// import tasks do; the receiver yields its answer, so a test fails on a deadline rather than
    /// hanging when nothing releases the wait.
    fn waiting(
        owner: &OwnerHandle,
        client: ClientId,
        job: Option<JobId>,
    ) -> Receiver<Result<(), Error>> {
        let (sender, receiver) = sync_channel(1);
        let owner = owner.clone();
        thread::spawn(move || {
            let _ = sender.send(owner.wait_source(client, job.as_ref()));
        });
        receiver
    }

    /// A source task that panics fails `internal` like any other failed task: `job.read` reads
    /// the job failed, the activity board records it failed, every client waiting on it is
    /// released, and the worker runs the next source job. The panic comes from the worker's test
    /// hold, inside the task, after its activity began and before its work.
    #[test]
    fn a_panicking_source_task_fails_internal_releases_its_waiters_and_the_worker_runs_on() {
        let catalog = temp("source-panic.sqlite");
        let photo = temp("source-panic.jpg");
        let _ = std::fs::remove_file(&catalog);
        std::fs::copy(fixture(), &photo).unwrap();
        let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let armed = Arc::new(AtomicBool::new(true));
        let (hold, fault) = (gate.clone(), armed.clone());
        let (owner, join) = OwnerHandle::start_observed(
            &catalog,
            Arc::new(ModuleRegistry::builtin()),
            board,
            Some(Arc::new(move || {
                hold.pass();
                if fault.swap(false, Ordering::SeqCst) {
                    panic!("a source task panicked");
                }
            })),
        )
        .unwrap();
        let client = owner.register();
        let other = owner.register();

        // The task is held inside its work while two clients wait: one on the job, one on any.
        gate.shut();
        let queued = ok(
            &owner,
            client,
            "import",
            "catalog.import",
            import_params(&photo),
        );
        let job_id = queued["job_id"].clone();
        let job = JobId::parse(job_id.as_str().unwrap()).unwrap();
        listed(&owner, client, |list| active_kind(list, "source.prepare"));
        let waits = [
            waiting(&owner, client, Some(job.clone())),
            waiting(&owner, other, None),
        ];
        gate.open();
        for wait in waits {
            wait.recv_timeout(luxforge_testbase::HANG)
                .expect("the wait is released")
                .expect("the owner answered the wait");
        }

        // The job, the board and a wait that starts now agree: failed, internal.
        let status = ok(
            &owner,
            client,
            "status",
            "job.read",
            json!({"job_id": job_id}),
        );
        assert_eq!(status["status"], json!("failed"), "{status}");
        assert_eq!(status["error"]["code"], json!("internal"), "{status}");
        let list = ok(&owner, client, "list", "activity.list", json!({}));
        assert_eq!(list["active"], json!([]), "{list}");
        assert_eq!(list["recent"][0]["job_id"], job_id, "{list}");
        assert_eq!(list["recent"][0]["outcome"], json!("failed"), "{list}");
        owner
            .wait_source(client, Some(&job))
            .expect("a failed job is not waited for");

        // The worker lives on: the next source job runs, and every other client is served.
        let again = ok(
            &owner,
            client,
            "again",
            "catalog.import",
            import_params(&photo),
        );
        assert_ne!(again["job_id"], job_id, "a new job");
        assert_eq!(
            wait_source(&owner, client, again["job_id"].as_str().unwrap())["status"],
            json!("ready")
        );
        assert!(ok(&owner, other, "list", "catalog.list", json!({}))["assets"].is_array());
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
        std::fs::remove_file(photo).unwrap();
    }

    /// A panic while the owner serves one message is contained. The request it was serving is
    /// answered `internal`; a source job whose completion it was committing fails `internal`,
    /// commits nothing and releases its waiters; and the owner goes on serving that client, every
    /// other client and the next source job. The panic comes from the owner's test fault, where a
    /// handler or a completion's commit runs.
    #[test]
    fn a_panic_while_the_owner_serves_a_message_answers_internal_and_the_owner_serves_on() {
        let catalog = temp("owner-panic.sqlite");
        let photo = temp("owner-panic.jpg");
        let _ = std::fs::remove_file(&catalog);
        std::fs::copy(fixture(), &photo).unwrap();
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let other = owner.register();
        let panicking = |at: &'static str| -> Fault {
            Arc::new(move |served: &str| {
                if served == at {
                    panic!("the owner panicked serving {served}");
                }
            })
        };
        let assets =
            |client| ok(&owner, client, "list", "catalog.list", json!({}))["assets"].clone();

        // A handler that panics answers its own request `internal`, and the owner serves on.
        owner.fault(Some(panicking("catalog.list")));
        let error = failure(&owner, client, "panics", "catalog.list", json!({}));
        assert_eq!(error.code, "internal", "{}", error.message);
        assert!(
            ok(&owner, other, "state", "session.state", json!({}))["revision"].is_u64(),
            "another client is served"
        );
        owner.fault(None);
        assert_eq!(assets(client), json!([]), "and so is the same client");

        // A completion that panics as it commits fails its job and releases its waiters.
        owner.fault(Some(panicking("source.complete")));
        let queued = ok(
            &owner,
            client,
            "import",
            "catalog.import",
            import_params(&photo),
        );
        let job_id = queued["job_id"].clone();
        let job = JobId::parse(job_id.as_str().unwrap()).unwrap();
        waiting(&owner, client, Some(job))
            .recv_timeout(luxforge_testbase::HANG)
            .expect("the wait is released")
            .expect("the owner answered the wait");
        let status = ok(
            &owner,
            client,
            "status",
            "job.read",
            json!({"job_id": job_id}),
        );
        assert_eq!(status["status"], json!("failed"), "{status}");
        assert_eq!(status["error"]["code"], json!("internal"), "{status}");
        assert_eq!(assets(other), json!([]), "the failed commit added nothing");

        // The next source job commits.
        owner.fault(None);
        let again = ok(
            &owner,
            client,
            "again",
            "catalog.import",
            import_params(&photo),
        );
        assert_eq!(
            wait_source(&owner, client, again["job_id"].as_str().unwrap())["status"],
            json!("ready")
        );
        assert_eq!(assets(other).as_array().map(Vec::len), Some(1));
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
        std::fs::remove_file(photo).unwrap();
    }

    /// Clipping overlay settings are per-client session state, reported by `session.state` and set
    /// through `workspace.set`; they mutate nothing and emit no event. Discovery lists the three
    /// analysis methods, so an independent JSON client needs no GUI and no hand-written list.
    #[test]
    fn the_clipping_overlay_flags_round_trip_and_the_analysis_methods_are_discoverable() {
        let catalog = temp("analysis-workspace.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let one = owner.register();
        let two = owner.register();
        let state = ok(&owner, one, "state", "session.state", json!({}));
        assert_eq!(state["workspace"]["clip_shadows"], json!(false));
        assert_eq!(state["workspace"]["clip_highlights"], json!(false));
        let set = ok(
            &owner,
            one,
            "set",
            "workspace.set",
            json!({"clip_shadows": true, "clip_highlights": true}),
        );
        assert_eq!(set["workspace"]["clip_shadows"], json!(true));
        assert_eq!(set["workspace"]["clip_highlights"], json!(true));
        assert_eq!(
            set["workspace"]["thirds"],
            json!(false),
            "nothing else moved"
        );
        let off = ok(
            &owner,
            one,
            "off",
            "workspace.set",
            json!({"clip_highlights": false}),
        );
        assert_eq!(off["workspace"]["clip_shadows"], json!(true));
        assert_eq!(off["workspace"]["clip_highlights"], json!(false));
        assert_eq!(
            ok(&owner, one, "read", "session.state", json!({}))["workspace"]["clip_shadows"],
            json!(true)
        );
        // Another client's overlay settings are its own.
        assert_eq!(
            ok(&owner, two, "other", "session.state", json!({}))["workspace"]["clip_shadows"],
            json!(false)
        );
        let schema = ok(&owner, one, "schema", "schema.list", json!({}));
        let workspace = &schema["methods"]["workspace.set"]["optional"];
        assert!(workspace["clip_shadows"].is_string());
        assert!(workspace["clip_highlights"].is_string());
        for method in ["analysis.request", "job.read", "job.cancel"] {
            assert_eq!(
                schema["methods"][method]["mutates"],
                json!(false),
                "{method} mutates nothing"
            );
        }
        assert_eq!(
            schema["methods"]["analysis.request"]["required"],
            json!(["asset_id", "target"])
        );
        assert_eq!(schema["methods"]["job.read"]["required"], json!(["job_id"]));
        assert_eq!(
            schema["methods"]["job.cancel"]["required"],
            json!(["job_id"])
        );
        // One job API serves every kind; no kind keeps a read or cancel of its own.
        for retired in [
            "job.status",
            "analysis.read",
            "analysis.cancel",
            "module.job.read",
            "module.job.cancel",
            "export.read",
            "export.cancel",
        ] {
            assert!(
                schema["methods"].get(retired).is_none(),
                "{retired} is not a method"
            );
        }
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// The events a client reads after `after`, as `(method, request_id)`, and the log's sequence.
    fn events_after(
        owner: &OwnerHandle,
        client: ClientId,
        after: u64,
    ) -> (Vec<(String, String)>, u64) {
        let read = ok(
            owner,
            client,
            "events",
            "events.since",
            json!({"after": after}),
        );
        let events = read["events"]
            .as_array()
            .expect("the events")
            .iter()
            .map(|event| {
                (
                    event["method"].as_str().unwrap().to_owned(),
                    event["request_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        (events, read["current_sequence"].as_u64().unwrap())
    }

    /// A retry answered from the request table changed nothing, so it records no event: the first
    /// attempt announced the change, and a client watching the log sees it once. The retry still
    /// answers with the original result, marked `deduplicated`, at the unchanged sequence.
    #[test]
    fn a_retried_mutation_answered_from_the_request_table_records_no_event() {
        let catalog = temp("retry-event.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let client = owner.register();
        let asset = import_asset(&owner, client, &fixture())["asset"]["id"].clone();
        let (_, before) = events_after(&owner, client, 0);

        let first = send(
            &owner,
            client,
            "first",
            "edit.set-pixel",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 0, "request_id": "first", "actor": "test"},
                "x": 0, "y": 0, "rgb": [1, 2, 3],
            }),
        );
        let applied = first.result.expect("the first attempt applies");
        assert_eq!(applied["outcome"], json!("applied"));
        assert_eq!(applied["deduplicated"], json!(false));
        assert_eq!(
            first.sequence,
            before + 1,
            "the first attempt records one event"
        );
        assert_eq!(
            events_after(&owner, client, before),
            (
                vec![("edit.set-pixel".to_owned(), "first".to_owned())],
                before + 1
            )
        );

        let retry = send(
            &owner,
            client,
            "first",
            "edit.set-pixel",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 0, "request_id": "first", "actor": "test"},
                "x": 0, "y": 0, "rgb": [1, 2, 3],
            }),
        );
        let retried = retry.result.expect("the retry answers");
        assert_eq!(retried["deduplicated"], json!(true));
        assert_eq!(retried["outcome"], applied["outcome"]);
        assert_eq!(retried["current_entry_id"], applied["current_entry_id"]);
        assert_eq!(retry.sequence, before + 1, "the retry records no event");
        assert_eq!(
            events_after(&owner, client, before),
            (
                vec![("edit.set-pixel".to_owned(), "first".to_owned())],
                before + 1
            ),
            "the log holds the first attempt's event alone"
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// Every event names what it changed when the change has a subject: an import names the asset
    /// it created and its revision, a commit, a navigation and a drafted commit name the asset and
    /// the revision they left it at, and a version names its asset but no revision, because naming
    /// an entry moves none. A change to the preset library names no asset. So a client with one
    /// photograph open can tell another client's change to a second photograph from one to its
    /// own, and a change it already holds from a newer one.
    #[test]
    fn events_name_the_asset_and_revision_they_changed() {
        let catalog = temp("event-subjects.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let desktop = owner.register();
        let agent = owner.register();
        let first = import_asset(&owner, desktop, &fixture())["asset"]["id"].clone();
        let second = import_asset(
            &owner,
            agent,
            &luxforge_testbase::paths::fixture("s0/orientation-2.jpg"),
        )["asset"]["id"]
            .clone();
        assert_ne!(first, second);
        let subjects = |after: u64| -> Vec<(String, Value, Value)> {
            ok(
                &owner,
                desktop,
                "events",
                "events.since",
                json!({"after": after}),
            )["events"]
                .as_array()
                .expect("the events")
                .iter()
                .map(|event| {
                    (
                        event["method"].as_str().unwrap().to_owned(),
                        event.get("asset_id").cloned().unwrap_or(Value::Null),
                        event.get("revision").cloned().unwrap_or(Value::Null),
                    )
                })
                .collect()
        };
        assert_eq!(
            subjects(0),
            [
                ("catalog.import".to_owned(), first.clone(), json!(0)),
                ("catalog.import".to_owned(), second.clone(), json!(0)),
            ],
            "an import names the asset it created"
        );
        let (_, before) = events_after(&owner, desktop, 0);

        pixel_edit(&owner, agent, &second, 0, "agent-edit", [1, 2, 3]);
        let undo = |client: ClientId, asset: &Value, revision: u64, request: &str| {
            ok(
                &owner,
                client,
                request,
                "history.undo",
                json!({"asset_id": asset, "mutation": {"expected_revision": revision, "request_id": request, "actor": "test"}}),
            )
        };
        undo(agent, &second, 1, "agent-undo");
        let begun = ok(
            &owner,
            desktop,
            "begin",
            "draft.begin",
            json!({"asset_id": first, "action": "set-basic"}),
        );
        ok(
            &owner,
            desktop,
            "set",
            "draft.set",
            json!({"draft_id": begun["draft_id"], "fields": {"exposure": 0.5}}),
        );
        ok(
            &owner,
            desktop,
            "commit",
            "draft.commit",
            json!({"draft_id": begun["draft_id"], "mutation": {"expected_revision": 0, "request_id": "drafted", "actor": "test"}}),
        );
        ok(
            &owner,
            agent,
            "version",
            "version.create",
            json!({"asset_id": first, "name": "Kept", "mutation": envelope()}),
        );
        ok(
            &owner,
            agent,
            "preset",
            "preset.create",
            json!({"name": "Bright", "settings": {"set-basic": {"exposure": 1.0}}, "mutation": envelope()}),
        );
        assert_eq!(
            subjects(before),
            [
                ("edit.set-pixel".to_owned(), second.clone(), json!(1)),
                ("history.undo".to_owned(), second.clone(), json!(2)),
                ("draft.commit".to_owned(), first.clone(), json!(1)),
                ("version.create".to_owned(), first.clone(), Value::Null),
                ("preset.create".to_owned(), Value::Null, Value::Null),
            ]
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A cursor past the log's newest sequence was read from another owner process, whose events
    /// this log never held: the client is told it missed something rather than left waiting for
    /// sequences this process has already used. A cursor at the newest sequence is simply caught
    /// up, and one inside the log reads what follows it.
    #[test]
    fn events_since_reports_a_gap_for_a_cursor_beyond_the_log() {
        let mut log = EventLog::default();
        assert!(!log.since(0).gap, "an empty log and a fresh cursor");
        assert!(log.since(1).gap, "a cursor ahead of an empty log");
        for request in ["a", "b", "c"] {
            log.record(&Origin::new("edit.set-pixel", request));
        }
        assert!(!log.since(0).gap && log.since(0).events.len() == 3);
        assert!(!log.since(2).gap && log.since(2).events.len() == 1);
        assert!(!log.since(3).gap, "caught up");
        let ahead = log.since(7);
        assert!(ahead.gap, "a cursor from an earlier owner process");
        assert!(ahead.events.is_empty());
        assert_eq!(ahead.current_sequence, 3);

        // Through the method, as a JSON client reads it.
        let catalog = temp("event-gap.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        import_asset(&owner, client, &fixture());
        let read = |after: u64| {
            ok(
                &owner,
                client,
                "events",
                "events.since",
                json!({"after": after}),
            )
        };
        assert_eq!(read(1)["gap"], json!(false));
        assert_eq!(read(2)["gap"], json!(true));
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// Every method family that changes something without a revision takes the `{request_id,
    /// actor}` envelope, and a retry of it is answered from the owner's request table: the first
    /// answer comes back marked `deduplicated`, nothing changes again and no second event is
    /// recorded. The same `request_id` with other input is a conflict. A revisioned family,
    /// history here, answers its retry from the catalog's own request table the same way, and
    /// `draft.commit`, which ends its draft, from the owner's.
    #[test]
    fn a_retry_of_every_mutation_family_returns_the_first_answer_and_records_no_event() {
        let catalog = temp("retry-families.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let client = owner.register();
        let photo = temp("retry-families.jpg");
        std::fs::copy(fixture(), &photo).unwrap();
        // The same request twice, as a client resends it after losing the answer.
        let twice = |id: &str, method: &str, params: Value| -> (Value, Value, u64) {
            let (_, before) = events_after(&owner, client, 0);
            let first = ok(&owner, client, id, method, params.clone());
            let (_, after_first) = events_after(&owner, client, 0);
            let retry = send(&owner, client, id, method, params);
            let (_, after_retry) = events_after(&owner, client, 0);
            let retried = retry
                .result
                .unwrap_or_else(|| panic!("{method}: {:?}", retry.error));
            assert_eq!(first["deduplicated"], json!(false), "{method}");
            assert_eq!(retried["deduplicated"], json!(true), "{method}");
            let mut original = retried.clone();
            original["deduplicated"] = json!(false);
            assert_eq!(original, first, "{method}: the retry is the first answer");
            assert_eq!(
                after_retry, after_first,
                "{method}: the retry records no event"
            );
            assert_eq!(retry.sequence, after_first, "{method}");
            (first, retried, after_first - before)
        };
        let conflict = |method: &str, params: Value| {
            let error = failure(&owner, client, "conflict", method, params);
            assert_eq!(error.code, "conflict", "{method}: {}", error.message);
            assert_eq!(
                error.message, "request_id was already used with different input",
                "{method}"
            );
        };
        let request = |request_id: &str| json!({"request_id": request_id, "actor": "test"});

        // catalog: the import commits its asset, and its event, once. The event is recorded when
        // the worker's preparation commits, so the retry is sent after that.
        let import = json!({"path": photo, "mutation": request("import-1")});
        let queued = ok(&owner, client, "import", "catalog.import", import.clone());
        assert_eq!(queued["deduplicated"], json!(false));
        let job_id = queued["job_id"].as_str().unwrap().to_owned();
        let ready = wait_source(&owner, client, &job_id);
        assert_eq!(ready["status"], "ready");
        let asset = ready["result"]["asset"]["id"].clone();
        let (events, imported_at) = events_after(&owner, client, 0);
        assert_eq!(events, [("catalog.import".to_owned(), "import".to_owned())]);
        let retried = ok(&owner, client, "import", "catalog.import", import);
        assert_eq!(retried["deduplicated"], json!(true));
        assert_eq!(retried["job_id"], json!(job_id));
        assert_eq!(retried["status"], queued["status"], "the first answer");
        assert_eq!(events_after(&owner, client, 0).1, imported_at);
        conflict(
            "catalog.import",
            json!({"path": fixture(), "mutation": request("import-1")}),
        );

        // preset: create, update, import and delete.
        let settings = json!({"set-basic": {"exposure": 0.5}});
        let (created, _, announced) = twice(
            "create",
            "preset.create",
            json!({"name": "Soft", "settings": settings, "mutation": request("preset-1")}),
        );
        assert_eq!(announced, 1);
        assert_eq!(created["preset"]["actor"], json!("test"));
        let presets = ok(&owner, client, "list", "preset.list", json!({}));
        assert_eq!(
            presets["presets"].as_array().unwrap().len(),
            1,
            "one preset"
        );
        conflict(
            "preset.create",
            json!({"name": "Hard", "settings": settings, "mutation": request("preset-1")}),
        );
        let preset_id = created["preset"]["id"].clone();
        let (updated, _, announced) = twice(
            "update",
            "preset.update",
            json!({"preset_id": preset_id, "name": "Softer", "mutation": request("preset-2")}),
        );
        assert_eq!(
            (updated["outcome"].clone(), announced),
            (json!("applied"), 1)
        );
        let document = ok(
            &owner,
            client,
            "export",
            "preset.export",
            json!({"preset_id": preset_id}),
        );
        let (imported, _, announced) = twice(
            "import",
            "preset.import",
            json!({
                "content": document["content"], "name": "Imported copy",
                "mutation": request("preset-3"),
            }),
        );
        assert_eq!(announced, 1);
        assert_eq!(imported["preset"]["name"], json!("Imported copy"));
        let (deleted, _, announced) = twice(
            "delete",
            "preset.delete",
            json!({"preset_id": preset_id, "mutation": request("preset-4")}),
        );
        assert_eq!((deleted["deleted"].clone(), announced), (json!(true), 1));
        let presets = ok(&owner, client, "list", "preset.list", json!({}));
        assert_eq!(
            presets["presets"].as_array().unwrap().len(),
            1,
            "the import is left"
        );

        // version: create and delete, per asset.
        let (named, _, announced) = twice(
            "version",
            "version.create",
            json!({"asset_id": asset, "name": "Start", "mutation": request("version-1")}),
        );
        assert_eq!((named["outcome"].clone(), announced), (json!("applied"), 1));
        assert_eq!(named["version"]["actor"], json!("test"));
        conflict(
            "version.create",
            json!({"asset_id": asset, "name": "Other", "mutation": request("version-1")}),
        );
        let (removed, _, announced) = twice(
            "unversion",
            "version.delete",
            json!({"asset_id": asset, "name": "Start", "mutation": request("version-2")}),
        );
        assert_eq!(
            (removed["outcome"].clone(), announced),
            (json!("applied"), 1)
        );

        // artifact: a collection is accepted once.
        let (collect, _, announced) = twice(
            "collect",
            "artifact.collect",
            json!({"mutation": request("collect-1")}),
        );
        assert_eq!(announced, 1);
        assert_eq!(
            wait_source(&owner, client, collect["job_id"].as_str().unwrap())["status"],
            "ready"
        );

        // export: the event is recorded when the job has written its file, so the retry is sent
        // after that, and answers with the same job rather than a conflict on the file it wrote.
        let exported = temp("retry-families-export.jpg");
        let export =
            json!({"asset_id": asset, "destination": exported, "mutation": request("export-1")});
        let first = ok(&owner, client, "export", "export.jpeg", export.clone());
        assert_eq!(first["deduplicated"], json!(false));
        luxforge_testbase::wait_until("the export to finish", || {
            ok(
                &owner,
                client,
                "read",
                "job.read",
                json!({"job_id": first["job_id"]}),
            )["status"]
                == "ready"
        });
        let (events, exported_at) = events_after(&owner, client, 0);
        assert_eq!(
            events.last(),
            Some(&("export.jpeg".to_owned(), "export".to_owned()))
        );
        let retried = ok(&owner, client, "export", "export.jpeg", export);
        assert_eq!(retried["deduplicated"], json!(true));
        assert_eq!(retried["job_id"], first["job_id"]);
        assert_eq!(events_after(&owner, client, 0).1, exported_at);
        conflict(
            "export.jpeg",
            json!({"asset_id": asset, "destination": temp("retry-families-other.jpg"), "mutation": request("export-1")}),
        );
        std::fs::remove_file(exported).unwrap();

        // history, a revisioned family: the catalog's request table answers its retry.
        pixel_edit(&owner, client, &asset, 0, "edit-1", [1, 2, 3]);
        let undo = json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 1, "request_id": "undo-1", "actor": "test"},
        });
        let (_, before) = events_after(&owner, client, 0);
        let first = ok(&owner, client, "undo", "history.undo", undo.clone());
        let retry = ok(&owner, client, "undo", "history.undo", undo);
        assert_eq!(first["deduplicated"], json!(false));
        assert_eq!(retry["deduplicated"], json!(true));
        assert_eq!(retry["current_entry_id"], first["current_entry_id"]);
        assert_eq!(events_after(&owner, client, 0).1, before + 1);

        // draft: a commit ends the draft it names, so the owner's request table answers its retry.
        let state = |owner: &OwnerHandle| {
            ok(
                owner,
                client,
                "state",
                "asset.state",
                json!({"asset_id": asset}),
            )
        };
        let revision = state(&owner)["revision"].clone();
        let begun = ok(
            &owner,
            client,
            "begin",
            "draft.begin",
            json!({"asset_id": asset, "action": "set-basic"}),
        );
        let draft_id = begun["draft_id"].clone();
        ok(
            &owner,
            client,
            "set",
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"exposure": 0.5}}),
        );
        let commit = |draft_id: &Value| {
            json!({
                "draft_id": draft_id,
                "mutation": {"expected_revision": revision, "request_id": "commit-1", "actor": "test"},
            })
        };
        let (committed, _, announced) = twice("commit", "draft.commit", commit(&draft_id));
        assert_eq!(
            (committed["outcome"].clone(), announced),
            (json!("applied"), 1)
        );
        let after = state(&owner);
        assert_eq!(after["revision"], committed["revision"], "no second entry");
        assert_eq!(after["current_entry"]["id"], committed["current_entry_id"]);
        conflict("draft.commit", commit(&json!(crate::DraftId::new())));

        // The envelope is required and carries no revision here.
        for (method, params, expected) in [
            (
                "preset.delete",
                json!({"preset_id": preset_id}),
                "missing field `mutation`",
            ),
            (
                "version.create",
                json!({"asset_id": asset, "name": "X", "mutation": {"expected_revision": 0, "request_id": "r", "actor": "a"}}),
                "unknown field `expected_revision`",
            ),
            (
                "preset.create",
                json!({"name": "Y", "settings": settings, "mutation": {"request_id": "", "actor": "a"}}),
                "request_id must contain 1..128 characters",
            ),
        ] {
            let error = failure(&owner, client, "envelope", method, params);
            assert_eq!(error.code, "validation", "{method}");
            assert!(
                error.message.contains(expected),
                "{method}: {}",
                error.message
            );
        }
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
        std::fs::remove_file(photo).unwrap();
    }

    /// A value a declared kind accepts, at the edge of its range where it has one, and one it
    /// refuses, just outside it; `None` for a kind whose values the field's own type checks: `json`,
    /// whose every value is in range, and a secret, which is never a plain value to the generic
    /// check.
    fn kind_samples(parameter: &crate::ParameterDescriptor) -> Option<(Value, Value)> {
        use crate::{IdentityKind, ParameterKind};
        Some(match &parameter.kind {
            ParameterKind::Integer { min, max } => {
                (json!(max), json!(max.checked_add(1).unwrap_or(min - 1)))
            }
            ParameterKind::Number { min: _, max } => (json!(max), json!(max + max.abs().max(1.0))),
            ParameterKind::Enum { options } => (json!(options[0]), json!("not-an-option")),
            ParameterKind::Boolean => (json!(true), json!("true")),
            ParameterKind::String { max_length } => (
                json!("x".repeat(*max_length)),
                json!("x".repeat(max_length + 1)),
            ),
            // Text may hold a line break, which a string may not.
            ParameterKind::Text { max_bytes } => {
                (json!("a line\n"), json!("x".repeat(max_bytes + 1)))
            }
            ParameterKind::Identity { of } => {
                let valid = match of {
                    IdentityKind::Asset => crate::AssetId::new().to_string(),
                    IdentityKind::Entry => crate::EntryId::new().to_string(),
                    IdentityKind::Draft => crate::DraftId::new().to_string(),
                    IdentityKind::Job => crate::JobId::new().to_string(),
                    IdentityKind::Preset => crate::PresetId::new().to_string(),
                    IdentityKind::Mask => crate::MaskId::new().to_string(),
                    IdentityKind::Component => crate::ComponentId::new().to_string(),
                    IdentityKind::Stroke => "0".repeat(32),
                    IdentityKind::CatalogFolder => {
                        crate::catalog_types::CatalogFolderId::new().to_string()
                    }
                    IdentityKind::Collection => {
                        crate::catalog_types::CollectionId::new().to_string()
                    }
                };
                (json!(valid), json!("x"))
            }
            ParameterKind::Artifact => (json!(format!("artifact-{}", "0".repeat(64))), json!("x")),
            ParameterKind::Settings => (json!({"set-basic": {"exposure": 0.5}}), json!({})),
            ParameterKind::Json | ParameterKind::Secret { .. } => return None,
            other => panic!("no host method declares a {} parameter", other.name()),
        })
    }

    /// Every method `schema.list` lists is answered through the one table, and its schema is its
    /// parser: each declared field is accepted by name, all of them together raise no
    /// unknown-field error, and one field nothing declares is a validation error — naming it, for a
    /// host method, whose parameters are declared once as the struct it parses. The owner answers,
    /// so the service and owner handlers are covered alike, and the proof module's task is listed
    /// too. Values are `null`: the point is which names each parser knows, not what it accepts.
    ///
    /// Then what it accepts: each host method's typed field is sent, alone, a value its declared
    /// kind accepts at the edge of its range, which neither the kind check nor the field's type
    /// refuses, and a value just outside it, which is refused where the request is parsed in the
    /// generic check's words, naming the field. Every kind a host method declares is exercised.
    #[test]
    fn every_listed_method_accepts_its_declared_fields_and_refuses_an_unknown_one() {
        let catalog = temp("declared.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(crate::CapabilitiesProofModule::new(
                "http://127.0.0.1:9",
            )))
            .unwrap();
        let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
        let client = owner.register();
        let schema = ok(&owner, client, "schema", "schema.list", json!({}));
        let listed = schema["methods"].as_object().expect("the methods");
        assert!(listed.keys().any(|name| name.starts_with("task.")));
        const UNDECLARED: &str = "_undeclared";
        for (name, method) in listed {
            let host = methods::METHODS.iter().any(|spec| spec.name == name);
            let mut declared: Vec<String> = method["required"]
                .as_array()
                .expect("required fields")
                .iter()
                .map(|field| field.as_str().expect("a field name").to_owned())
                .collect();
            declared.extend(
                method["optional"]
                    .as_object()
                    .expect("optional fields")
                    .keys()
                    .cloned(),
            );
            let unknown = |response: &ApiResponse, field: &str| {
                response.error.as_ref().is_some_and(|error| {
                    error.message.contains(&format!("unknown field `{field}`"))
                })
            };
            for field in &declared {
                let response = send(&owner, client, name, name, json!({field.as_str(): null}));
                assert!(
                    !unknown(&response, field),
                    "{name} refuses its declared {field}"
                );
                assert!(
                    response
                        .error
                        .as_ref()
                        .is_none_or(|error| !error.message.starts_with("unknown method")),
                    "{name} is listed but not answered"
                );
            }
            let all: serde_json::Map<String, Value> = declared
                .iter()
                .map(|field| (field.clone(), Value::Null))
                .collect();
            let response = send(&owner, client, name, name, Value::Object(all.clone()));
            for field in &declared {
                assert!(
                    !unknown(&response, field),
                    "{name} refuses its declared {field}"
                );
            }
            let mut probe = all;
            probe.insert(UNDECLARED.into(), json!(1));
            let error = send(&owner, client, name, name, Value::Object(probe))
                .error
                .unwrap_or_else(|| panic!("{name} accepted an undeclared field"));
            assert_eq!(error.code, "validation", "{name}: {}", error.message);
            if host {
                assert!(
                    error
                        .message
                        .contains(&format!("unknown field `{UNDECLARED}`")),
                    "{name}: {}",
                    error.message
                );
            }
        }
        // Each typed field of each host method, in and just out of its declared range.
        let mut exercised = std::collections::BTreeSet::new();
        for spec in methods::METHODS {
            let name = spec.name;
            for parameter in (spec.params.parameters)() {
                let field = parameter.name.as_str();
                let Some((valid, outside)) = kind_samples(parameter) else {
                    continue;
                };
                exercised.insert(parameter.kind.name());
                let refused_here = format!("parameter {field} ");
                let response = send(&owner, client, name, name, json!({field: valid}));
                if let Some(error) = &response.error {
                    let message = &error.message;
                    assert!(
                        !message.starts_with(&refused_here)
                            && !message.contains("invalid type")
                            && !message.contains("invalid value")
                            && !message.contains("invalid length")
                            && !message.starts_with("invalid "),
                        "{name} refuses {field} = {valid}, which its kind accepts: {message}"
                    );
                }
                let error = send(&owner, client, name, name, json!({field: outside}))
                    .error
                    .unwrap_or_else(|| panic!("{name} accepted {field} = {outside}"));
                assert_eq!(
                    error.code, "validation",
                    "{name} {field}: {}",
                    error.message
                );
                assert!(
                    error.message.starts_with(&refused_here),
                    "{name} refuses {field} = {outside} by its declared kind: {}",
                    error.message
                );
            }
        }
        assert_eq!(
            exercised.into_iter().collect::<Vec<_>>(),
            [
                "artifact", "boolean", "enum", "identity", "integer", "number", "settings",
                "string", "text"
            ],
            "every kind a host method declares, but json and secret, is exercised"
        );
        // Methods that take nothing say so too.
        for name in [
            "schema.list",
            "preset.list",
            "session.state",
            "preview.return-current",
            "activity.list",
            "resources.read",
            "artifact.status",
        ] {
            assert!(send(&owner, client, name, name, json!({})).error.is_none());
            assert!(
                send(&owner, client, name, name, Value::Null)
                    .error
                    .is_none()
            );
            let error = send(&owner, client, name, name, json!({"filter": "x"}))
                .error
                .unwrap_or_else(|| panic!("{name} ignored a parameter"));
            assert_eq!(
                (error.code.as_str(), error.message.as_str()),
                ("validation", "unknown field `filter`, there are no fields"),
                "{name}"
            );
        }
        assert_eq!(
            send(&owner, client, "missing", "test.missing", json!({}))
                .error
                .expect("an unknown method is refused")
                .code,
            "protocol"
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// A malformed request is refused with one structured error however it arrives: the code and
    /// message the API answers are the service's own error, unchanged.
    #[test]
    fn a_malformed_request_is_the_same_error_through_the_service_and_the_api() {
        let catalog = temp("malformed.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let registry = Arc::new(ModuleRegistry::developer());
        let malformed = json!({"x": 0, "y": 0, "rgb": [1, 2]});
        let (asset, direct) = {
            let mut service = EditorService::open_with(&catalog, Arc::clone(&registry)).unwrap();
            let asset = service.import(&fixture()).unwrap().asset.id;
            let direct = service
                .apply_action(
                    &asset,
                    crate::editor::mutation(0, "malformed"),
                    "set-pixel",
                    malformed.clone(),
                )
                .expect_err("an rgb of two channels is refused");
            (asset, direct)
        };
        let (owner, join) = OwnerHandle::start_with(&catalog, registry).unwrap();
        let client = owner.register();
        let mut params = malformed;
        params["asset_id"] = json!(asset);
        params["mutation"] = crate::editor::mutation_json(0, "malformed");
        let failure = failure(&owner, client, "malformed", "edit.set-pixel", params);
        assert_eq!(
            serde_json::to_value(failure).unwrap(),
            json!({"code": direct.kind.code(), "message": direct.detail})
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// Every mutating method `schema.list` lists — the host's, a module action, a `mask.*` command
    /// and a module's task — has its envelope checked once, by the dispatcher, before any handler
    /// runs: a request identity or actor out of range is refused in the envelope's words although
    /// every other field is missing, which the handler's own parse, or a permission grant's
    /// authority check, would have refused first, and nothing is announced.
    #[test]
    fn every_mutating_method_has_its_envelope_checked_before_its_handler_runs() {
        let catalog = temp("envelope.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(crate::CapabilitiesProofModule::new(
                "http://127.0.0.1:9",
            )))
            .unwrap();
        let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
        let client = owner.register();
        let schema = ok(&owner, client, "schema", "schema.list", json!({}));
        let listed = schema["methods"].as_object().expect("the methods");
        let (_, before) = events_after(&owner, client, 0);
        let mut checked = Vec::new();
        for (name, method) in listed {
            let Some(envelope) = method.get("mutation").and_then(Value::as_str) else {
                continue;
            };
            let sent = |request_id: &str, actor: &str| {
                let mut mutation = json!({"request_id": request_id, "actor": actor});
                if envelope == "revision" {
                    mutation["expected_revision"] = json!(0);
                }
                json!({"mutation": mutation})
            };
            for (params, refusal) in [
                (
                    sent("", "test"),
                    "request_id must contain 1..128 characters",
                ),
                (
                    sent("request", &"a".repeat(129)),
                    "actor must contain 1..128 characters",
                ),
            ] {
                let error = failure(&owner, client, name, name, params);
                assert_eq!(
                    (error.code.as_str(), error.message.as_str()),
                    ("validation", refusal),
                    "{name}"
                );
            }
            checked.push(name.as_str());
        }
        for family in ["edit.", "mask.", "task.", "module.", "preset.", "history."] {
            assert!(
                checked.iter().any(|name| name.starts_with(family)),
                "a {family}* method is checked: {checked:?}"
            );
        }
        for name in [
            "catalog.import",
            "artifact.collect",
            "export.jpeg",
            "draft.commit",
        ] {
            assert!(checked.contains(&name), "{name}");
        }
        assert_eq!(
            events_after(&owner, client, 0).1,
            before,
            "nothing was announced"
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// Hold the point worker before each evaluation: `reached` receives once per sample it takes,
    /// and each send on `release` lets one go.
    fn hold_points(owner: &OwnerHandle) -> (std::sync::mpsc::Receiver<()>, SyncSender<()>) {
        let (reached, reaches) = std::sync::mpsc::channel();
        let (release, released) = sync_channel::<()>(64);
        let reached = std::sync::Mutex::new(reached);
        let released = std::sync::Mutex::new(released);
        owner.hold_points(Some(Arc::new(move || {
            let _ = reached.lock().unwrap().send(());
            let _ = released.lock().unwrap().recv();
        })));
        (reaches, release)
    }

    fn presence(
        owner: &OwnerHandle,
        client: ClientId,
        asset: &Value,
        revision: u64,
        request: &str,
        fields: Value,
    ) -> Value {
        let mut params = json!({
            "asset_id": asset,
            "mutation": {"expected_revision": revision, "request_id": request, "actor": "test"},
        });
        params
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        ok(owner, client, request, "edit.set-presence", params)
    }

    /// The whole frame of what `request` names, rendered exactly from the job the owner plans for
    /// it: the independent reference a sample is compared against.
    fn rendered(owner: &OwnerHandle, request: PreviewRequest) -> crate::Raster {
        let job = owner.preview_job(request).expect("a preview job");
        job.evaluation
            .source()
            .render(
                job.evaluation.registry(),
                job.evaluation.entry().snapshot.id.clone(),
                job.evaluation.recipe(),
            )
            .expect("a rendered frame")
    }

    fn sample_at(owner: &OwnerHandle, client: ClientId, id: &str, params: Value) -> ApiResponse {
        send(owner, client, id, "render.sample", params)
    }

    /// Through a spatial layer the owner only plans a sample: while the point worker holds it,
    /// another client reads state and commits, and the held sample then answers with the value, the
    /// entry and the snapshot it was planned against and the event sequence of that moment. Every
    /// sample, of an entry and of a draft, equals the full render at its pixel.
    #[test]
    fn a_spatial_sample_is_answered_off_the_owner_against_the_entry_it_was_planned_against() {
        let catalog = temp("point-worker.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let sampler = owner.register();
        let editor = owner.register();
        let state = import_asset(&owner, sampler, &fixture());
        let asset = state["asset"]["id"].clone();
        let asset_id = AssetId::parse(asset.as_str().unwrap()).unwrap();
        let sample = |client: ClientId, id: &str, x: u32, y: u32| {
            sample_at(
                &owner,
                client,
                id,
                json!({"asset_id": asset, "x": x, "y": y}),
            )
        };
        let points = [(0, 0), (479, 319), (240, 160), (17, 301), (300, 5)];

        let planned = presence(
            &owner,
            editor,
            &asset,
            0,
            "clarity",
            json!({"clarity": 60.0, "dehaze": 30.0}),
        );
        let frame = rendered(&owner, PreviewRequest::new(editor, asset_id.clone()));
        for (x, y) in points {
            let sampled = sample(sampler, "exact", x, y);
            assert!(sampled.error.is_none(), "{:?}", sampled.error);
            let sampled = sampled.result.unwrap();
            assert_eq!(
                sampled["rgba"],
                json!(frame.pixel(x, y).unwrap()),
                "({x}, {y})"
            );
            assert_eq!(sampled["entry_id"], planned["current_entry_id"]);
            assert_eq!(sampled["source_detail_ready"], json!(true));
        }

        let (reached, release) = hold_points(&owner);
        std::thread::scope(|scope| {
            let held = scope.spawn(|| sample(sampler, "held", 10, 10));
            reached
                .recv_timeout(luxforge_testbase::HANG)
                .expect("the point worker took the sample");
            // The owner is free: another client reads state and commits while the sample is held.
            let before = send(
                &owner,
                editor,
                "state",
                "asset.state",
                json!({"asset_id": asset}),
            );
            assert!(before.error.is_none(), "{:?}", before.error);
            let recommitted = presence(
                &owner,
                editor,
                &asset,
                1,
                "recommit",
                json!({"clarity": -40.0}),
            );
            assert_ne!(recommitted["current_entry_id"], planned["current_entry_id"]);
            release.send(()).unwrap();
            let held = held.join().unwrap();
            assert!(held.error.is_none(), "{:?}", held.error);
            assert_eq!(
                held.sequence, before.sequence,
                "the answer carries the sequence the owner had when it planned the sample"
            );
            let held = held.result.unwrap();
            assert_eq!(held["entry_id"], planned["current_entry_id"]);
            assert_eq!(
                held["snapshot_id"],
                before.result.unwrap()["current_entry"]["snapshot"]["id"]
            );
            assert_eq!(held["rgba"], json!(frame.pixel(10, 10).unwrap()));
        });
        owner.hold_points(None);
        let _ = release.send(());

        // A draft's sample is planned from the drafting client's session and evaluated the same way.
        let begun = ok(
            &owner,
            editor,
            "begin",
            "draft.begin",
            json!({"asset_id": asset, "action": "set-presence"}),
        );
        let draft_id = begun["draft_id"].clone();
        ok(
            &owner,
            editor,
            "set",
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"texture": 70.0}}),
        );
        let drafted = rendered(
            &owner,
            PreviewRequest::new(editor, asset_id)
                .draft(crate::DraftId::parse(draft_id.as_str().unwrap()).unwrap()),
        );
        for (x, y) in points {
            let sampled = sample_at(
                &owner,
                editor,
                "drafted",
                json!({"asset_id": asset, "x": x, "y": y, "draft_id": draft_id}),
            );
            assert!(sampled.error.is_none(), "{:?}", sampled.error);
            let sampled = sampled.result.unwrap();
            assert_eq!(
                sampled["rgba"],
                json!(drafted.pixel(x, y).unwrap()),
                "({x}, {y})"
            );
            assert_eq!(sampled["draft"]["draft_id"], draft_id);
        }
        // Another client cannot sample this draft.
        assert!(
            sample_at(
                &owner,
                sampler,
                "foreign",
                json!({"asset_id": asset, "x": 0, "y": 0, "draft_id": draft_id}),
            )
            .error
            .is_some()
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// The point worker's queue is bounded: with one sample held and the queue full, the next is
    /// refused with `resource-limit` at once, a disconnect drops only that client's waiting sample,
    /// and a sample without a spatial layer is answered by the owner itself meanwhile.
    #[test]
    fn a_full_point_queue_refuses_and_a_disconnect_drops_the_waiting_sample() {
        let catalog = temp("point-queue.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let editor = owner.register();
        let state = import_asset(&owner, editor, &fixture());
        let asset = state["asset"]["id"].clone();
        let plain = sample_at(
            &owner,
            editor,
            "plain",
            json!({"asset_id": asset, "x": 1, "y": 1}),
        )
        .result
        .expect("a sample of the Original");
        presence(
            &owner,
            editor,
            &asset,
            0,
            "clarity",
            json!({"clarity": 60.0}),
        );

        let (reached, release) = hold_points(&owner);
        let clients: Vec<ClientId> = (0..=POINT_QUEUE_CAPACITY)
            .map(|_| owner.register())
            .collect();
        std::thread::scope(|scope| {
            let sample = |client: ClientId| {
                let owner = &owner;
                let asset = &asset;
                scope.spawn(move || {
                    owner.call(
                        client,
                        ApiRequest {
                            id: "queued".into(),
                            method: "render.sample".into(),
                            params: json!({"asset_id": asset, "x": 5, "y": 5}),
                            token: None,
                        },
                    )
                })
            };
            let running = sample(clients[0]);
            reached
                .recv_timeout(luxforge_testbase::HANG)
                .expect("the first sample is being evaluated");
            let waiting: Vec<_> = clients[1..].iter().map(|client| sample(*client)).collect();
            luxforge_testbase::wait_until("the point queue to fill", || {
                owner.points_waiting() >= POINT_QUEUE_CAPACITY
            });
            let refused = failure(
                &owner,
                editor,
                "refused",
                "render.sample",
                json!({"asset_id": asset, "x": 5, "y": 5}),
            );
            assert_eq!(refused.code, "resource-limit", "{refused:?}");

            // The owner still answers everything else, and the one client that goes loses only its
            // own waiting sample.
            let undone = ok(
                &owner,
                editor,
                "undo",
                "history.undo",
                json!({"asset_id": asset, "mutation": {"expected_revision": 1, "request_id": "undo", "actor": "test"}}),
            );
            assert_eq!(undone["revision"], json!(2));
            assert_eq!(
                ok(
                    &owner,
                    editor,
                    "plain",
                    "render.sample",
                    json!({"asset_id": asset, "x": 1, "y": 1})
                )["rgba"],
                plain["rgba"],
                "a sample without a spatial layer is answered by the owner while the worker is held"
            );
            owner.disconnect(clients[1]);
            assert_eq!(owner.points_waiting(), POINT_QUEUE_CAPACITY - 1);
            for _ in 0..=POINT_QUEUE_CAPACITY {
                release.send(()).unwrap();
            }
            let answered = running.join().unwrap().expect("answered");
            assert!(answered.error.is_none(), "{:?}", answered.error);
            let mut waiting = waiting.into_iter();
            let dropped = waiting.next().unwrap().join().unwrap();
            assert!(
                dropped.is_err(),
                "a disconnected client's waiting sample is dropped, not answered"
            );
            for queued in waiting {
                let response = queued.join().unwrap().expect("answered");
                assert!(response.error.is_none(), "{:?}", response.error);
                assert_eq!(
                    response.result.unwrap()["rgba"],
                    answered.result.as_ref().unwrap()["rgba"]
                );
            }
        });
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// On a real RAW file through Presence: every sample the point worker answers equals the full
    /// linear render at its pixel, far corner included. Run in release with LUXFORGE_RAW_FIXTURE
    /// pointing to a private qualified NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_raw_sample_through_presence_off_the_owner_equals_the_render() {
        let path =
            PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("LUXFORGE_RAW_FIXTURE"));
        let catalog = temp("point-worker-raw.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let state = import_asset(&owner, client, &path);
        let asset = state["asset"]["id"].clone();
        let asset_id = AssetId::parse(asset.as_str().unwrap()).unwrap();
        for (revision, fields) in [
            (0, json!({"clarity": 60.0})),
            (1, json!({"clarity": 60.0, "dehaze": 30.0})),
        ] {
            presence(
                &owner,
                client,
                &asset,
                revision,
                &format!("presence-{revision}"),
                fields.clone(),
            );
            let frame = rendered(&owner, PreviewRequest::new(client, asset_id.clone()));
            let mut state = 0x2545_f491_4f6c_dd1d_u64;
            let mut points: Vec<(u32, u32)> = (0..20)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (
                        (state % u64::from(frame.width)) as u32,
                        ((state >> 32) % u64::from(frame.height)) as u32,
                    )
                })
                .collect();
            points.push((frame.width - 1, frame.height - 1));
            for (x, y) in &points {
                let sampled = sample_at(
                    &owner,
                    client,
                    "raw",
                    json!({"asset_id": asset, "x": x, "y": y}),
                );
                assert!(sampled.error.is_none(), "{:?}", sampled.error);
                assert_eq!(
                    sampled.result.unwrap()["rgba"],
                    json!(frame.pixel(*x, *y).unwrap()),
                    "{fields} at ({x}, {y})"
                );
            }
            println!(
                "{fields}: {} samples off the owner equal the render",
                points.len()
            );
        }
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }
}
