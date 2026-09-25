//! One thread owns the catalog and every client session; all clients call it in turn. The owner
//! loop finds each request's method in the one method table, calls its handler and records the
//! events the call announced; the owner handlers the table names live here.
use super::{
    ApiEvent, ApiRequest, ApiResponse, ClientAuthority, ClientSession, EventsResult,
    methods::{self, Route},
    params::{Envelope, NoParams, host_params},
};
use crate::{
    AnalysisPlan, AnalysisSelection, AssetId, DraftId, EditorService, EditorState, EntryId, Error,
    ErrorKind, HostConfig, JobId, JobStatus, MaskOverlayRequest, ModuleRegistry, Preparation,
    PreparationNeeds, PreviewJob, ProxyBounds,
    activity::{ActivityBoard, ActivitySpec, Outcome},
    analysis::{AnalysisIdentity, AnalysisJob, AnalysisQueue, AnalysisRead, AnalysisStore, Report},
    artifacts::{self, ArtifactId, ArtifactRead, Collected, Collection, VerifiedArtifact},
    capabilities::{
        host::{CapabilityHost, announce_once},
        jobs::Origin,
    },
    editor::{FilePreparation, PreparedFile, RawDevelopment, SourceSignature},
    source::{PlaneGate, RawPrepared},
};
use requests::{RequestKey, RequestTable};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
};
#[cfg(test)]
use std::{thread, time::Duration};

#[cfg(test)]
mod artifact_tests;
pub(super) mod capability;
mod requests;

const EVENT_CAPACITY: usize = 256;
const SOURCE_QUEUE_CAPACITY: usize = 8;
const SOURCE_RESULT_CAPACITY: usize = 64;
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
    /// A capability lane finished a job. Like the analysis worker, the lane posts it into this
    /// channel, so nothing polls.
    CapabilityFinished {
        job_id: JobId,
        result: Result<Value, Error>,
    },
    /// How many capability lane threads have started, for tests that prove discovery is inert.
    #[cfg(test)]
    CapabilityThreads(SyncSender<usize>),
    Disconnect(ClientId),
    Stop,
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
    /// Also fill one mask's coverage grid beside the rendered frame, which the worker returns with
    /// it. The owner validates it against the stack the job will render, so a mask or component the
    /// stack does not hold refuses the request rather than producing a frame with no overlay.
    pub mask_overlay: Option<MaskOverlayRequest>,
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
            mask_overlay: None,
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
    /// Fill one mask's coverage grid beside the frame, so the canvas can draw the mask overlay
    /// without a second render.
    pub fn mask_overlay(mut self, request: MaskOverlayRequest) -> Self {
        self.mask_overlay = Some(request);
        self
    }
}

/// What makes two source jobs the same work, so a second request joins the first. `signature` is
/// the original's file signature, absent for a job that reads artifacts only; `artifacts` are the
/// identities the job reads and verifies after any source work, sorted.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SourceFlightKey {
    path: PathBuf,
    signature: Option<SourceSignature>,
    expected_fingerprint: Option<String>,
    gains_bits: Option<[u32; 3]>,
    artifacts: Vec<ArtifactId>,
}

enum SourceTaskKind {
    /// Read and decode the original, developing a known RAW at the requested entry gains.
    File(Option<Box<FilePreparation>>),
    Develop(RawDevelopment),
    /// The asset's source is already prepared; only its artifacts need reading.
    Artifacts(AssetId),
    /// Remove the object files the owner's collection left unrecorded, and stale staged files.
    Collect(Collection),
}

enum SourceResult {
    File(PreparedFile, Vec<VerifiedArtifact>),
    Develop(RawDevelopment, RawPrepared, Vec<VerifiedArtifact>),
    Artifacts(AssetId, Vec<VerifiedArtifact>),
    Collected(Collected),
}

struct SourceTask {
    id: JobId,
    key: SourceFlightKey,
    cancelled: Arc<AtomicBool>,
    kind: SourceTaskKind,
    /// Read and verified after the source work of a preparation, so one job readies a whole stack.
    artifacts: Vec<ArtifactRead>,
}

enum SourceState {
    Queued,
    Preparing,
    Ready(Box<EditorState>),
    /// A job whose result is not an asset: a collection.
    Finished(Value),
    Failed(Error),
}

/// What a completed source job leaves for its clients to read.
enum Completed {
    /// A prepared asset, and whether this job created it.
    Asset(Box<EditorState>, bool),
    Value(Value),
}

struct SourceJob {
    key: SourceFlightKey,
    import_request_id: Option<String>,
    clients: HashSet<ClientId>,
    cancelled: Arc<AtomicBool>,
    state: SourceState,
    sensor: Option<Weak<lightwell_raw::RawSource>>,
}

struct SourceJobs {
    sender: SyncSender<SourceTask>,
    jobs: HashMap<JobId, SourceJob>,
    active: HashMap<SourceFlightKey, JobId>,
    completed: VecDeque<JobId>,
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

impl SourceJobs {
    fn new(sender: SyncSender<SourceTask>, gate: Arc<PlaneGate>) -> Self {
        Self {
            sender,
            jobs: HashMap::new(),
            active: HashMap::new(),
            completed: VecDeque::new(),
            gate,
        }
    }

    fn enqueue(
        &mut self,
        client: ClientId,
        path: PathBuf,
        target: Option<FilePreparation>,
        artifacts: Vec<ArtifactRead>,
    ) -> Result<JobId, Error> {
        self.enqueue_file(client, path, target, artifacts)
    }

    /// Prepare an original and its artifacts for the requested entry.
    fn enqueue_file(
        &mut self,
        client: ClientId,
        path: PathBuf,
        target: Option<FilePreparation>,
        artifacts: Vec<ArtifactRead>,
    ) -> Result<JobId, Error> {
        let (canonical, signature) = EditorService::request_signature(&path)?;
        let key = SourceFlightKey {
            path: canonical,
            signature: Some(signature),
            expected_fingerprint: target.as_ref().map(|target| target.fingerprint.clone()),
            gains_bits: target
                .as_ref()
                .and_then(|target| target.raw.as_ref())
                .map(|raw| raw.gains.map(f32::to_bits)),
            artifacts: flight_artifacts(&artifacts),
        };
        let kind = SourceTaskKind::File(target.map(Box::new));
        self.submit(client, key, kind, artifacts, None, true)
    }

    /// Read and verify an asset's artifacts when its source is already prepared.
    fn enqueue_artifacts(
        &mut self,
        client: ClientId,
        asset_id: AssetId,
        artifacts: Vec<ArtifactRead>,
    ) -> Result<JobId, Error> {
        let key = SourceFlightKey {
            path: asset_id.as_str().into(),
            signature: None,
            expected_fingerprint: None,
            gains_bits: None,
            artifacts: flight_artifacts(&artifacts),
        };
        let kind = SourceTaskKind::Artifacts(asset_id);
        self.submit(client, key, kind, artifacts, None, true)
    }

    /// A collection: work a client asked for explicitly, which another request never joins.
    fn enqueue_maintenance(
        &mut self,
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
        self.submit(client, key, kind, Vec::new(), None, false)
    }

