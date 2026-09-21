//! One thread owns the catalog and every client session; all clients call it in turn.
use super::{ApiEvent, ApiRequest, ApiResponse, ClientSession, EventsResult, methods};
use crate::{
    AssetId, EditorService, EditorState, EntryId, Error, ErrorKind, ModuleRegistry, PreviewJob,
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

const EVENT_CAPACITY: usize = 256;
const SOURCE_QUEUE_CAPACITY: usize = 8;
const SOURCE_RESULT_CAPACITY: usize = 64;
/// Pending developments may pin one sensor mosaic identity; the cache can retain one more.
const MAX_QUEUED_MOSAICS: usize = 1;

/// Identifies one connected client; the owner keeps that client's session until it disconnects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(u64);

struct OwnerCall {
    client: ClientId,
    request: ApiRequest,
    response: SyncSender<ApiResponse>,
}

enum OwnerMessage {
    Call(OwnerCall),
    Preview {
        client: ClientId,
        asset_id: AssetId,
        entry_id: Option<EntryId>,
        /// `Some(n)` renders the entry's first `n` layers only.
        layer_count: Option<usize>,
        response: SyncSender<Result<PreviewJob, Error>>,
    },
    SourceStarted(String),
    SourceComplete(String, Result<SourceResult, Error>),
    Disconnect(ClientId),
    Stop,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SourceFlightKey {
    path: PathBuf,
    signature: SourceSignature,
    expected_fingerprint: Option<String>,
    gains_bits: Option<[u32; 3]>,
}

enum SourceTaskKind {
    File,
    Develop(RawDevelopment),
}

enum SourceResult {
    File(PreparedFile),
    Develop(RawDevelopment, RawPrepared),
}

struct SourceTask {
    id: String,
    key: SourceFlightKey,
    cancelled: Arc<AtomicBool>,
    kind: SourceTaskKind,
}

enum SourceState {
    Queued,
    Preparing,
    Ready(Box<EditorState>),
    Failed(Error),
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

impl SourceJobs {
    fn enqueue(
        &mut self,
        client: ClientId,
        path: PathBuf,
        expected_fingerprint: Option<String>,
    ) -> Result<String, Error> {
        let (canonical, signature) = EditorService::request_signature(&path)?;
        let key = SourceFlightKey {
            path: canonical,
            signature,
            expected_fingerprint,
            gains_bits: None,
        };
        if let Some(id) = self.active.get(&key) {
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
            kind: SourceTaskKind::File,
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
        self.active.insert(key.clone(), id.clone());
        self.jobs.insert(
            id.clone(),
            SourceJob {
                key,
                import_request_id: None,
                clients: HashSet::from([client]),
                cancelled,
                state: SourceState::Queued,
                sensor: None,
            },
        );
        Ok(id)
    }

    fn enqueue_development(
        &mut self,
        client: ClientId,
        request: RawDevelopment,
    ) -> Result<String, Error> {
        let key = SourceFlightKey {
            path: request.asset_id.as_str().into(),
            signature: request.signature.clone(),
            expected_fingerprint: Some(request.fingerprint.clone()),
            gains_bits: Some(request.gains.map(f32::to_bits)),
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
        let id = format!("source-job-{}", self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let task = SourceTask {
            id: id.clone(),
            key: key.clone(),
            cancelled: cancelled.clone(),
            kind: SourceTaskKind::Develop(request),
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
        self.active.insert(key.clone(), id.clone());
        self.jobs.insert(
            id.clone(),
            SourceJob {
                key,
                import_request_id: None,
                clients: HashSet::from([client]),
                cancelled,
                state: SourceState::Queued,
                sensor: Some(sensor),
            },
        );
        Ok(id)
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
                    signature,
                    expected_fingerprint: Some(state.asset.fingerprint.clone()),
                    gains_bits: None,
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

    fn complete(&mut self, id: &str, result: Result<EditorState, Error>) {
        let Some(job) = self.jobs.get_mut(id) else {
            return;
        };
        if self.active.get(&job.key).is_some_and(|active| active == id) {
            self.active.remove(&job.key);
        }
        job.state = match result {
            Ok(asset) => SourceState::Ready(Box::new(asset)),
            Err(error) => SourceState::Failed(error),
        };
        job.sensor = None;
        self.completed.push_back(id.to_owned());
        while self.completed.len() > SOURCE_RESULT_CAPACITY {
            if let Some(old) = self.completed.pop_front() {
                self.jobs.remove(&old);
            }
        }
    }
}

fn queue_preparation(
    service: &EditorService,
    jobs: &mut SourceJobs,
    client: ClientId,
    asset_id: &AssetId,
    entry_id: Option<&EntryId>,
) -> Result<String, Error> {
    if let Some(request) = service.raw_development(asset_id, entry_id)? {
        let id = jobs.enqueue_development(client, request)?;
        service.evict_development();
        return Ok(id);
    }
    if let Some(state) = service.cached_state(asset_id)? {
        return jobs.ready(client, state);
    }
    let state = service.state(asset_id)?;
    let id = jobs.enqueue(client, state.asset.locator, Some(state.asset.fingerprint))?;
    service.evict_development();
    Ok(id)
}

fn source_worker(
    receiver: Receiver<SourceTask>,
    owner: SyncSender<OwnerMessage>,
    live_planes: Arc<Mutex<Vec<Weak<Vec<f32>>>>>,
) {
    while let Ok(task) = receiver.recv() {
        if task.cancelled.load(Ordering::Relaxed) {
            let _ = owner.send(OwnerMessage::SourceComplete(
                task.id,
                Err(Error::new(ErrorKind::Conflict, "source job cancelled")),
            ));
            continue;
        }
        // A previous RAW result/cache or active/pending preview may still pin its large float
        // planes. Wait on the worker, never the catalog owner, before another source allocation.
        loop {
            if task.cancelled.load(Ordering::Relaxed) {
                break;
            }
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
                Err(Error::new(ErrorKind::Conflict, "source job cancelled")),
            ));
            continue;
        }
        if owner
            .send(OwnerMessage::SourceStarted(task.id.clone()))
            .is_err()
        {
            break;
        }
        let result = match task.kind {
            SourceTaskKind::File => {
                EditorService::prepare_file_cancel(&task.key.path, &task.cancelled).and_then(
                    |prepared| {
                        if prepared.signature != task.key.signature {
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
                        Ok(SourceResult::File(prepared))
                    },
                )
            }
            SourceTaskKind::Develop(request) => RawPrepared::develop(
                request.sensor.clone(),
                request.fingerprint.clone(),
                request.gains,
                &task.cancelled,
            )
            .map(|developed| SourceResult::Develop(request, developed)),
        };
        if let Ok(ref prepared) = result {
            let raw = match prepared {
                SourceResult::File(file) => match &file.source {
                    crate::source::PreparedSource::Raw(raw) => Some(raw),
                    _ => None,
                },
                SourceResult::Develop(_, raw) => Some(raw),
            };
            if let Some(raw) = raw.and_then(|raw| raw.linear.as_ref()) {
                live_planes
                    .lock()
                    .expect("source memory gate")
                    .push(raw.storage_weak());
            }
        }
        if owner
            .send(OwnerMessage::SourceComplete(task.id, result))
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

#[derive(Clone)]
pub struct OwnerHandle {
    sender: SyncSender<OwnerMessage>,
    next_client: Arc<AtomicU64>,
}

impl OwnerHandle {
    pub fn start(catalog: &Path) -> Result<(Self, JoinHandle<()>), Error> {
        Self::start_with(catalog, Arc::new(ModuleRegistry::builtin()))
    }

    /// Own a catalog served by a specific set of providers, which is how a client registers a
    /// built-in wrapped as unavailable. Registration happens before any catalog work.
    pub fn start_with(
        catalog: &Path,
        registry: Arc<ModuleRegistry>,
    ) -> Result<(Self, JoinHandle<()>), Error> {
        let mut service = EditorService::open_with(catalog, registry)?;
        service.disable_sync_source();
        let (sender, receiver) = sync_channel(64);
        let (source_sender, source_receiver) = sync_channel(SOURCE_QUEUE_CAPACITY);
        let worker_sender = sender.clone();
        let live_planes = Arc::new(Mutex::new(Vec::new()));
        let worker =
            std::thread::spawn(move || source_worker(source_receiver, worker_sender, live_planes));
        let join = std::thread::spawn(move || owner_loop(service, receiver, source_sender, worker));
        Ok((
            Self {
                sender,
                next_client: Arc::new(AtomicU64::new(1)),
            },
            join,
        ))
    }

    /// Allocate a client identity; its session starts as default on first use.
    pub fn register(&self) -> ClientId {
        ClientId(self.next_client.fetch_add(1, Ordering::Relaxed))
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

    /// A preview job from the catalog owner. `layer_count` truncates the rendered stack to its
    /// first `n` layers; this is a desktop-internal path, not a JSON method.
    pub fn preview_job(
        &self,
        client: ClientId,
        asset_id: AssetId,
        entry_id: Option<EntryId>,
        layer_count: Option<usize>,
    ) -> Result<PreviewJob, Error> {
        let (response, receiver) = sync_channel(1);
        self.sender
            .send(OwnerMessage::Preview {
                client,
                asset_id,
                entry_id,
                layer_count,
                response,
            })
            .map_err(|_| Error::new(ErrorKind::Protocol, "catalog owner is unavailable"))?;
        receiver
            .recv()
            .map_err(|_| Error::new(ErrorKind::Protocol, "catalog owner stopped before preview"))?
    }
}

fn owner_loop(
    mut service: EditorService,
    receiver: Receiver<OwnerMessage>,
    source_sender: SyncSender<SourceTask>,
    worker: JoinHandle<()>,
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
    let mut latest_import: HashMap<ClientId, String> = HashMap::new();
    while let Ok(message) = receiver.recv() {
        match message {
            OwnerMessage::Stop => break,
            OwnerMessage::Disconnect(client) => {
                sessions.remove(&client);
                latest_import.remove(&client);
                jobs.disconnect(client);
            }
            OwnerMessage::SourceStarted(id) => {
                if let Some(job) = jobs.jobs.get_mut(&id) {
                    job.state = SourceState::Preparing;
                }
            }
            OwnerMessage::SourceComplete(id, result) => {
                let interested = jobs
                    .jobs
                    .get(&id)
                    .is_some_and(|job| !job.clients.is_empty());
                let outcome = if interested {
                    result.and_then(|prepared| match prepared {
                        SourceResult::File(file) => service.import_prepared(file),
                        SourceResult::Develop(request, developed) => service
                            .install_development(&request, developed)
                            .map(|state| (state, false)),
                    })
                } else {
                    Err(Error::new(ErrorKind::Conflict, "source job cancelled"))
                };
                if outcome.as_ref().is_ok_and(|(_, created)| *created) {
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
                jobs.complete(&id, outcome.map(|(state, _)| state));
                if !jobs.active.is_empty() {
                    service.evict_development();
                }
            }
            OwnerMessage::Preview {
                client,
                asset_id,
                entry_id,
                layer_count,
                response,
            } => {
                let result = service.preview_job(&asset_id, entry_id.as_ref(), layer_count);
                let result = match result {
                    Err(error) if error.kind == ErrorKind::PreparationRequired => {
                        let queued = queue_preparation(
                            &service,
                            &mut jobs,
                            client,
                            &asset_id,
                            entry_id.as_ref(),
                        );
                        Err(queued.map_or_else(
                            |error| error,
                            |id| Error::new(ErrorKind::PreparationRequired, id),
                        ))
                    }
                    other => other,
                };
                let _ = response.send(result);
            }
            OwnerMessage::Call(call) => {
                if matches!(
                    call.request.method.as_str(),
                    "catalog.import" | "job.status" | "job.adopt" | "job.cancel" | "source.prepare"
                ) {
                    let answer: Result<Value, Error> = match call.request.method.as_str() {
                        "catalog.import" => parse_params::<ImportParams>(&call.request.params)
                            .and_then(|p| match service.cached_import(&p.path)? {
                                Some(state) => {
                                    jobs.ready(call.client, state).map(|id| (id, "ready"))
                                }
                                None => {
                                    let expected = service.known_fingerprint(&p.path)?;
                                    let id = jobs.enqueue(call.client, p.path, expected)?;
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
                                )
                            })
                            .map(|id| json!({"job_id":id,"state":"queued"})),
                        _ => unreachable!(),
                    };
                    let response = match answer {
                        Ok(value) => ApiResponse::success(call.request.id, sequence, value),
                        Err(error) => ApiResponse::failure(call.request.id, sequence, error),
                    };
                    let _ = call.response.send(response);
                    continue;
                }
                let session = sessions.entry(call.client).or_default();
                // Discovery and dispatch resolve through the same registry-aware lookup.
                let method = methods::find(&service, &call.request.method);
                let response = match method {
                    Some(method) if method.owner_answered() => {
                        event_response(&call.request, sequence, &events)
                    }
                    Some(method) => {
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
                            let queued = asset_id.and_then(|id| {
                                entry_id.and_then(|entry| {
                                    queue_preparation(
                                        &service,
                                        &mut jobs,
                                        call.client,
                                        &id,
                                        entry.as_ref(),
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
                    None => methods::dispatch(&mut service, session, &call.request, sequence),
                };
                let _ = call.response.send(response);
            }
        }
    }
    for job in jobs.jobs.values() {
        job.cancelled.store(true, Ordering::Relaxed);
    }
    drop(jobs);
    drop(receiver);
    let _ = worker.join();
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
    use serde_json::{Value, json};
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lightwell-owner-{}-{name}", std::process::id()))
    }
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
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
        let first = jobs.enqueue_development(ClientId(1), a.clone()).unwrap();
        assert_eq!(jobs.enqueue_development(ClientId(2), a).unwrap(), first);
        let refusal = jobs
            .enqueue_development(ClientId(3), b.clone())
            .unwrap_err();
        assert_eq!(refusal.kind, ErrorKind::ResourceLimit);
        assert!(refusal.detail.starts_with("RAW mosaic queue is full"));
        drop(receiver.try_recv().unwrap());
        jobs.complete(&first, Err(Error::new(ErrorKind::Conflict, "finished")));
        assert!(jobs.enqueue_development(ClientId(3), b).is_ok());
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
        let id = jobs.enqueue(first, fixture(), None).unwrap();
        assert_eq!(jobs.enqueue(second, fixture(), None).unwrap(), id);
        assert_eq!(jobs.cancel(first, &id).unwrap()["state"], "cancelled");
        assert_eq!(jobs.status(second, &id).unwrap()["state"], "queued");
        jobs.disconnect(second);
        assert!(jobs.jobs[&id].cancelled.load(Ordering::Relaxed));
        assert_ne!(jobs.enqueue(first, fixture(), None).unwrap(), id);
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
        let first = jobs.enqueue(ClientId(1), path.clone(), None).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = 0;
        std::fs::write(&path, bytes).unwrap();
        let second = jobs.enqueue(ClientId(2), path.clone(), None).unwrap();
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
}
