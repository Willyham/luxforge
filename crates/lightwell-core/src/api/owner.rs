//! One thread owns the catalog and every client session; all clients call it in turn.
use super::{
    ApiEvent, ApiRequest, ApiResponse, ClientAuthority, ClientSession, EventsResult, methods,
};
use crate::{
    AnalysisPlan, AnalysisSelection, AssetId, DraftId, EditorService, EditorState, EntryId, Error,
    ErrorKind, HostConfig, JobId, MaskOverlayRequest, ModuleRegistry, PreviewJob, ProxyBounds,
    activity::{ActivityBoard, ActivitySpec, Outcome},
    analysis::{AnalysisIdentity, AnalysisJob, AnalysisQueue, AnalysisRead, AnalysisStore, Report},
    artifacts::{self, ArtifactId, ArtifactRead, Collected, Collection, VerifiedArtifact},
    capabilities::{host::CapabilityHost, jobs::Origin},
    editor::{PreparedFile, RawDevelopment, SourceSignature},
    source::RawPrepared,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[cfg(test)]
mod artifact_tests;

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
    /// The analysis worker finished a job. Nothing polls for this: the worker posts it into the
    /// owner's own channel, so the owner stays asleep until there is something to do.
    AnalysisFinished {
        job_id: JobId,
        result: Result<Box<Report>, Error>,
    },
    /// A report the desktop's preview worker already produced for this identity, so an API request
    /// for the same identity is a cache hit and no second render happens.
    AnalysisSubmitted {
        identity: Box<AnalysisIdentity>,
        report: Box<Report>,
    },
    SourceStarted(String),
    /// Boxed: a prepared source with its verified artifacts is several times larger than any
    /// other message, and every message on the owner's channel would otherwise carry that size.
    SourceComplete(String, Box<Result<SourceResult, Error>>),
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
    File,
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
    id: String,
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
    jobs: HashMap<String, SourceJob>,
    active: HashMap<SourceFlightKey, String>,
    completed: VecDeque<String>,
    next_id: u64,
}

/// The identities a job reads, sorted, for its flight key.
fn flight_artifacts(reads: &[ArtifactRead]) -> Vec<ArtifactId> {
    let mut ids: Vec<ArtifactId> = reads.iter().map(|read| read.id.clone()).collect();
    ids.sort();
    ids
}

impl SourceJobs {
    fn enqueue(
        &mut self,
        client: ClientId,
        path: PathBuf,
        expected_fingerprint: Option<String>,
        artifacts: Vec<ArtifactRead>,
    ) -> Result<String, Error> {
        let (canonical, signature) = EditorService::request_signature(&path)?;
        let key = SourceFlightKey {
            path: canonical,
            signature: Some(signature),
            expected_fingerprint,
            gains_bits: None,
            artifacts: flight_artifacts(&artifacts),
        };
        self.submit(client, key, SourceTaskKind::File, artifacts, None, true)
    }