    /// Queue one task on the source worker, or join the queued or running job for the same key
    /// when `shared`. A full queue is a `resource-limit` and changes nothing.
    fn submit(
        &mut self,
        client: ClientId,
        key: SourceFlightKey,
        kind: SourceTaskKind,
        artifacts: Vec<ArtifactRead>,
        sensor: Option<Weak<lightwell_raw::RawSource>>,
        shared: bool,
    ) -> Result<JobId, Error> {
        if shared && let Some(id) = self.active.get(&key) {
            let job = self.jobs.get_mut(id).expect("active job is indexed");
            job.clients.insert(client);
            return Ok(id.clone());
        }
        let id = JobId::new();
        let cancelled = Arc::new(AtomicBool::new(false));
        let task = SourceTask {
            id: id.clone(),
            key: key.clone(),
            cancelled: cancelled.clone(),
            kind,
            artifacts,
        };
        match self.sender.try_send(task) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    "source preparation queue is full",
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(Error::new(
                    ErrorKind::Protocol,
                    "source preparation worker stopped",
                ));
            }
        }
        if shared {
            self.active.insert(key.clone(), id.clone());
        }
        self.jobs.insert(
            id.clone(),
            SourceJob {
                key,
                import_request_id: None,
                clients: HashSet::from([client]),
                cancelled,
                state: SourceState::Queued,
                sensor,
            },
        );
        Ok(id)
    }

    fn enqueue_development(
        &mut self,
        client: ClientId,
        request: RawDevelopment,
        artifacts: Vec<ArtifactRead>,
    ) -> Result<JobId, Error> {
        let key = SourceFlightKey {
            path: request.asset_id.as_str().into(),
            signature: Some(request.signature.clone()),
            expected_fingerprint: Some(request.fingerprint.clone()),
            gains_bits: Some(request.gains.map(f32::to_bits)),
            artifacts: flight_artifacts(&artifacts),
        };
        if let Some(id) = self.active.get(&key) {
            let job = self.jobs.get_mut(id).expect("active development indexed");
            job.clients.insert(client);
            return Ok(id.clone());
        }
        let sensor = Arc::downgrade(&request.sensor);
        let mut distinct = Vec::<&Weak<lightwell_raw::RawSource>>::new();
        for job in self.jobs.values() {
            if let Some(existing) = job
                .sensor
                .as_ref()
                .filter(|sensor| sensor.strong_count() > 0)
                && !distinct.iter().any(|seen| Weak::ptr_eq(seen, existing))
            {
                distinct.push(existing);
            }
        }
        if distinct.len() >= MAX_QUEUED_MOSAICS
            && !distinct
                .iter()
                .any(|existing| Weak::ptr_eq(existing, &sensor))
        {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                "RAW mosaic queue is full; retry after the active development",
            ));
        }
        let kind = SourceTaskKind::Develop(request);
        self.submit(client, key, kind, artifacts, Some(sensor), true)
    }

    fn ready(&mut self, client: ClientId, state: EditorState) -> Result<JobId, Error> {
        let signature = EditorService::request_signature(&state.asset.locator)?.1;
        let id = JobId::new();
        self.jobs.insert(
            id.clone(),
            SourceJob {
                key: SourceFlightKey {
                    path: state.asset.locator.clone(),
                    signature: Some(signature),
                    expected_fingerprint: Some(state.asset.fingerprint.clone()),
                    gains_bits: None,
                    artifacts: Vec::new(),
                },
                import_request_id: None,
                clients: HashSet::from([client]),
                cancelled: Arc::new(AtomicBool::new(false)),
                state: SourceState::Ready(Box::new(state)),
                sensor: None,
            },
        );
        self.completed.push_back(id.clone());
        while self.completed.len() > SOURCE_RESULT_CAPACITY {
            if let Some(old) = self.completed.pop_front() {
                self.jobs.remove(&old);
            }
        }
        Ok(id)
    }

    fn mark_import(&mut self, id: &JobId, request_id: &str) {
        if let Some(job) = self.jobs.get_mut(id)
            && job.import_request_id.is_none()
        {
            job.import_request_id = Some(request_id.to_owned());
        }
    }

    /// `queued`, `running` (preparing), `ready`, `failed`. Never `cancelled`: a client that cancels
    /// leaves the job (see [`Self::cancel`]) and cannot read it again.
    fn status(&self, client: ClientId, id: &JobId) -> Result<Value, Error> {
        let job = self
            .jobs
            .get(id)
            .filter(|job| job.clients.contains(&client))
            .ok_or_else(|| {
                Error::new(ErrorKind::Validation, "unknown source job for this client")
            })?;
        Ok(match &job.state {
            SourceState::Queued => json!({"job_id":id,"status":JobStatus::Queued}),
            SourceState::Preparing => json!({"job_id":id,"status":JobStatus::Running}),
            SourceState::Ready(asset) => {
                json!({"job_id":id,"status":JobStatus::Ready,"asset":asset})
            }
            SourceState::Finished(result) => {
                json!({"job_id":id,"status":JobStatus::Ready,"result":result})
            }
            SourceState::Failed(error) => {
                json!({"job_id":id,"status":JobStatus::Failed,"error":{"code":error.kind.code(),"message":error.detail}})
            }
        })
    }

    /// Leave the job: the calling client will never read it again, whatever becomes of the work.
    /// The last client leaving stops the underlying task, if it has not already finished.
    fn cancel(&mut self, client: ClientId, id: &JobId) -> Result<Value, Error> {
        let job = self.jobs.get_mut(id).ok_or_else(|| {
            Error::new(ErrorKind::Validation, "unknown source job for this client")
        })?;
        if !job.clients.remove(&client) {
            return Err(Error::new(
                ErrorKind::Validation,
                "unknown source job for this client",
            ));
        }
        if job.clients.is_empty() {
            job.cancelled.store(true, Ordering::Relaxed);
            self.gate.wake();
            let key = job.key.clone();
            if self.active.get(&key).is_some_and(|active| active == id) {
                self.active.remove(&key);
            }
        }
        Ok(json!({"job_id":id,"status":JobStatus::Cancelled}))
    }

    fn disconnect(&mut self, client: ClientId) {
        let mut detached = Vec::new();
        for (id, job) in &mut self.jobs {
            if job.clients.remove(&client) && job.clients.is_empty() {
                job.cancelled.store(true, Ordering::Relaxed);
                detached.push((id.clone(), job.key.clone()));
            }
        }
        if !detached.is_empty() {
            self.gate.wake();
        }
        for (id, key) in detached {
            if self.active.get(&key).is_some_and(|active| active == &id) {
                self.active.remove(&key);
            }
        }
    }

    /// Record a finished job's outcome: ready, finished or failed.
    fn complete(&mut self, id: &JobId, state: SourceState) {
        let Some(job) = self.jobs.get_mut(id) else {
            return;
        };
        if self.active.get(&job.key).is_some_and(|active| active == id) {
            self.active.remove(&job.key);
        }
        job.state = state;
        job.sensor = None;
        self.completed.push_back(id.clone());
        while self.completed.len() > SOURCE_RESULT_CAPACITY {
            if let Some(old) = self.completed.pop_front() {
                self.jobs.remove(&old);
            }
        }
    }
}

/// Queue one source job that prepares exactly what `needs` names — the asset's original, its RAW
/// development at the named gains and the named artifacts — and nothing re-derived from the request
/// that was refused. An original the cache does not hold is prepared and developed at those gains
/// in the same job; one it holds is redeveloped only when its planes do not hold them; artifacts
/// that became ready since they were named are left out. Needs with nothing left to prepare answer
/// with a job that is already ready.
fn queue_preparation(
    service: &EditorService,
    jobs: &mut SourceJobs,
    client: ClientId,
    needs: &PreparationNeeds,
) -> Result<JobId, Error> {
    let asset_id = &needs.asset_id;
    let artifacts = service.artifact_reads(&needs.artifacts)?;
    let Some(state) = service.cached_state(asset_id)? else {
        let state = service.state(asset_id)?;
        let id = jobs.enqueue_file(
            client,
            state.asset.locator,
            Some(service.file_preparation(asset_id, Some(&needs.entry_id))?),
            artifacts,
        )?;
        service.evict_development();
        return Ok(id);
    };
    let development = match needs.gains {
        Some(gains) => service.raw_development(asset_id, gains)?,
        None => None,
    };
    if let Some(request) = development {
        let id = jobs.enqueue_development(client, request, artifacts)?;
        service.evict_development();
        return Ok(id);
    }
    if artifacts.is_empty() {
        jobs.ready(client, state)
    } else {
        jobs.enqueue_artifacts(client, state.asset.id, artifacts)
    }
}