    /// Read and verify an asset's artifacts when its source is already prepared.
    fn enqueue_artifacts(
        &mut self,
        client: ClientId,
        asset_id: AssetId,
        artifacts: Vec<ArtifactRead>,
    ) -> Result<String, Error> {
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
    ) -> Result<String, Error> {
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
    ) -> Result<String, Error> {
        if shared && let Some(id) = self.active.get(&key) {
            let job = self.jobs.get_mut(id).expect("active job is indexed");
            job.clients.insert(client);
            return Ok(id.clone());
        }
        let id = format!("source-job-{}", self.next_id);
        self.next_id = self.next_id.saturating_add(1);
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
    ) -> Result<String, Error> {
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

    fn ready(&mut self, client: ClientId, state: EditorState) -> Result<String, Error> {
        let signature = EditorService::request_signature(&state.asset.locator)?.1;
        let id = format!("source-job-{}", self.next_id);
        self.next_id = self.next_id.saturating_add(1);
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

    fn mark_import(&mut self, id: &str, request_id: &str) {
        if let Some(job) = self.jobs.get_mut(id)
            && job.import_request_id.is_none()
        {
            job.import_request_id = Some(request_id.to_owned());
        }
    }

    fn status(&self, client: ClientId, id: &str) -> Result<Value, Error> {
        let job = self
            .jobs
            .get(id)
            .filter(|job| job.clients.contains(&client))
            .ok_or_else(|| {
                Error::new(ErrorKind::Validation, "unknown source job for this client")
            })?;
        Ok(match &job.state {
            SourceState::Queued => json!({"job_id":id,"state":"queued"}),
            SourceState::Preparing => json!({"job_id":id,"state":"preparing"}),
            SourceState::Ready(asset) => json!({"job_id":id,"state":"ready","asset":asset}),
            SourceState::Finished(result) => json!({"job_id":id,"state":"ready","result":result}),
            SourceState::Failed(error) => {
                json!({"job_id":id,"state":"failed","error":{"code":error.kind.code(),"message":error.detail}})
            }
        })
    }

    fn cancel(&mut self, client: ClientId, id: &str) -> Result<Value, Error> {
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
            let key = job.key.clone();
            if self.active.get(&key).is_some_and(|active| active == id) {
                self.active.remove(&key);
            }
        }
        Ok(json!({"job_id":id,"state":"cancelled"}))
    }

    fn disconnect(&mut self, client: ClientId) {
        let mut detached = Vec::new();
        for (id, job) in &mut self.jobs {
            if job.clients.remove(&client) && job.clients.is_empty() {
                job.cancelled.store(true, Ordering::Relaxed);
                detached.push((id.clone(), job.key.clone()));
            }
        }
        for (id, key) in detached {
            if self.active.get(&key).is_some_and(|active| active == &id) {
                self.active.remove(&key);
            }
        }
    }

    /// Record a finished job's outcome: ready, finished or failed.
    fn complete(&mut self, id: &str, state: SourceState) {
        let Some(job) = self.jobs.get_mut(id) else {
            return;
        };
        if self.active.get(&job.key).is_some_and(|active| active == id) {
            self.active.remove(&job.key);
        }
        job.state = state;
        job.sensor = None;
        self.completed.push_back(id.to_owned());
        while self.completed.len() > SOURCE_RESULT_CAPACITY {
            if let Some(old) = self.completed.pop_front() {
                self.jobs.remove(&old);
            }
        }
    }
}

/// Queue one source job that prepares everything an entry's stack still needs: its original, its
/// RAW development and the artifacts it references, plus any artifact identities a refused request
/// named (a draft's effective recipe can reference one the entry does not). A stack with nothing
/// left to prepare answers with a job that is already ready.
fn queue_preparation(
    service: &EditorService,
    jobs: &mut SourceJobs,
    client: ClientId,
    asset_id: &AssetId,
    entry_id: Option<&EntryId>,
    requested: &[ArtifactId],
) -> Result<String, Error> {
    let artifacts = service.artifact_preparation(asset_id, entry_id, requested)?;
    if let Some(request) = service.raw_development(asset_id, entry_id)? {
        let id = jobs.enqueue_development(client, request, artifacts)?;
        service.evict_development();
        return Ok(id);
    }
    if let Some(state) = service.cached_state(asset_id)? {
        return if artifacts.is_empty() {
            jobs.ready(client, state)
        } else {
            jobs.enqueue_artifacts(client, state.asset.id, artifacts)
        };
    }
    let state = service.state(asset_id)?;
    let id = jobs.enqueue(
        client,
        state.asset.locator,
        Some(state.asset.fingerprint),
        artifacts,
    )?;
    service.evict_development();
    Ok(id)
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
        SourceTaskKind::File => ActivitySpec {
            kind: "source.prepare",
            label: "Preparing original",
            detail: task
                .key
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            asset_id: None,
            job_id: Some(task.id.clone()),
        },
        SourceTaskKind::Develop(request) => ActivitySpec {
            kind: "source.develop",
            label: "Developing RAW",
            detail: request.file_name.clone(),
            asset_id: Some(request.asset_id.clone()),
            job_id: Some(task.id.clone()),
        },
        SourceTaskKind::Artifacts(asset_id) => ActivitySpec {
            kind: "artifacts.read",
            label: "Verifying artifacts",
            detail: None,
            asset_id: Some(asset_id.clone()),
            job_id: Some(task.id.clone()),
        },
        SourceTaskKind::Collect(_) => ActivitySpec {
            kind: "artifacts.collect",
            label: "Removing unused artifacts",
            detail: None,
            asset_id: None,
            job_id: Some(task.id.clone()),
        },
    }
}

/// The artifact identities a `preparation-required` failure named in `data.artifacts`.
fn requested_artifacts(data: Option<&Value>) -> Vec<ArtifactId> {
    data.and_then(|data| data.get("artifacts"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|id| id.as_str().and_then(|id| ArtifactId::parse(id).ok()))
        .collect()
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
    live_planes: Arc<Mutex<Vec<Weak<Vec<f32>>>>>,
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
        // planes. Wait on the worker, never the catalog owner, before another source allocation.
        // Artifact work allocates no planes and never waits.
        let allocates_planes =
            matches!(task.kind, SourceTaskKind::File | SourceTaskKind::Develop(_));
        while allocates_planes && !task.cancelled.load(Ordering::Relaxed) {
            let live = {
                let mut planes = live_planes.lock().expect("source memory gate");
                planes.retain(|plane| plane.strong_count() != 0);
                !planes.is_empty()
            };
            if !live {
                break;
            }
            thread::sleep(Duration::from_millis(25));
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
        let result = match task.kind {
            SourceTaskKind::File => {
                EditorService::prepare_file_cancel(&task.key.path, &task.cancelled).and_then(
                    |prepared| {
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
                    },
                )
            }
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
        if let Ok(ref prepared) = result {
            let raw = match prepared {
                SourceResult::File(file, _) => match &file.source {
                    crate::source::PreparedSource::Raw(raw) => Some(raw),
                    _ => None,
                },
                SourceResult::Develop(_, raw, _) => Some(raw),
                SourceResult::Artifacts(..) | SourceResult::Collected(_) => None,
            };
            if let Some(raw) = raw.and_then(|raw| raw.linear.as_ref()) {
                live_planes
                    .lock()
                    .expect("source memory gate")
                    .push(raw.storage_weak());
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportParams {
    path: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobParams {
    job_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceParams {
    asset_id: AssetId,
    entry_id: Option<EntryId>,
}
fn parse_params<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, Error> {
    serde_json::from_value(value.clone())
        .map_err(|e| Error::new(ErrorKind::Validation, e.to_string()))
}

/// A method that takes no parameters accepts `{}` or no params at all.
fn no_params(method: &str, value: &Value) -> Result<(), Error> {
    match value {
        Value::Null => Ok(()),
        Value::Object(fields) if fields.is_empty() => Ok(()),
        _ => Err(Error::new(
            ErrorKind::Validation,
            format!("{method} takes no parameters"),
        )),
    }
}

/// `artifact.collect`: remove the collectable rows now, in one catalog transaction, and queue a
/// source job that removes their files, orphan files and stale staged files. Should the queue be
/// full, the removed rows' files are simply orphans the next collection removes.
fn queue_collection(
    service: &mut EditorService,
    jobs: &mut SourceJobs,
    client: ClientId,
    params: &Value,
) -> Result<String, Error> {
    no_params("artifact.collect", params)?;
    let collection = service.plan_collection()?;
    let root = collection.root.clone();
    jobs.enqueue_maintenance(client, root, SourceTaskKind::Collect(collection))
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
        let live_planes = Arc::new(Mutex::new(Vec::new()));
        let worker_activity = activity.clone();
        let worker = std::thread::spawn(move || {
            source_worker(
                source_receiver,
                worker_sender,
                live_planes,
                worker_activity,
                hold,
            )
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
        );
        let owner_activity = activity.clone();
        let join = std::thread::spawn(move || {
            owner_loop(
                service,
                host,
                completions,
                receiver,
                source_sender,
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
    mut service: EditorService,
    mut host: CapabilityHost,
    completions: SyncSender<OwnerMessage>,
    receiver: Receiver<OwnerMessage>,
    source_sender: SyncSender<SourceTask>,
    worker: JoinHandle<()>,
    activity: Arc<ActivityBoard>,
) {
    let mut jobs = SourceJobs {
        sender: source_sender,
        jobs: HashMap::new(),
        active: HashMap::new(),
        completed: VecDeque::new(),
        next_id: 1,
    };
    let mut sequence = 0u64;
    let mut events = VecDeque::with_capacity(EVENT_CAPACITY);
    let mut sessions: HashMap<ClientId, ClientSession> = HashMap::new();
    let mut store = AnalysisStore::default();
    // One analysis worker with one active and one replaceable pending job, globally. The worker
    // sends its report back into this loop; nothing here waits on it or polls for it.
    let mut queue = AnalysisQueue::new(Arc::new(move |job_id, result| {
        let _ = completions.send(OwnerMessage::AnalysisFinished {
            job_id,
            result: result.map(Box::new),
        });
    }));
    queue.set_activity(activity.clone());
    let mut latest_import: HashMap<ClientId, String> = HashMap::new();
    while let Ok(message) = receiver.recv() {
        match message {
            OwnerMessage::Stop => break,
            OwnerMessage::Register { client, authority } => {
                sessions.entry(client).or_default().authority = authority;
            }
            OwnerMessage::CapabilityFinished { job_id, result } => {
                let mut announced = Vec::new();
                host.finished(&mut service, &job_id, result, &mut announced);
                for origin in &announced {
                    record_event(&mut events, &mut sequence, origin);
                }
            }
            #[cfg(test)]
            OwnerMessage::CapabilityThreads(reply) => {
                let _ = reply.send(host.lanes_started());
            }
            OwnerMessage::Disconnect(client) => {
                sessions.remove(&client);
                // A gone client releases its analysis interests exactly as a cancel does; a job
                // nobody else wants is dropped from the pending slot, or its result is discarded
                // when it arrives from the worker.
                for job_id in store.disconnect(client) {
                    queue.drop_pending(&job_id);
                    store.cancel(&job_id);
                }
                latest_import.remove(&client);
                jobs.disconnect(client);
            }
            OwnerMessage::SourceStarted(id) => {
                if let Some(job) = jobs.jobs.get_mut(&id) {
                    job.state = SourceState::Preparing;
                }
            }
            OwnerMessage::SourceComplete(id, result) => {
                let result = *result;
                let interested = jobs
                    .jobs
                    .get(&id)
                    .is_some_and(|job| !job.clients.is_empty());
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
                if matches!(outcome, Ok(Completed::Asset(_, true))) {
                    sequence = sequence.saturating_add(1);
                    if events.len() == EVENT_CAPACITY {
                        events.pop_front();
                    }
                    if let Some(request_id) = jobs
                        .jobs
                        .get(&id)
                        .and_then(|job| job.import_request_id.clone())
                    {
                        events.push_back(ApiEvent {
                            sequence,
                            method: "catalog.import".into(),
                            request_id,
                        });
                    }
                }
                jobs.complete(
                    &id,
                    match outcome {
                        Ok(Completed::Asset(state, _)) => SourceState::Ready(state),
                        Ok(Completed::Value(value)) => SourceState::Finished(value),
                        Err(error) => SourceState::Failed(error),
                    },
                );
                if !jobs.active.is_empty() {
                    service.evict_development();
                }
            }
            OwnerMessage::AnalysisFinished { job_id, result } => {
                if store.awaits(&job_id) {
                    store.complete(&job_id, result.map(|report| *report));
                }
                queue.finished(&job_id);
            }
            OwnerMessage::AnalysisSubmitted { identity, report } => {
                store.submit(*identity, *report);
            }
            OwnerMessage::Preview { request, response } => {
                // A draft is session state, so the owner looks it up in the requesting client's own
                // session: naming another client's draft, or one that has ended, is a validation
                // error rather than a preview of someone else's gesture.
                let draft = match &request.draft {
                    None => Ok(None),
                    Some(draft_id) => sessions
                        .get(&request.client)
                        .and_then(|session| session.draft.as_ref())
                        .filter(|draft| &draft.draft_id == draft_id)
                        .map(Some)
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::Validation,
                                format!("unknown draft {draft_id} for this client"),
                            )
                        }),
                };
                let job = draft.and_then(|draft| {
                    if request.analyse && request.layer_count.is_some() {
                        return Err(Error::new(
                            ErrorKind::Validation,
                            "a truncated preview renders a layer prefix its identity does not describe, so it cannot be analysed",
                        ));
                    }
                    service
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
                            // Validated against the stack the job will render, which is why it is
                            // applied here and not copied in like the flags above.
                            Some(overlay) => job.with_mask_overlay(overlay),
                            None => Ok(job),
                        })
                });
                // A stack whose source is not prepared queues that preparation and answers with
                // the job to wait for, exactly as a JSON request does.
                let job = match job {
                    Err(error) if error.kind == ErrorKind::PreparationRequired => {
                        let queued = queue_preparation(
                            &service,
                            &mut jobs,
                            request.client,
                            &request.asset_id,
                            request.entry_id.as_ref(),
                            &requested_artifacts(error.data.as_deref()),
                        );
                        Err(queued.map_or_else(
                            |error| error,
                            |id| Error::new(ErrorKind::PreparationRequired, id),
                        ))
                    }
                    other => other,
                };
                let _ = response.send(job);
            }
            OwnerMessage::Call(call) => {
                if matches!(
                    call.request.method.as_str(),
                    "catalog.import"
                        | "job.status"
                        | "job.adopt"
                        | "job.cancel"
                        | "source.prepare"
                        | "artifact.collect"
                ) {
                    let answer: Result<Value, Error> = match call.request.method.as_str() {
                        "catalog.import" => parse_params::<ImportParams>(&call.request.params)
                            .and_then(|p| match service.cached_import(&p.path)? {
                                Some(state) => {
                                    jobs.ready(call.client, state).map(|id| (id, "ready"))
                                }
                                None => {
                                    let expected = service.known_fingerprint(&p.path)?;
                                    let id =
                                        jobs.enqueue(call.client, p.path, expected, Vec::new())?;
                                    service.evict_development();
                                    Ok((id, "queued"))
                                }
                            })
                            .map(|(id, state)| {
                                jobs.mark_import(&id, &call.request.id);
                                latest_import.insert(call.client, id.clone());
                                json!({"job_id":id,"state":state})
                            }),
                        "job.status" => parse_params::<JobParams>(&call.request.params)
                            .and_then(|p| jobs.status(call.client, &p.job_id)),
                        "job.adopt" => {
                            parse_params::<JobParams>(&call.request.params).and_then(|p| {
                                if latest_import.get(&call.client) != Some(&p.job_id) {
                                    return Err(Error::new(
                                        ErrorKind::Conflict,
                                        "a newer import superseded this job",
                                    ));
                                }
                                let state = match jobs.jobs.get(&p.job_id) {
                                    Some(job) if job.clients.contains(&call.client) => {
                                        match &job.state {
                                            SourceState::Ready(state) => (**state).clone(),
                                            SourceState::Failed(error) => return Err(error.clone()),
                                            SourceState::Finished(_) => {
                                                return Err(Error::new(
                                                    ErrorKind::Validation,
                                                    "this source job is not an import",
                                                ));
                                            }
                                            _ => {
                                                return Err(Error::new(
                                                    ErrorKind::PreparationRequired,
                                                    p.job_id,
                                                ));
                                            }
                                        }
                                    }
                                    _ => {
                                        return Err(Error::new(
                                            ErrorKind::Validation,
                                            "unknown source job for this client",
                                        ));
                                    }
                                };
                                let session = sessions.entry(call.client).or_default();
                                session.preview.return_current();
                                session.touch();
                                latest_import.remove(&call.client);
                                Ok(json!({"asset":state,"session":session}))
                            })
                        }
                        "job.cancel" => {
                            parse_params::<JobParams>(&call.request.params).and_then(|p| {
                                if latest_import.get(&call.client) == Some(&p.job_id) {
                                    latest_import.remove(&call.client);
                                }
                                jobs.cancel(call.client, &p.job_id)
                            })
                        }
                        "source.prepare" => parse_params::<SourceParams>(&call.request.params)
                            .and_then(|p| {
                                queue_preparation(
                                    &service,
                                    &mut jobs,
                                    call.client,
                                    &p.asset_id,
                                    p.entry_id.as_ref(),
                                    &[],
                                )
                            })
                            .map(|id| json!({"job_id":id,"state":"queued"})),
                        "artifact.collect" => queue_collection(
                            &mut service,
                            &mut jobs,
                            call.client,
                            &call.request.params,
                        )
                        .map(|id| json!({"job_id":id,"status":"queued"})),
                        _ => unreachable!(),
                    };
                    // A collection is a mutation the moment it is accepted, as an import is: other
                    // clients learn of it from the event log.
                    if answer.is_ok() && call.request.method.as_str() == "artifact.collect" {
                        sequence = sequence.saturating_add(1);
                        if events.len() == EVENT_CAPACITY {
                            events.pop_front();
                        }
                        events.push_back(ApiEvent {
                            sequence,
                            method: call.request.method.clone(),
                            request_id: call.request.id.clone(),
                        });
                    }
                    let response = match answer {
                        Ok(value) => ApiResponse::success(call.request.id, sequence, value),
                        Err(error) => ApiResponse::failure(call.request.id, sequence, error),
                    };
                    let _ = call.response.send(response);
                    continue;
                }
                // Discovery and dispatch resolve through the same registry-aware lookup.
                let method = methods::find(&service, &call.request.method);
                let response = match method {
                    // The methods the owner answers from its own state: the event log, the
                    // analysis jobs, whose store, worker slots and client drafts all live here,
                    // the capability host's settings and the activity board its workers publish to.
                    Some(method) if method.owner_answered() => {
                        let request = &call.request;
                        // A client's authority is part of its session, fixed when it registered.
                        let authority = sessions
                            .get(&call.client)
                            .map_or(ClientAuthority::Edit, |session| session.authority);
                        let mut announced = Vec::new();
                        if let Some(result) =
                            host.answer(&service, authority, request, &mut announced)
                        {
                            // The host announces exactly what a request changed: a committed
                            // write, a grant, a cancelled job or a deactivation. A read, a no-op
                            // and a retry answered from a request log change nothing.
                            for origin in &announced {
                                record_event(&mut events, &mut sequence, origin);
                            }
                            // A task samples its asset before it is queued; an unprepared source
                            // or artifact queues that preparation and answers with the job to wait
                            // for, as every other evaluating request does.
                            let result = match result {
                                Err(error) if error.kind == ErrorKind::PreparationRequired => {
                                    Err(prepare_current(
                                        &service,
                                        &mut jobs,
                                        call.client,
                                        &request.params,
                                        &error,
                                    ))
                                }
                                other => other,
                            };
                            let response = answer(request, sequence, result);
                            let _ = call.response.send(response);
                            continue;
                        }
                        match request.method.as_str() {
                            "activity.list" => {
                                answer(request, sequence, activity_list(&activity, &request.params))
                            }
                            "analysis.request" => {
                                let requested = analysis_request(
                                    &service,
                                    &sessions,
                                    &mut store,
                                    &mut queue,
                                    call.client,
                                    &request.params,
                                );
                                // A stack whose source or artifacts are not prepared queues that
                                // preparation and answers with the job to wait for, as every other
                                // evaluating request does.
                                let requested = match requested {
                                    Err(error) if error.kind == ErrorKind::PreparationRequired => {
                                        Err(prepare_analysis(
                                            &service,
                                            &mut jobs,
                                            call.client,
                                            &request.params,
                                            &error,
                                        ))
                                    }
                                    other => other,
                                };
                                answer(request, sequence, requested)
                            }
                            "analysis.read" => answer(
                                request,
                                sequence,
                                analysis_read(&store, call.client, &request.params),
                            ),
                            "analysis.cancel" => answer(
                                request,
                                sequence,
                                analysis_cancel(
                                    &mut store,
                                    &mut queue,
                                    call.client,
                                    &request.params,
                                ),
                            ),
                            "events.since" => event_response(request, sequence, &events),
                            _ => unrouted(request, sequence),
                        }
                    }
                    Some(method) => {
                        let session = sessions.entry(call.client).or_default();
                        let mut response =
                            methods::dispatch(&mut service, session, &call.request, sequence);
                        if response
                            .error
                            .as_ref()
                            .is_some_and(|e| e.code == ErrorKind::PreparationRequired.code())
                        {
                            let asset_id = call
                                .request
                                .params
                                .get("asset_id")
                                .cloned()
                                .ok_or_else(|| {
                                    Error::new(ErrorKind::Validation, "missing asset_id")
                                })
                                .and_then(|value| parse_params::<AssetId>(&value));
                            let entry_id = call
                                .request
                                .params
                                .get("entry_id")
                                .cloned()
                                .map(|value| parse_params::<EntryId>(&value))
                                .transpose();
                            let requested = requested_artifacts(
                                response.error.as_ref().and_then(|e| e.data.as_ref()),
                            );
                            let queued = asset_id.and_then(|id| {
                                entry_id.and_then(|entry| {
                                    queue_preparation(
                                        &service,
                                        &mut jobs,
                                        call.client,
                                        &id,
                                        entry.as_ref(),
                                        &requested,
                                    )
                                })
                            });
                            response = match queued {
                                Ok(id) => ApiResponse::failure(
                                    call.request.id.clone(),
                                    sequence,
                                    Error::new(ErrorKind::PreparationRequired, id),
                                ),
                                Err(error) => {
                                    ApiResponse::failure(call.request.id.clone(), sequence, error)
                                }
                            };
                        }
                        if response.error.is_none()
                            && methods::mutates(&method, response.result.as_ref())
                        {
                            sequence = sequence.saturating_add(1);
                            if events.len() == EVENT_CAPACITY {
                                events.pop_front();
                            }
                            events.push_back(ApiEvent {
                                sequence,
                                method: call.request.method.clone(),
                                request_id: call.request.id.clone(),
                            });
                            ApiResponse {
                                sequence,
                                ..response
                            }
                        } else {
                            response
                        }
                    }
                    None => {
                        let session = sessions.entry(call.client).or_default();
                        methods::dispatch(&mut service, session, &call.request, sequence)
                    }
                };
                let _ = call.response.send(response);
            }
        }
    }
    for job in jobs.jobs.values() {
        job.cancelled.store(true, Ordering::Relaxed);
    }
    drop(jobs);
    // The lanes post into the receiver, so it goes first: a lane finishing as it stops is never
    // left waiting on a full channel while the owner waits for it.
    drop(receiver);
    host.shutdown();
    let _ = worker.join();
}

/// Append one event for a committed change, dropping the oldest beyond the log's capacity.
fn record_event(events: &mut VecDeque<ApiEvent>, sequence: &mut u64, origin: &Origin) {
    *sequence = sequence.saturating_add(1);
    if events.len() == EVENT_CAPACITY {
        events.pop_front();
    }
    events.push_back(ApiEvent {
        sequence: *sequence,
        method: origin.method.clone(),
        request_id: origin.request_id.clone(),
    });
}

/// A method the table lists without a service handler and that no owner route answers. It is a
/// defect in this build, never a request a client got wrong, so it is refused as an internal error
/// naming the method rather than answered as some other method would be.
fn unrouted(request: &ApiRequest, sequence: u64) -> ApiResponse {
    answer(
        request,
        sequence,
        Err(Error::new(
            ErrorKind::Internal,
            format!(
                "{} has no service handler and no catalog-owner route",
                request.method
            ),
        )),
    )
}

/// Wrap one owner-answered result in the shared response envelope.
fn answer(request: &ApiRequest, sequence: u64, result: Result<Value, Error>) -> ApiResponse {
    match result {
        Ok(result) => ApiResponse::success(request.id.clone(), sequence, result),
        Err(error) => ApiResponse::failure(request.id.clone(), sequence, error),
    }
}

/// `activity.list`: one lock and a copy of at most 80 small entries. It takes no parameters; an
/// omitted `params` is accepted and a named one is refused, so a misspelt filter is never silently
/// ignored.
fn activity_list(board: &ActivityBoard, params: &Value) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {}
    if !params.is_null() {
        methods::params::<Params>(params)?;
    }
    serde_json::to_value(board.snapshot())
        .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
}

/// Which evaluated stack the caller wants analysed.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum AnalysisTarget {
    /// The asset's current entry.
    Current,
    /// One frozen historical entry, which a later commit never relabels.
    Entry { entry_id: EntryId },
    /// This client's own open draft, at the `draft_revision` it holds now.
    Draft { draft_id: DraftId },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnalysisJobParams {
    job_id: JobId,
}

/// `analysis.request`. Everything the owner does here is bookkeeping and `O(layers)` planning: a
/// state read, the cached verified source, the draft's plan and one compile to learn the output
/// stage. No frame is allocated and nothing is rasterized on this thread.
fn analysis_request(
    service: &EditorService,
    sessions: &HashMap<ClientId, ClientSession>,
    store: &mut AnalysisStore,
    queue: &mut AnalysisQueue,
    client: ClientId,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        asset_id: AssetId,
        target: AnalysisTarget,
    }
    let request: Params = methods::params(params)?;
    // A draft is session state, so it resolves from the calling client's own session: another
    // client's draft, or one that has ended, is simply not this session's.
    let held;
    let selection = match &request.target {
        AnalysisTarget::Current => AnalysisSelection::Current,
        AnalysisTarget::Entry { entry_id } => AnalysisSelection::Entry(entry_id),
        AnalysisTarget::Draft { draft_id } => {
            held = sessions
                .get(&client)
                .and_then(|session| session.draft.as_ref())
                .filter(|draft| &draft.draft_id == draft_id)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Validation,
                        format!("unknown draft {draft_id} for this client"),
                    )
                })?;
            AnalysisSelection::Draft(held)
        }
    };
    let AnalysisPlan {
        identity,
        source,
        registry,
        recipe,
        failure,
        artifacts,
    } = service.analysis_plan(&request.asset_id, selection)?;
    // An identical identity joins the job that already covers it, whether it is still running or
    // already holds a report: the same work is never done twice.
    let (job_id, fresh) = store.request(identity.clone(), client);
    if fresh {
        match failure {
            // The effective recipe resolved but has no output stage the host can evaluate, so no
            // worker is started: the job is failed from the start and carries the reason.
            Some(error) => store.fail(&job_id, error),
            None => {
                let source = source.expect("an evaluable stack carries its prepared source");
                if let Some(displaced) = queue.submit(AnalysisJob {
                    job_id: job_id.clone(),
                    identity,
                    source,
                    registry,
                    recipe,
                    artifacts,
                }) {
                    store.supersede(&displaced);
                }
            }
        }
    }
    let read = store
        .state_of(&job_id)
        .expect("the job was just opened or joined");
    let mut value = analysis_value(&read)?;
    value["job_id"] = json!(job_id);
    Ok(value)
}

/// Queue the preparation a refused `analysis.request` needs and return the error that names its
/// job, or the reason it could not be queued.
fn prepare_analysis(
    service: &EditorService,
    jobs: &mut SourceJobs,
    client: ClientId,
    params: &Value,
    refused: &Error,
) -> Error {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        asset_id: AssetId,
        target: AnalysisTarget,
    }
    let queued = methods::params::<Params>(params).and_then(|request| {
        let entry_id = match &request.target {
            AnalysisTarget::Entry { entry_id } => Some(entry_id),
            AnalysisTarget::Current | AnalysisTarget::Draft { .. } => None,
        };
        queue_preparation(
            service,
            jobs,
            client,
            &request.asset_id,
            entry_id,
            &requested_artifacts(refused.data.as_deref()),
        )
    });
    match queued {
        Ok(id) => Error::new(ErrorKind::PreparationRequired, id),
        Err(error) => error,
    }
}

/// Queue the preparation a refused owner-answered request needs to evaluate its asset's current
/// entry — a task sampling the data it discloses — and return the error that names its job, or the
/// reason it could not be queued.
fn prepare_current(
    service: &EditorService,
    jobs: &mut SourceJobs,
    client: ClientId,
    params: &Value,
    refused: &Error,
) -> Error {
    let queued = params
        .get("asset_id")
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Validation, "missing asset_id"))
        .and_then(|value| parse_params::<AssetId>(&value))
        .and_then(|asset_id| {
            queue_preparation(
                service,
                jobs,
                client,
                &asset_id,
                None,
                &requested_artifacts(refused.data.as_deref()),
            )
        });
    match queued {
        Ok(id) => Error::new(ErrorKind::PreparationRequired, id),
        Err(error) => error,
    }
}