/// Where a test holds the source worker: after a task's activity has begun and before any of its
/// work, so the test can read the task as running for as long as it needs to. Outside tests it is
/// empty and holds nothing.
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
        SourceTaskKind::File(_) => ActivitySpec {
            kind: "source.prepare",
            label: "Preparing original",
            detail: task
                .key
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            asset_id: None,
            job_id: Some(task.id.to_string()),
        },
        SourceTaskKind::Develop(request) => ActivitySpec {
            kind: "source.develop",
            label: "Developing RAW",
            detail: request.file_name.clone(),
            asset_id: Some(request.asset_id.clone()),
            job_id: Some(task.id.to_string()),
        },
        SourceTaskKind::Artifacts(asset_id) => ActivitySpec {
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

/// Read and verify every listed artifact on the source worker, stopping at the first that is
/// missing, corrupt or cancelled.
fn read_artifacts(
    reads: &[ArtifactRead],
    cancel: &AtomicBool,
) -> Result<Vec<VerifiedArtifact>, Error> {
    reads
        .iter()
        .map(|read| artifacts::read_verified(read, cancel))
        .collect()
}

fn source_worker(
    receiver: Receiver<SourceTask>,
    owner: SyncSender<OwnerMessage>,
    gate: Arc<PlaneGate>,
    board: Arc<ActivityBoard>,
    hold: SourceHold,
) {
    while let Ok(task) = receiver.recv() {
        if task.cancelled.load(Ordering::Relaxed) {
            let _ = owner.send(OwnerMessage::SourceComplete(
                task.id,
                Box::new(Err(Error::new(ErrorKind::Conflict, "source job cancelled"))),
            ));
            continue;
        }
        // A previous RAW result/cache or active/pending preview may still pin its large float
        // planes. Wait on the worker, never the catalog owner, before another source allocation:
        // the last release or a stop wakes it. Artifact work allocates no planes and never waits.
        if matches!(
            task.kind,
            SourceTaskKind::File(_) | SourceTaskKind::Develop(_)
        ) {
            gate.wait_released(&task.cancelled);
        }
        if task.cancelled.load(Ordering::Relaxed) {
            let _ = owner.send(OwnerMessage::SourceComplete(
                task.id,
                Box::new(Err(Error::new(ErrorKind::Conflict, "source job cancelled"))),
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
        hold.wait();
        let mut result = match task.kind {
            SourceTaskKind::File(target) => EditorService::prepare_file_cancel(
                &task.key.path,
                target.as_deref(),
                &task.cancelled,
            )
            .and_then(|prepared| {
                if Some(&prepared.signature) != task.key.signature.as_ref() {
                    return Err(Error::new(
                        ErrorKind::Conflict,
                        "source changed after job was queued",
                    ));
                }
                if task
                    .key
                    .expected_fingerprint
                    .as_deref()
                    .is_some_and(|expected| expected != prepared.fingerprint)
                {
                    return Err(Error::new(
                        ErrorKind::SourceUnavailable,
                        "original source fingerprint changed",
                    ));
                }
                let verified = read_artifacts(&task.artifacts, &task.cancelled)?;
                Ok(SourceResult::File(prepared, verified))
            }),
            SourceTaskKind::Develop(request) => RawPrepared::develop(
                request.sensor.clone(),
                request.fingerprint.clone(),
                request.gains,
                &task.cancelled,
            )
            .and_then(|developed| {
                let verified = read_artifacts(&task.artifacts, &task.cancelled)?;
                Ok(SourceResult::Develop(request, developed, verified))
            }),
            SourceTaskKind::Artifacts(asset_id) => read_artifacts(&task.artifacts, &task.cancelled)
                .map(|verified| SourceResult::Artifacts(asset_id, verified)),
            SourceTaskKind::Collect(collection) => {
                artifacts::collect_files(&collection, &task.cancelled).map(SourceResult::Collected)
            }
        };
        if let Ok(prepared) = &mut result {
            let raw = match prepared {
                SourceResult::File(file, _) => match &mut file.source {
                    crate::source::PreparedSource::Raw(raw) => Some(raw),
                    _ => None,
                },
                SourceResult::Develop(_, raw, _) => Some(raw),
                SourceResult::Artifacts(..) | SourceResult::Collected(_) => None,
            };
            if let Some(linear) = raw.and_then(|raw| raw.linear.as_mut()) {
                linear.hold(gate.lease());
            }
        }
        // The activity ends before the owner learns the result, so a client that reads the job as
        // ready or failed never still finds it listed as running. A cancelled preparation fails
        // with a conflict rather than `Cancelled`, so the job's own flag says which it was.
        activity.finish(match &result {
            Err(_) if task.cancelled.load(Ordering::Relaxed) => Outcome::Cancelled,
            result => Outcome::of(result),
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
        path: PathBuf,
        mutation: MutationRequest,
    }
}

host_params! {
    /// `job.status`, `job.adopt` and `job.cancel`.
    pub(super) struct JobParams {
        job_id: JobId,
    }
}

host_params! {
    /// `source.prepare`.
    pub(super) struct SourcePrepare {
        asset_id: AssetId,
        entry_id: Option<EntryId> = "historical entry; default current",
    }
}

host_params! {
    /// `events.since`.
    pub(super) struct EventsSince {
        after: u64,
    }
}

host_params! {
    /// `analysis.request`.
    pub(super) struct AnalysisRequest {
        asset_id: AssetId,
        target: AnalysisTarget,
    }
}

host_params! {
    /// `analysis.read` and `analysis.cancel`.
    pub(super) struct AnalysisJobParams {
        job_id: JobId,
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
        service.disable_sync_source();
        let (sender, receiver) = sync_channel(64);
        let (source_sender, source_receiver) = sync_channel(SOURCE_QUEUE_CAPACITY);
        let worker_sender = sender.clone();
        let gate = Arc::new(PlaneGate::default());
        let jobs = SourceJobs::new(source_sender, gate.clone());
        let worker_activity = activity.clone();
        let worker = std::thread::spawn(move || {
            source_worker(source_receiver, worker_sender, gate, worker_activity, hold)
        });
        // The analysis worker posts its results back through this same channel, so the owner needs
        // one clone of its own sender. The loop ends on `Stop`, never on the senders dropping.
        let completions = sender.clone();
        // The capability lanes post their finished jobs the same way.
        let capability_sender = sender.clone();
        let host = CapabilityHost::new(
            host,
            Arc::new(move |job_id, result| {
                let _ = capability_sender.send(OwnerMessage::CapabilityFinished { job_id, result });
            }),
            activity.clone(),
        );
        let owner_activity = activity.clone();
        let join = std::thread::spawn(move || {
            owner_loop(
                service,
                host,
                completions,
                receiver,
                jobs,
                worker,
                owner_activity,
            )
        });
        Ok((
            Self {
                sender,
                next_client: Arc::new(AtomicU64::new(1)),
                activity,
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

    /// How many capability lane threads the owner has started.
    #[cfg(test)]
    pub(crate) fn capability_threads(&self) -> usize {
        let (reply, answer) = sync_channel(1);
        self.sender
            .send(OwnerMessage::CapabilityThreads(reply))
            .expect("the owner is running");
        answer.recv().expect("the owner answered")
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
            .map_err(|_| Error::new(ErrorKind::Protocol, "catalog owner is unavailable"))?;
        receiver.recv().map_err(|_| {
            Error::new(
                ErrorKind::Protocol,
                "catalog owner stopped before responding",
            )
        })
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
            .map_err(|_| Error::new(ErrorKind::Protocol, "catalog owner is unavailable"))?;
        receiver
            .recv()
            .map_err(|_| Error::new(ErrorKind::Protocol, "catalog owner stopped before preview"))?
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

fn owner_loop(
    service: EditorService,
    host: CapabilityHost,
    completions: SyncSender<OwnerMessage>,
    receiver: Receiver<OwnerMessage>,
    jobs: SourceJobs,
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
        sessions: HashMap::new(),
        analyses: AnalysisStore::default(),
        queue,
        activity,
        latest_import: HashMap::new(),
        log: EventLog::default(),
        requests: RequestTable::default(),
        announced: Vec::new(),
    };
    while let Ok(message) = receiver.recv() {
        match message {
            OwnerMessage::Stop => break,
            OwnerMessage::Call(call) => {
                let response = owner.call(call.client, &call.request);
                let _ = call.response.send(response);
            }
            OwnerMessage::Preview { request, response } => {
                let _ = response.send(owner.preview(request));
            }
            OwnerMessage::Register { client, authority } => {
                owner.sessions.entry(client).or_default().authority = authority;
            }
            OwnerMessage::CapabilityFinished { job_id, result } => {
                owner
                    .host
                    .finished(&mut owner.service, &job_id, result, &mut owner.announced);
                owner.record_announced();
            }
            #[cfg(test)]
            OwnerMessage::CapabilityThreads(reply) => {
                let _ = reply.send(owner.host.lanes_started());
            }
            OwnerMessage::Disconnect(client) => owner.disconnect(client),
            OwnerMessage::SourceStarted(id) => {
                if let Some(job) = owner.jobs.jobs.get_mut(&id) {
                    job.state = SourceState::Preparing;
                }
            }
            OwnerMessage::SourceComplete(id, result) => owner.source_complete(&id, *result),
            OwnerMessage::AnalysisReady => {
                while let Some(outcome) = owner.queue.poll() {
                    if owner.analyses.awaits(&outcome.job_id) {
                        owner.analyses.complete(&outcome.job_id, outcome.result);
                    }
                }
            }
            OwnerMessage::AnalysisSubmitted { identity, report } => {
                owner.analyses.submit(*identity, *report);
            }
        }
    }
    for job in owner.jobs.jobs.values() {
        job.cancelled.store(true, Ordering::Relaxed);
    }
    owner.jobs.gate.wake();
    let Owner { jobs, mut host, .. } = owner;
    drop(jobs);
    // The lanes post into the receiver, so it goes first: a lane finishing as it stops is never
    // left waiting on a full channel while the owner waits for it.
    drop(receiver);
    host.shutdown();
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
    /// Append one event for a committed change, dropping the oldest beyond the log's capacity.
    fn record(&mut self, origin: &Origin) {
        self.sequence = self.sequence.saturating_add(1);
        if self.events.len() == EVENT_CAPACITY {
            self.events.pop_front();
        }
        self.events.push_back(ApiEvent {
            sequence: self.sequence,
            method: origin.method.clone(),
            request_id: origin.request_id.clone(),
        });
    }

    /// The events after `after`, and whether the log no longer holds some of them.
    fn since(&self, after: u64) -> EventsResult {
        let oldest = self
            .events
            .front()
            .map_or(self.sequence.saturating_add(1), |event| event.sequence);
        EventsResult {
            events: self
                .events
                .iter()
                .filter(|event| event.sequence > after)
                .cloned()
                .collect(),
            current_sequence: self.sequence,
            gap: after.saturating_add(1) < oldest,
        }
    }
}

/// One request an owner handler answers.
pub(super) struct Call<'a> {
    pub client: ClientId,
    pub request: &'a ApiRequest,
    /// The method and request identity an event this call announces names.
    pub origin: Origin,
}

/// Everything the catalog owner holds: the catalog, every client's session, the source and analysis
/// jobs, the capability host, the activity board, the event log and the request table. The owner
/// loop hands it every message; the owner handlers of the method table read and change it.
pub(super) struct Owner {
    pub(super) service: EditorService,
    pub(super) host: CapabilityHost,
    jobs: SourceJobs,
    sessions: HashMap<ClientId, ClientSession>,
    analyses: AnalysisStore,
    queue: AnalysisQueue,
    activity: Arc<ActivityBoard>,
    latest_import: HashMap<ClientId, JobId>,
    log: EventLog,
    requests: RequestTable,
    /// What the message being handled changed, each change once. The owner records them as events
    /// when the message is handled, before it answers.
    pub(super) announced: Vec<Origin>,
}

impl Owner {
    /// Answer one request: find its method, answer a retried revision-less mutation from the request
    /// table, otherwise call its handler, then record the events its changes announced.
    fn call(&mut self, client: ClientId, request: &ApiRequest) -> ApiResponse {
        let result = self.answer(client, request);
        self.record_announced();
        match result {
            Ok(value) => ApiResponse::success(request.id.clone(), self.log.sequence, value),
            Err(error) => ApiResponse::failure(request.id.clone(), self.log.sequence, error),
        }
    }

    fn answer(&mut self, client: ClientId, request: &ApiRequest) -> Result<Value, Error> {
        let method = methods::find(&self.service, &request.method).ok_or_else(|| {
            Error::new(
                ErrorKind::Protocol,
                format!("unknown method {}", request.method),
            )
        })?;
        // A retried mutation is answered from the request table, and its handler does not run
        // again, unless the catalog answers it: the service records an asset change's request
        // with the change. A settings write has a revision but no request log, so it is here too.
        let key = match (method.envelope(), method.route()) {
            (Envelope::Request, _) | (Envelope::Revision, Route::Owner(_)) => {
                RequestKey::of(&request.method, &request.params)
            }
            (Envelope::None, _) | (Envelope::Revision, Route::Service) => None,
        };
        if let Some(key) = &key
            && let Some(first) = self.requests.answered(key)?
        {
            return Ok(first);
        }
        let call = Call {
            client,
            request,
            origin: Origin::new(&request.method, &request.id),
        };
        let result = match method.route() {
            Route::Owner(handler) => handler(self, &call),
            Route::Service => {
                let session = self.sessions.entry(client).or_default();
                let result = method.serve(&mut self.service, session, &request.params);
                // A service answer says whether it changed anything: a no-op and a retry answered
                // from a store's request log did not.
                if result
                    .as_ref()
                    .is_ok_and(|value| methods::mutates(&method, Some(value)))
                {
                    announce_once(&mut self.announced, &call.origin);
                }
                result
            }
        };
        // A stack whose source or artifacts are not prepared queues exactly what the refusal
        // names and answers with the job to wait for, whichever handler evaluated it.
        let result = result.map_err(|error| self.prepare(client, error));
        match (result, key) {
            (Ok(mut value), Some(key)) => {
                self.requests.record(key, &mut value);
                Ok(value)
            }
            (result, _) => result,
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
            return Err(Error::new(
                ErrorKind::Validation,
                "a truncated preview renders a layer prefix its identity does not describe, so it cannot be analysed",
            ));
        }
        let job = self
            .service
            .preview_job(
                &request.asset_id,
                request.entry_id.as_ref(),
                request.layer_count,
                draft,
                request.proxy,
            )
            .map(|mut job| {
                job.analyse = request.analyse;
                job
            })
            .and_then(|job| match request.mask_overlay.clone() {
                // Validated against the stack the job will render, which is why it is applied here
                // and not copied in like the flags above.
                Some(overlay) => job.with_mask_overlay(overlay),
                None => Ok(job),
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
        match queue_preparation(&self.service, &mut self.jobs, client, needs) {
            Ok(job) => refused.with_preparation(Preparation::Queued(job)),
            Err(error) => error,
        }
    }

    /// Forget a client's session. A gone client releases its analysis interests exactly as a
    /// cancel does; a job nobody else wants is dropped from the pending slot, or stopped on the
    /// worker within a chunk.
    fn disconnect(&mut self, client: ClientId) {
        self.sessions.remove(&client);
        for job_id in self.analyses.disconnect(client) {
            self.queue.withdraw(&job_id);
            self.analyses.cancel(&job_id);
        }
        self.latest_import.remove(&client);
        self.jobs.disconnect(client);
    }

    /// A source job finished on the worker: commit what it prepared for the clients still waiting,
    /// and record the import event when it created an asset.
    fn source_complete(&mut self, id: &JobId, result: Result<SourceResult, Error>) {
        let interested = self
            .jobs
            .jobs
            .get(id)
            .is_some_and(|job| !job.clients.is_empty());
        let service = &mut self.service;
        let outcome = if interested {
            result.and_then(|prepared| match prepared {
                SourceResult::File(file, verified) => {
                    let (state, created) = service.import_prepared(file)?;
                    service.adopt_artifacts(verified);
                    Ok(Completed::Asset(Box::new(state), created))
                }
                SourceResult::Develop(request, developed, verified) => {
                    let state = service.install_development(&request, developed)?;
                    service.adopt_artifacts(verified);
                    Ok(Completed::Asset(Box::new(state), false))
                }
                SourceResult::Artifacts(asset_id, verified) => {
                    service.adopt_artifacts(verified);
                    Ok(Completed::Asset(Box::new(service.state(&asset_id)?), false))
                }
                SourceResult::Collected(collected) => serde_json::to_value(collected)
                    .map(Completed::Value)
                    .map_err(|error| Error::new(ErrorKind::Internal, error.to_string())),
            })
        } else {
            Err(Error::new(ErrorKind::Conflict, "source job cancelled"))
        };
        // An import is announced when it commits an asset, under the request that asked for it.
        if matches!(outcome, Ok(Completed::Asset(_, true)))
            && let Some(request_id) = self
                .jobs
                .jobs
                .get(id)
                .and_then(|job| job.import_request_id.as_deref())
        {
            self.log.record(&Origin::new("catalog.import", request_id));
        }
        self.jobs.complete(
            id,
            match outcome {
                Ok(Completed::Asset(state, _)) => SourceState::Ready(state),
                Ok(Completed::Value(value)) => SourceState::Finished(value),
                Err(error) => SourceState::Failed(error),
            },
        );
        if !self.jobs.active.is_empty() {
            self.service.evict_development();
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
    params.mutation.validate()?;
    let (id, status) = match owner.service.cached_import(&params.path)? {
        Some(state) => (owner.jobs.ready(call.client, state)?, JobStatus::Ready),
        None => {
            let expected = owner.service.known_file_preparation(&params.path)?;
            let id = owner
                .jobs
                .enqueue(call.client, params.path, expected, Vec::new())?;
            owner.service.evict_development();
            (id, JobStatus::Queued)
        }
    };
    owner.jobs.mark_import(&id, &call.request.id);
    owner.latest_import.insert(call.client, id.clone());
    Ok(json!({"job_id": id, "status": status}))
}

pub(super) fn job_status(
    owner: &mut Owner,
    call: &Call<'_>,
    params: JobParams,
) -> Result<Value, Error> {
    owner.jobs.status(call.client, &params.job_id)
}

/// `job.adopt`: the ready result of this client's latest import becomes its current asset.
pub(super) fn job_adopt(
    owner: &mut Owner,
    call: &Call<'_>,
    params: JobParams,
) -> Result<Value, Error> {
    if owner.latest_import.get(&call.client) != Some(&params.job_id) {
        return Err(Error::new(
            ErrorKind::Conflict,
            "a newer import superseded this job",
        ));
    }
    let state = match owner.jobs.jobs.get(&params.job_id) {
        Some(job) if job.clients.contains(&call.client) => match &job.state {
            SourceState::Ready(state) => (**state).clone(),
            SourceState::Failed(error) => return Err(error.clone()),
            SourceState::Finished(_) => {
                return Err(Error::new(
                    ErrorKind::Validation,
                    "this source job is not an import",
                ));
            }
            SourceState::Queued | SourceState::Preparing => {
                return Err(Error::new(
                    ErrorKind::PreparationRequired,
                    "the import is still being prepared",
                )
                .with_preparation(Preparation::Queued(params.job_id.clone())));
            }
        },
        _ => {
            return Err(Error::new(
                ErrorKind::Validation,
                "unknown source job for this client",
            ));
        }
    };
    let session = owner.sessions.entry(call.client).or_default();
    session.preview.return_current();
    session.touch();
    owner.latest_import.remove(&call.client);
    Ok(json!({"asset": state, "session": session}))
}

pub(super) fn job_cancel(
    owner: &mut Owner,
    call: &Call<'_>,
    params: JobParams,
) -> Result<Value, Error> {
    if owner.latest_import.get(&call.client) == Some(&params.job_id) {
        owner.latest_import.remove(&call.client);
    }
    owner.jobs.cancel(call.client, &params.job_id)
}

pub(super) fn source_prepare(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SourcePrepare,
) -> Result<Value, Error> {
    let needs = owner
        .service
        .entry_needs(&params.asset_id, params.entry_id.as_ref())?;
    let id = queue_preparation(&owner.service, &mut owner.jobs, call.client, &needs)?;
    Ok(json!({"job_id": id, "status": JobStatus::Queued}))
}

/// `artifact.collect`: remove the collectable rows now, in one catalog transaction, and queue a
/// source job that removes their files, orphan files and stale staged files. Should the queue be
/// full, the removed rows' files are simply orphans the next collection removes. The collection is
/// a mutation the moment it is accepted, so other clients learn of it from the event log then.
pub(super) fn artifact_collect(
    owner: &mut Owner,
    call: &Call<'_>,
    params: Collect,
) -> Result<Value, Error> {
    params.mutation.validate()?;
    let collection = owner.service.plan_collection()?;
    let root = collection.root.clone();
    let id =
        owner
            .jobs
            .enqueue_maintenance(call.client, root, SourceTaskKind::Collect(collection))?;
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

/// `analysis.request`. Everything the owner does here is bookkeeping and `O(layers)` planning: a
/// state read, the cached verified source, the draft's plan and one compile to learn the output
/// stage. No frame is allocated and nothing is rasterized on this thread.
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
        source,
        registry,
        recipe,
        failure,
    } = owner.service.analysis_plan(&params.asset_id, selection)?;
    // An identical identity joins the job that already covers it, whether it is still running or
    // already holds a report: the same work is never done twice.
    let (job_id, fresh) = owner.analyses.request(identity.clone(), call.client);
    if fresh {
        match failure {
            // The effective recipe resolved but has no output stage the host can evaluate, so no
            // worker is started: the job is failed from the start and carries the reason.
            Some(error) => owner.analyses.fail(&job_id, error),
            None => {
                let source = source.expect("an evaluable stack carries its prepared source");
                if let Some(displaced) = owner.queue.submit(AnalysisJob {
                    job_id: job_id.clone(),
                    identity,
                    source,
                    registry,
                    recipe,
                }) {
                    owner.analyses.supersede(&displaced);
                }
            }
        }
    }
    let read = owner
        .analyses
        .state_of(&job_id, &owner.queue)
        .expect("the job was just opened or joined");
    let mut value = analysis_value(&read)?;
    value["job_id"] = json!(job_id);
    Ok(value)
}

/// `analysis.read`. A job this client never requested is not its own.
pub(super) fn analysis_read(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AnalysisJobParams,
) -> Result<Value, Error> {
    analysis_value(
        &owner
            .analyses
            .read(&params.job_id, call.client, &owner.queue)?,
    )
}

/// `analysis.cancel`. Dropping the last interest drops a pending job outright and stops a running
/// one within a chunk of its render or reduction; its cancelled outcome is discarded on arrival.
pub(super) fn analysis_cancel(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AnalysisJobParams,
) -> Result<Value, Error> {
    if owner.analyses.release(&params.job_id, call.client)? == crate::analysis::Release::Cancelled {
        owner.queue.withdraw(&params.job_id);
        owner.analyses.cancel(&params.job_id);
    }
    Ok(json!({"cancelled": true}))
}

/// One job's state as a client reads it. Only `ready` ever carries `report`, so pending, failed,
/// superseded and cancelled can never be mistaken for a valid but empty histogram.
fn analysis_value(read: &AnalysisRead<'_>) -> Result<Value, Error> {
    let mut value = json!({
        "status": read.status,
        "identity": read.identity,
    });
    if let Some(report) = read.report {
        value["report"] = methods::value(report)?;
    }
    if let Some(error) = read.error {
        value["error"] = json!({"code": error.kind.code(), "message": error.detail});
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewSource;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    use lightwell_testkit::fixtures::{jpeg as fixture, temp_path as temp};

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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let status = ok(
                owner,
                client,
                "status",
                "job.status",
                json!({"job_id": job_id}),
            );
            match status["status"].as_str() {
                Some("ready") => break,
                Some("queued" | "running") => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the import never became ready: {status}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                other => panic!("unexpected import job {other:?}: {status}"),
            }
        }
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

    /// Poll `analysis.read` until the job leaves `queued` or `running`. Nothing in the owner
    /// polls: this is the test standing in for a client that would rather be told, and it fails on
    /// a deadline instead of spinning forever.
    fn settled(owner: &OwnerHandle, client: ClientId, job_id: &Value) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let read = ok(
                owner,
                client,
                "read",
                "analysis.read",
                json!({"job_id": job_id}),
            );
            if !matches!(read["status"].as_str(), Some("queued" | "running")) {
                return read;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the analysis job never settled"
            );
            std::thread::yield_now();
        }
    }

    /// The exact report the contract says a target must produce: render that entry's own stack from
    /// the verified source and reduce the resulting raster. This is the independent reference the
    /// API answer is compared against, not a copy of the API's own arithmetic.
    fn expected_report(owner: &OwnerHandle, request: PreviewRequest) -> Value {
        let job = owner.preview_job(request).expect("a preview job");
        let raster = job
            .source
            .render(&job.registry, job.entry.snapshot.id.clone(), &job.recipe)
            .expect("a rendered frame");
        serde_json::to_value(crate::analysis::reduce_raster(&raster).expect("a reduction"))
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
        let start = std::time::Instant::now();
        loop {
            let status = request(owner, client, "job.status", json!({"job_id":id}));
            assert!(status.error.is_none(), "{:?}", status.error);
            let status = status.result.unwrap();
            match status["status"].as_str() {
                Some("ready" | "failed") => return status,
                Some("queued" | "running")
                    if start.elapsed() < std::time::Duration::from_secs(5) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
                other => panic!("source job did not complete: {other:?}: {status}"),
            }
        }
    }

    /// A source job's settled status, waiting as long as a photo-sized RAW development takes.
    fn wait_development(owner: &OwnerHandle, client: ClientId, id: &str) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            let status = ok(owner, client, "status", "job.status", json!({"job_id": id}));
            if !matches!(status["status"].as_str(), Some("queued" | "running")) {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the source job never settled: {status}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// On a real RAW file: `render.sample` names no entry, so it samples the session's selection.
    /// A historical entry whose white balance the developed planes do not hold is refused naming
    /// that entry's development rather than the current one's, and the sample converges after the
    /// one job that prepares it. Run with LIGHTWELL_RAW_FIXTURE pointing to a private qualified
    /// NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_sample_of_a_historical_raw_entry_prepares_that_entry_and_converges_after_one_job() {
        let path =
            PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("LIGHTWELL_RAW_FIXTURE"));
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

        // A custom white balance becomes current, and its development is prepared.
        ok(
            &owner,
            client,
            "temperature",
            "edit.set-raw-temperature",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 0, "request_id": "temperature", "actor": "test"},
                "kelvin": 3500.0,
            }),
        );
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

        // The Original, selected, holds the as-shot white balance the planes no longer hold.
        ok(
            &owner,
            client,
            "select",
            "preview.select",
            json!({"asset_id": asset, "entry_id": original}),
        );
        let refused = sample("historical").error.expect("a refusal");
        assert_eq!(refused.code, "preparation-required", "{refused:?}");
        let job = refused.job_id.expect("the job preparing the Original");
        assert_eq!(wait_development(&owner, client, &job)["status"], "ready");
        let sampled = sample("again");
        assert!(sampled.error.is_none(), "{:?}", sampled.error);
        assert_eq!(sampled.result.unwrap()["entry_id"], original);
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    #[ignore = "requires two private photo-sized RAW fixtures"]
    fn queued_distinct_raw_imports_release_first_float_before_second_development() {
        let first_path =
            PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE_A").expect("first RAW fixture"));
        let second_path =
            PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE_B").expect("second RAW fixture"));
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
        let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut a_ready = false;
        let mut b_ready = false;
        while !(a_ready && b_ready) && std::time::Instant::now() < until {
            let a_status = request(&owner, first, "job.status", json!({"job_id":a_id}))
                .result
                .unwrap();
            let b_status = request(&owner, second, "job.status", json!({"job_id":b_id}))
                .result
                .unwrap();
            assert_ne!(a_status["status"], "failed", "{a_status}");
            assert_ne!(b_status["status"], "failed", "{b_status}");
            a_ready = a_status["status"] == "ready";
            b_ready = b_status["status"] == "ready";
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            a_ready && b_ready,
            "second RAW import stalled behind retained first float"
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
            let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
            loop {
                assert!(
                    std::time::Instant::now() < until,
                    "evicted RAW never became sampleable"
                );
                let sample = request(
                    &owner,
                    client,
                    "render.sample",
                    json!({"asset_id":asset_id,"x":100,"y":100}),
                );
                match sample.error {
                    None => break,
                    Some(error) if error.code == "preparation-required" => {
                        let prepared = loop {
                            let status = request(
                                &owner,
                                client,
                                "job.status",
                                json!({"job_id":error.job_id}),
                            )
                            .result
                            .unwrap();
                            match status["status"].as_str() {
                                Some("ready" | "failed") => break status,
                                Some("queued" | "running") => {
                                    assert!(
                                        std::time::Instant::now() < until,
                                        "RAW preparation stalled: {status}"
                                    );
                                    std::thread::sleep(std::time::Duration::from_millis(20));
                                }
                                other => panic!("invalid source state: {other:?}"),
                            }
                        };
                        assert_eq!(prepared["status"], "ready", "{prepared}");
                    }
                    Some(error) => panic!("RAW sample failed: {error:?}"),
                }
            }
        }
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    #[ignore = "requires two private photo-sized RAW fixtures"]
    fn distinct_mosaic_admission_rejects_then_accepts_after_completion() {
        let first_path = PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE_A").unwrap());
        let second_path = PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE_B").unwrap());
        let cancel = AtomicBool::new(false);
        let first_sensor = Arc::new(
            lightwell_raw::RawSource::decode(
                Arc::from(std::fs::read(&first_path).unwrap()),
                &cancel,
            )
            .unwrap(),
        );
        let second_sensor = Arc::new(
            lightwell_raw::RawSource::decode(
                Arc::from(std::fs::read(&second_path).unwrap()),
                &cancel,
            )
            .unwrap(),
        );
        let request = |path: &Path, sensor: Arc<lightwell_raw::RawSource>| RawDevelopment {
            asset_id: AssetId::new(),
            signature: EditorService::request_signature(path).unwrap().1,
            fingerprint: "test".into(),
            gains: sensor.metadata().as_shot_gains,
            sensor,
            file_name: None,
        };
        let a = request(&first_path, first_sensor);
        let b = request(&second_path, second_sensor);
        let (sender, receiver) = sync_channel(SOURCE_QUEUE_CAPACITY);
        let mut jobs = SourceJobs::new(sender, Arc::default());
        let first = jobs
            .enqueue_development(ClientId(1), a.clone(), Vec::new())
            .unwrap();
        assert_eq!(
            jobs.enqueue_development(ClientId(2), a, Vec::new())
                .unwrap(),
            first
        );
        let refusal = jobs
            .enqueue_development(ClientId(3), b.clone(), Vec::new())
            .unwrap_err();
        assert_eq!(refusal.kind, ErrorKind::ResourceLimit);
        assert!(refusal.detail.starts_with("RAW mosaic queue is full"));
        drop(receiver.try_recv().unwrap());
        jobs.complete(
            &first,
            SourceState::Failed(Error::new(ErrorKind::Conflict, "finished")),
        );
        assert!(jobs.enqueue_development(ClientId(3), b, Vec::new()).is_ok());
    }

    #[test]
    fn pending_source_jobs_deduplicate_and_cancellation_keeps_other_waiters() {
        let (sender, _receiver) = sync_channel(SOURCE_QUEUE_CAPACITY);
        let mut jobs = SourceJobs::new(sender, Arc::default());
        let first = ClientId(1);
        let second = ClientId(2);
        let id = jobs.enqueue(first, fixture(), None, Vec::new()).unwrap();
        assert_eq!(
            jobs.enqueue(second, fixture(), None, Vec::new()).unwrap(),
            id
        );
        assert_eq!(jobs.cancel(first, &id).unwrap()["status"], "cancelled");
        assert_eq!(jobs.status(second, &id).unwrap()["status"], "queued");
        jobs.disconnect(second);
        assert!(jobs.jobs[&id].cancelled.load(Ordering::Relaxed));
        assert_ne!(
            jobs.enqueue(first, fixture(), None, Vec::new()).unwrap(),
            id
        );
    }

    #[test]
    fn changed_signature_starts_a_new_flight_instead_of_attaching_to_old_bytes() {
        let (sender, _receiver) = sync_channel(SOURCE_QUEUE_CAPACITY);
        let mut jobs = SourceJobs::new(sender, Arc::default());
        let path = temp("source-flight-changed.jpg");
        std::fs::copy(fixture(), &path).unwrap();
        let first = jobs
            .enqueue(ClientId(1), path.clone(), None, Vec::new())
            .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = 0;
        std::fs::write(&path, bytes).unwrap();
        let second = jobs
            .enqueue(ClientId(2), path.clone(), None, Vec::new())
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
        let expected = wait_source(&owner, client, second)["asset"]["asset"]["id"].clone();
        let stale = request(&owner, client, "job.adopt", json!({"job_id":first}));
        assert_eq!(stale.error.unwrap().code, "conflict");
        let adopted = request(&owner, client, "job.adopt", json!({"job_id":second}))
            .result
            .unwrap();
        assert_eq!(adopted["asset"]["asset"]["id"], expected);
        assert_eq!(adopted["session"]["preview"]["selection"], "current");
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(older).unwrap();
        std::fs::remove_file(newer).unwrap();
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
        assert_eq!(
            request(&owner, first, "job.cancel", json!({"job_id":first_id}))
                .result
                .unwrap()["status"],
            "cancelled"
        );
        assert_eq!(
            request(&owner, first, "job.status", json!({"job_id":first_id}))
                .error
                .unwrap()
                .code,
            "validation"
        );
        let ready = wait_source(&owner, second, second_id);
        assert_eq!(ready["status"], "ready");
        let asset = ready["asset"]["asset"]["id"].clone();
        let again = request(&owner, second, "catalog.import", import_params(fixture()))
            .result
            .unwrap();
        assert_eq!(
            again["status"], "ready",
            "a matching signature uses cached pixels"
        );
        assert_eq!(
            wait_source(&owner, second, again["job_id"].as_str().unwrap())["asset"]["asset"]["id"],
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
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
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
        let imported = loop {
            let status = call(viewer, "status", "job.status", json!({"job_id":job_id}));
            match status["status"].as_str() {
                Some("ready") => break status["asset"].clone(),
                Some("queued" | "running") => {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
                other => panic!("unexpected import job {other:?}: {status}"),
            }
        };
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
        assert_eq!(latest["session"]["preview"]["selection"], json!("current"));
        assert!(latest["generation"].is_u64());
        let selected = call(
            viewer,
            "preview",
            "preview.select",
            json!({"asset_id":asset,"entry_id":original}),
        );
        assert_eq!(selected["session"]["revision"], json!(2));
        assert_eq!(
            selected["session"]["preview"]["selection"],
            json!({"entry": original})
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
        assert_eq!(job.draft_revision, Some(1));
        assert_eq!(job.recipe.layers.len(), 1, "the draft's planned layer");
        let rendered = job
            .source
            .render(&job.registry, job.entry.snapshot.id.clone(), &job.recipe)
            .expect("a drafted frame");
        assert_eq!(rendered.pixel(0, 0), Some([200, 0, 0, 255]));

        // The stored stack is untouched, and a truncated job still applies to what is rendered.
        let stored = owner
            .preview_job(PreviewRequest::new(client, asset.clone()))
            .expect("the committed preview");
        assert!(stored.recipe.layers.is_empty());
        assert_eq!(stored.draft_revision, None);
        assert_ne!(
            stored
                .source
                .render(
                    &stored.registry,
                    stored.entry.snapshot.id.clone(),
                    &stored.recipe,
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
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
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
            settled_viewer["report"], reference,
            "the counts are the exact reduction of that entry's rendered frame"
        );
        assert!(settled_viewer.get("error").is_none());
        // The other client reads the very same shared result.
        assert_eq!(
            settled(&owner, agent, &second["job_id"])["report"],
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
        assert_eq!(historical["report"], reference);
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
            settled_current["report"], reference,
            "one replaced pixel moves the counts"
        );
        assert_eq!(
            settled_current["report"],
            expected_report(&owner, PreviewRequest::new(viewer, asset_id))
        );
        // The earlier result is untouched by the commit.
        assert_eq!(
            settled(&owner, viewer, &first["job_id"])["report"],
            reference
        );

        // A job this client never requested is not its own, and neither is one that does not exist.
        let foreign = owner.register();
        for (id, method) in [("foreign", "analysis.read"), ("fc", "analysis.cancel")] {
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
                "analysis.read",
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
                "analysis.read",
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
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
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
        let baseline = settled(&owner, client, &current["job_id"])["report"].clone();

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
            settled_draft["report"], baseline,
            "the drafted white pixel moves the counts"
        );
        assert_eq!(
            settled_draft["report"],
            expected_report(
                &owner,
                PreviewRequest::new(client, asset_id)
                    .draft(crate::DraftId::parse(draft_id.as_str().unwrap()).unwrap()),
            )
        );
        // Exactly one pixel changed: the red channel's population moves by one at two codes.
        let moved: u64 = settled_draft["report"]["r"]
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
            settled(&owner, client, &current["job_id"])["report"],
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
        let gate = crate::modules::RenderGate::open_gate();
        let mut registry = ModuleRegistry::builtin();
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
            ok(&owner, client, id, "analysis.read", json!({"job_id": job}))
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
            superseded.get("report").is_none(),
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
                "analysis.cancel",
                json!({"job_id": again["job_id"]}),
            ),
            json!({"cancelled": true})
        );
        let withdrawn = read(viewer, "read-queued", &again["job_id"]);
        assert_eq!(withdrawn["status"], json!("cancelled"), "{withdrawn}");
        assert!(
            withdrawn.get("report").is_none(),
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
                "analysis.read",
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
                "analysis.cancel",
                json!({"job_id": shared_viewer["job_id"]}),
            ),
            json!({"cancelled": true})
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
        assert!(finished["report"].is_object());
        let kept = settled(&owner, partner, &shared_partner["job_id"]);
        assert_eq!(kept["status"], json!("ready"), "{kept}");
        assert!(kept["report"].is_object());
        assert_eq!(
            read(viewer, "read-shared", &shared_viewer["job_id"])["report"],
            kept["report"],
            "one client's cancel did not invalidate the shared result"
        );

        // Every identity the first half superseded or cancelled is re-requestable and runs.
        for (id, entry) in [("retry-b", 1), ("retry-c", 2)] {
            let requested = entry_request(viewer, id, &entries[entry]);
            let ready = settled(&owner, viewer, &requested["job_id"]);
            assert_eq!(ready["status"], json!("ready"), "{ready}");
            assert!(ready["report"].is_object());
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
            let (owner, join) = OwnerHandle::start(&catalog).unwrap();
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
                "lightwell.pixel",
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
        assert!(requested.get("report").is_none());
        assert_eq!(requested["identity"]["width"], json!(0));
        assert_eq!(requested["identity"]["height"], json!(0));
        let read = ok(
            &owner,
            client,
            "read",
            "analysis.read",
            json!({"job_id": requested["job_id"]}),
        );
        assert_eq!(read["status"], json!("failed"));
        assert!(
            read.get("report").is_none(),
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
        let PreviewSource::Jpeg(jpeg) = &job.source else {
            panic!("a JPEG preview source")
        };
        let source = jpeg.rgba.clone();
        let mut queue = crate::PreviewQueue::default();
        queue.request(job);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let result = loop {
            if let Some(result) = queue.poll() {
                break result;
            }
            assert!(std::time::Instant::now() < deadline, "no preview arrived");
            std::thread::yield_now();
        };
        let raster = result.result.expect("a frame");
        let report = result.report.expect("the job asked for a report");
        assert_eq!(
            report,
            crate::analysis::reduce_raster(&raster).unwrap(),
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
        assert_eq!(requested["report"], encoded);

        // Nothing re-read or re-decoded the original: every path served the one cached decode.
        let after = owner
            .preview_job(PreviewRequest::new(desktop, asset_id))
            .expect("another preview job");
        assert!(
            matches!(&after.source, PreviewSource::Jpeg(jpeg) if Arc::ptr_eq(&source, &jpeg.rgba)),
            "the verified source cache served every request; no duplicate decode"
        );
        assert!(!after.analyse, "a plain preview asks for no reduction");
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    /// Read `activity.list` until `wanted` holds. The work under test is held at a gate, so what
    /// this waits for is a worker reaching that gate, never a race with how fast it works.
    fn listed(owner: &OwnerHandle, client: ClientId, wanted: impl Fn(&Value) -> bool) -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let list = ok(owner, client, "list", "activity.list", json!({}));
            if wanted(&list) {
                return list;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "activity.list never showed the work: {list}"
            );
            std::thread::yield_now();
        }
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
        let source_gate = crate::modules::RenderGate::open_gate();
        let render_gate = crate::modules::RenderGate::open_gate();
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while request(&owner, client, "job.status", json!({"job_id": job_id}))
            .result
            .unwrap()["status"]
            != json!("running")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the held job never read as running"
            );
            std::thread::yield_now();
        }
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while queue.is_busy() {
            let _ = queue.poll();
            assert!(std::time::Instant::now() < deadline, "no preview arrived");
            std::thread::yield_now();
        }

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
        assert!(notes.contains("job.status"), "{notes}");
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
        for method in ["analysis.request", "analysis.read", "analysis.cancel"] {
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
        assert_eq!(
            schema["methods"]["analysis.read"]["required"],
            json!(["job_id"])
        );
        assert_eq!(
            schema["methods"]["analysis.cancel"]["required"],
            json!(["job_id"])
        );
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
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
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

    /// Every method family that changes something without a revision takes the `{request_id,
    /// actor}` envelope, and a retry of it is answered from the owner's request table: the first
    /// answer comes back marked `deduplicated`, nothing changes again and no second event is
    /// recorded. The same `request_id` with other input is a conflict. A revisioned family,
    /// history here, answers its retry from the catalog's own request table the same way.
    #[test]
    fn a_retry_of_every_mutation_family_returns_the_first_answer_and_records_no_event() {
        let catalog = temp("retry-families.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
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
        let asset = ready["asset"]["asset"]["id"].clone();
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

    /// Every method `schema.list` lists is answered through the one table, and its schema is its
    /// parser: each declared field is accepted by name, all of them together raise no
    /// unknown-field error, and one field nothing declares is a validation error — naming it, for a
    /// host method, whose parameters are declared once as the struct it parses. The owner answers,
    /// so the service and owner handlers are covered alike, and the proof module's task is listed
    /// too. Values are `null`: the point is which names each parser knows, not what it accepts.
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
        // Methods that take nothing say so too.
        for name in [
            "schema.list",
            "catalog.list",
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
}