/// `analysis.read`. A job this client never requested is not its own.
fn analysis_read(store: &AnalysisStore, client: ClientId, params: &Value) -> Result<Value, Error> {
    let params: AnalysisJobParams = methods::params(params)?;
    analysis_value(&store.read(&params.job_id, client)?)
}

/// `analysis.cancel`. Dropping the last interest drops a pending job outright; an active render is
/// not interrupted mid-way — it runs to completion on the worker and its result is discarded on
/// arrival, which costs nothing the render was not already spending.
fn analysis_cancel(
    store: &mut AnalysisStore,
    queue: &mut AnalysisQueue,
    client: ClientId,
    params: &Value,
) -> Result<Value, Error> {
    let params: AnalysisJobParams = methods::params(params)?;
    if store.release(&params.job_id, client)? == crate::analysis::Release::Cancelled {
        queue.drop_pending(&params.job_id);
        store.cancel(&params.job_id);
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
        value["report"] = serde_json::to_value(report)
            .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
    }
    if let Some(error) = read.error {
        value["error"] = json!({"code": error.kind.code(), "message": error.detail});
    }
    Ok(value)
}

fn event_response(request: &ApiRequest, sequence: u64, events: &VecDeque<ApiEvent>) -> ApiResponse {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        after: u64,
    }
    match methods::params::<Params>(&request.params) {
        Ok(params) => {
            let oldest = events
                .front()
                .map(|event| event.sequence)
                .unwrap_or(sequence.saturating_add(1));
            let gap = params.after.saturating_add(1) < oldest;
            let selected = events
                .iter()
                .filter(|event| event.sequence > params.after)
                .cloned()
                .collect();
            ApiResponse::success(
                request.id.clone(),
                sequence,
                EventsResult {
                    events: selected,
                    current_sequence: sequence,
                    gap,
                },
            )
        }
        Err(error) => ApiResponse::failure(request.id.clone(), sequence, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreviewSource;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lightwell-owner-{}-{name}", std::process::id()))
    }
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
    }

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

    /// Import a fixture the way every client must now: the owner acknowledges with a job id, the
    /// bounded source worker prepares the original, and `job.adopt` hands back the asset state.
    fn import_asset(owner: &OwnerHandle, client: ClientId, path: &std::path::Path) -> Value {
        let queued = ok(
            owner,
            client,
            "import",
            "catalog.import",
            json!({"path": path}),
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
            match status["state"].as_str() {
                Some("ready") => break,
                Some("queued" | "preparing") => {
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

    /// Poll `analysis.read` until the job leaves `pending`. Nothing in the owner polls: this is the
    /// test standing in for a client that would rather be told, and it fails on a deadline instead
    /// of spinning forever.
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
            if read["status"] != json!("pending") {
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
            match status["state"].as_str() {
                Some("ready" | "failed") => return status,
                Some("queued" | "preparing")
                    if start.elapsed() < std::time::Duration::from_secs(5) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(1))
                }
                other => panic!("source job did not complete: {other:?}: {status}"),
            }
        }
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
        let a = request(&owner, first, "catalog.import", json!({"path":first_path}))
            .result
            .unwrap();
        let b = request(
            &owner,
            second,
            "catalog.import",
            json!({"path":second_path}),
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
            assert_ne!(a_status["state"], "failed", "{a_status}");
            assert_ne!(b_status["state"], "failed", "{b_status}");
            a_ready = a_status["state"] == "ready";
            b_ready = b_status["state"] == "ready";
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
                                json!({"job_id":error.message}),
                            )
                            .result
                            .unwrap();
                            match status["state"].as_str() {
                                Some("ready" | "failed") => break status,
                                Some("queued" | "preparing") => {
                                    assert!(
                                        std::time::Instant::now() < until,
                                        "RAW preparation stalled: {status}"
                                    );
                                    std::thread::sleep(std::time::Duration::from_millis(20));
                                }
                                other => panic!("invalid source state: {other:?}"),
                            }
                        };
                        assert_eq!(prepared["state"], "ready", "{prepared}");
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
        let mut jobs = SourceJobs {
            sender,
            jobs: HashMap::new(),
            active: HashMap::new(),
            completed: VecDeque::new(),
            next_id: 1,
        };
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
        let mut jobs = SourceJobs {
            sender,
            jobs: HashMap::new(),
            active: HashMap::new(),
            completed: VecDeque::new(),
            next_id: 1,
        };
        let first = ClientId(1);
        let second = ClientId(2);
        let id = jobs.enqueue(first, fixture(), None, Vec::new()).unwrap();
        assert_eq!(
            jobs.enqueue(second, fixture(), None, Vec::new()).unwrap(),
            id
        );
        assert_eq!(jobs.cancel(first, &id).unwrap()["state"], "cancelled");
        assert_eq!(jobs.status(second, &id).unwrap()["state"], "queued");
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
        let mut jobs = SourceJobs {
            sender,
            jobs: HashMap::new(),
            active: HashMap::new(),
            completed: VecDeque::new(),
            next_id: 1,
        };
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
        let a = request(&owner, client, "catalog.import", json!({"path":older}))
            .result
            .unwrap();
        let b = request(&owner, client, "catalog.import", json!({"path":newer}))
            .result
            .unwrap();
        let first = a["job_id"].as_str().unwrap();
        let second = b["job_id"].as_str().unwrap();
        assert_eq!(wait_source(&owner, client, first)["state"], "ready");
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
        let a = request(&owner, first, "catalog.import", json!({"path":fixture()}))
            .result
            .unwrap();
        let b = request(&owner, second, "catalog.import", json!({"path":fixture()}))
            .result
            .unwrap();
        let first_id = a["job_id"].as_str().unwrap();
        let second_id = b["job_id"].as_str().unwrap();
        assert_eq!(
            request(&owner, first, "job.cancel", json!({"job_id":first_id}))
                .result
                .unwrap()["state"],
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
        assert_eq!(ready["state"], "ready");
        let asset = ready["asset"]["asset"]["id"].clone();
        let again = request(&owner, second, "catalog.import", json!({"path":fixture()}))
            .result
            .unwrap();
        assert_eq!(
            again["state"], "ready",
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
        assert_eq!(error.job_id.as_deref(), Some(error.message.as_str()));
        assert!(
            request(&owner, client, "session.state", json!({}))
                .error
                .is_none(),
            "the owner answers while decode runs"
        );
        assert_eq!(
            wait_source(&owner, client, &error.message)["state"],
            "ready"
        );
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
        let queued = request(&owner, client, "catalog.import", json!({"path":invalid}))
            .result
            .unwrap();
        let failed = wait_source(&owner, client, queued["job_id"].as_str().unwrap());
        assert_eq!(failed["state"], "failed");
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
        let queued = call(
            viewer,
            "import",
            "catalog.import",
            json!({"path":fixture()}),
        );
        let job_id = queued["job_id"].as_str().unwrap();
        let imported = loop {
            let status = call(viewer, "status", "job.status", json!({"job_id":job_id}));
            match status["state"].as_str() {
                Some("ready") => break status["asset"].clone(),
                Some("queued" | "preparing") => {
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
        assert_eq!(active["status"], json!("pending"));
        let displaced = entry_request(viewer, "b", &entries[1]);
        assert_eq!(displaced["status"], json!("pending"));
        let winner = entry_request(viewer, "c", &entries[2]);
        assert_eq!(winner["status"], json!("pending"));
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
        assert_eq!(again["status"], json!("pending"));
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
        assert_eq!(before["status"], json!("pending"));
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
            json!("pending"),
            "the other client still wants it"
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
            json!({"path": photo}),
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
        // preparing a moment after it is listed; it is held there, and still listed, until the
        // gate opens.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while request(&owner, client, "job.status", json!({"job_id": job_id}))
            .result
            .unwrap()["state"]
            != json!("preparing")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the held job never read as preparing"
            );
            std::thread::yield_now();
        }
        assert!(active_kind(
            &ok(&owner, client, "held", "activity.list", json!({})),
            "source.prepare"
        ));
        source_gate.open();
        assert_eq!(
            wait_source(&owner, client, job_id.as_str().unwrap())["state"],
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

    /// Every method the table lists without a service handler reaches an owner route: none is
    /// refused as unrouted, and only `events.since` answers with the event log. One that reached
    /// no route is an internal error naming it, never the `events.since` answer.
    #[test]
    fn every_handler_less_method_is_routed_and_an_unrouted_one_is_an_internal_error() {
        let catalog = temp("unrouted.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let owner_answered: Vec<&str> = methods::METHODS
            .iter()
            .filter(|spec| spec.handler.is_none())
            .map(|spec| spec.name)
            .collect();
        assert!(owner_answered.contains(&"events.since"));
        for name in owner_answered {
            // Parameters no method declares, so every route refuses them without acting.
            let response = send(&owner, client, name, name, json!({"unrouted-probe": true}));
            if let Some(error) = &response.error {
                assert!(
                    !error.message.contains("no catalog-owner route"),
                    "{name} is listed without a handler but no owner route answers it"
                );
            }
            if name != "events.since" {
                assert!(
                    response
                        .result
                        .as_ref()
                        .is_none_or(|result| result.get("current_sequence").is_none()),
                    "{name} was answered with the event log"
                );
            }
        }
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();

        let refused = unrouted(
            &ApiRequest {
                id: "probe".into(),
                method: "test.unrouted".into(),
                params: json!({"after": 0}),
                token: None,
            },
            7,
        );
        assert!(refused.result.is_none(), "no event log is answered");
        assert_eq!(refused.sequence, 7);
        let error = refused.error.expect("an unrouted method is refused");
        assert_eq!(error.code, "internal");
        assert_eq!(
            error.message,
            "test.unrouted has no service handler and no catalog-owner route"
        );
    }
}
