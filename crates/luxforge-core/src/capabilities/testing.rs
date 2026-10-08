//! A module declaring every kind of capability, shared by the descriptor, settings and host tests;
//! a module whose resource check the lifecycle tests steer; the in-memory transport every
//! capability test sends through, which counts its requests, so a test can prove a path sent none,
//! and answers the capability proof's fake provider in process; and the one owner harness the
//! lifecycle and proof tests drive the catalog owner through, as a client does.
use super::{
    descriptor::{
        AdapterAuth, AdapterCost, AdapterDescriptor, CapabilityDescriptor, CapabilityKind,
        DataClass, ProfilesDescriptor, ResourceDescriptor, SettingDescriptor, SettingsDescriptor,
        TaskApply, TaskDescriptor,
    },
    endpoint::EndpointClass,
    host::{HostConfig, STATUS},
    settings::READ,
    transport::{Method, SendOptions, Transport, TransportRequest, TransportResponse},
};
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, ApiFailure, ApiRequest, ApiResponse,
    CapabilityModule, ClientAuthority, ClientId, Control, EffectDescriptor, EffectStage, Error,
    ModuleDescriptor, ModuleRegistry, OwnerHandle, ParameterDescriptor, Processing, StageContext,
    ToolModule, editor::mutation_json, jobs::JOB_READ,
};
use luxforge_testbase::wait_for;
use luxforge_testbase::{ProofEndpoint, ProofProtocol};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use url::Url;

pub(crate) const MODULE: &str = "test.capabilities";
pub(crate) const TASK: &str = "generate-test-tint";
pub(crate) const ADAPTER: &str = "echo-adapter";

/// A request's parameters with the `request` mutation envelope filled in as a client fills it: a
/// host method that carries one, or a generated `task.*` method, that was sent none gets
/// `{request_id, actor: "test"}` under the caller's fresh `request_id`, so each call is a new
/// request. Anything else is sent as written, so a test that retries a request, or sends a
/// malformed envelope, writes its own.
pub(crate) fn enveloped(method: &str, params: Value, request_id: &str) -> Value {
    let carries = crate::api::host_envelope(method) == crate::api::params::Envelope::Request
        || method.starts_with(super::host::TASK_PREFIX);
    match params {
        Value::Object(mut fields) if carries && !fields.contains_key("mutation") => {
            fields.insert(
                "mutation".into(),
                json!({"request_id": request_id, "actor": "test"}),
            );
            Value::Object(fields)
        }
        params => params,
    }
}

/// A fresh path under the system temporary directory; nothing is created.
pub(crate) use luxforge_testbase::paths::temp_path as temp;

/// A setting of `parameter`, labelled with its name.
pub(crate) fn setting(parameter: ParameterDescriptor) -> SettingDescriptor {
    let label = parameter.name.replace('-', " ");
    SettingDescriptor::new(parameter, label)
}

pub(crate) fn adapter() -> AdapterDescriptor {
    AdapterDescriptor {
        id: ADAPTER.into(),
        title: "Echo".into(),
        auth: AdapterAuth::Bearer,
        data: vec![DataClass::SampleGrid8],
        max_request_bytes: 4096,
        max_response_bytes: 65536,
        timeout_ms: 5000,
        retention: Some("The echo service keeps nothing.".into()),
        cost: AdapterCost::Free,
    }
}

/// Every setting kind a module-level field takes, a bearer adapter whose profiles hold an endpoint,
/// a secret and a choice, the two implemented capabilities, one resource, and one task that uses
/// both capabilities and applies its artifact.
pub(crate) fn capability_descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        id: MODULE.into(),
        title: "Capabilities test".into(),
        effects: vec![EffectDescriptor {
            id: "test.capabilities.tint".into(),
            format: 1,
            stage: EffectStage::Color,
            order: 0,
            artifacts: true,
            single: false,
            sources: Vec::new(),
            maskable: false,
        }],
        actions: vec![
            ActionDescriptor {
                id: "apply-test-tint".into(),
                title: "Apply tint".into(),
                notes: "test".into(),
                patch: false,
                preset: true,
                analysis: None,
                shortcut: None,
                parameters: vec![
                    ParameterDescriptor::artifact("tint")
                        .required(true)
                        .notes("test"),
                ],
            },
            ActionDescriptor {
                id: "reset-test-tint".into(),
                title: "Reset tint".into(),
                notes: "test".into(),
                patch: false,
                preset: true,
                analysis: None,
                shortcut: None,
                parameters: Vec::new(),
            },
        ],
        controls: vec![Control::task(TASK, "Generate tint").into()],
        settings: Some(SettingsDescriptor {
            schema: 1,
            fields: vec![
                setting(
                    ParameterDescriptor::number("strength", 0.0, 1.0)
                        .default(0.5)
                        .step(0.01)
                        .precision(2),
                ),
                setting(
                    ParameterDescriptor::enumeration("mode", ["fast", "exact"]).default("exact"),
                ),
                setting(ParameterDescriptor::integer("count", 1, 8).default(2)),
                setting(ParameterDescriptor::boolean("enabled").default(true)),
                setting(ParameterDescriptor::string("note", 16)),
                setting(ParameterDescriptor::string("label", 32).required(true)),
                setting(ParameterDescriptor::secret("token", 64)),
            ],
            profiles: Some(ProfilesDescriptor {
                label: "Providers".into(),
                max: 2,
                adapters: vec![adapter()],
                fields: vec![
                    setting(
                        ParameterDescriptor::endpoint(
                            "endpoint",
                            [EndpointClass::Remote, EndpointClass::Loopback],
                        )
                        .required(true),
                    ),
                    setting(ParameterDescriptor::secret("api-key", 128).required(true)),
                    setting(
                        ParameterDescriptor::enumeration("model", ["small", "large"])
                            .default("small"),
                    ),
                ],
            }),
        }),
        capabilities: vec![
            CapabilityDescriptor {
                id: "echo".into(),
                kind: CapabilityKind::RemoteImageRequest {
                    adapter: ADAPTER.into(),
                    data: DataClass::SampleGrid8,
                },
                purpose: "Ask the echo service for a tint.".into(),
            },
            CapabilityDescriptor {
                id: "palette".into(),
                kind: CapabilityKind::DownloadArtifact {
                    resource: "palette".into(),
                },
                purpose: "Install the tint palette.".into(),
            },
        ],
        resources: vec![ResourceDescriptor {
            id: "palette".into(),
            title: "Tint palette".into(),
            version: "1.0.0".into(),
            url: "https://example.com/palette.bin".into(),
            bytes: 12,
            sha256: "a".repeat(64),
            format: "rgb-gains".into(),
            license: "CC0-1.0".into(),
            provenance: "Generated for tests".into(),
            redirect_origins: vec!["https://cdn.example.com".into()],
        }],
        tasks: vec![TaskDescriptor {
            id: TASK.into(),
            title: "Generate tint".into(),
            notes: "test".into(),
            asset: true,
            profile: true,
            uses: vec!["echo".into(), "palette".into()],
            parameters: vec![ParameterDescriptor::number("gain", 0.0, 2.0).notes("test")],
            apply: Some(TaskApply {
                action: "apply-test-tint".into(),
                parameter: "tint".into(),
            }),
        }],
        ..ModuleDescriptor::default()
    }
}

/// The palette the lifecycle tests install: exactly twelve bytes.
pub(crate) const PALETTE: &[u8] = b"twelve bytes";
/// A second resource, so one install can wait behind another.
pub(crate) const SWATCH: &[u8] = b"sixteen byte set";

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The capability descriptor with its palette served from `palette_url`, and a second resource,
/// `swatch`, served from `swatch_url` under its own `download-artifact` capability.
pub(crate) fn lifecycle_descriptor(palette_url: &str, swatch_url: &str) -> ModuleDescriptor {
    let mut descriptor = capability_descriptor();
    let palette = &mut descriptor.resources[0];
    palette.url = palette_url.into();
    palette.bytes = PALETTE.len() as u64;
    palette.sha256 = sha256_hex(PALETTE);
    descriptor.resources.push(ResourceDescriptor {
        id: "swatch".into(),
        title: "Swatch".into(),
        version: "2".into(),
        url: swatch_url.into(),
        bytes: SWATCH.len() as u64,
        sha256: sha256_hex(SWATCH),
        format: "rgb-gains".into(),
        license: "CC0-1.0".into(),
        provenance: "Generated for tests".into(),
        redirect_origins: Vec::new(),
    });
    descriptor.capabilities.push(CapabilityDescriptor {
        id: "swatches".into(),
        kind: CapabilityKind::DownloadArtifact {
            resource: "swatch".into(),
        },
        purpose: "Install the swatch.".into(),
    });
    descriptor
}

/// What a lifecycle test steers of a [`LifecycleModule`].
#[derive(Default)]
pub(crate) struct Probe {
    /// `validate_resource` refuses the staged bytes.
    pub refuse: AtomicBool,
}

/// A module whose resource check its probe steers.
pub(crate) struct LifecycleModule {
    descriptor: ModuleDescriptor,
    probe: Arc<Probe>,
}

impl LifecycleModule {
    pub(crate) fn shared(descriptor: ModuleDescriptor, probe: Arc<Probe>) -> Arc<dyn ToolModule> {
        Arc::new(Self { descriptor, probe })
    }
}

impl ToolModule for LifecycleModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: Map::new(),
        })
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::NoOp)
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, effect_id: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new(format!(
            "test layer of {effect_id}"
        )))
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Err(Error::internal("the lifecycle module never renders"))
    }
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        Some(self)
    }
}

impl CapabilityModule for LifecycleModule {
    fn validate_resource(&self, resource_id: &str, path: &Path) -> Result<(), Error> {
        if self.probe.refuse.load(Ordering::SeqCst) {
            return Err(Error::validation(format!(
                "{resource_id} at {} is not a palette",
                path.display()
            )));
        }
        Ok(())
    }
}

/// One request as a [`MemoryTransport`]'s responder sees it: header names lowercased, as a server
/// reads them.
pub(crate) struct Exchange {
    pub method: Method,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Exchange {
    /// The URL's path, as a server's request line names it.
    pub(crate) fn path(&self) -> &str {
        self.url.path()
    }
}

enum Piece {
    Status(u16),
    Bytes(Vec<u8>),
    Failed(Error),
}

/// What a responder answers with, piece by piece, as a server writes to its connection: a status,
/// then the body in as many pieces as it likes, or a failure of the transport's own in place of
/// either. A responder may stop between pieces, at a gate, to hold the rest of its answer.
pub(crate) struct Answer(mpsc::Sender<Piece>);

impl Answer {
    pub(crate) fn status(&self, status: u16) {
        let _ = self.0.send(Piece::Status(status));
    }

    pub(crate) fn bytes(&self, bytes: &[u8]) {
        let _ = self.0.send(Piece::Bytes(bytes.to_vec()));
    }

    /// A whole answer: the status and the body in one piece.
    pub(crate) fn whole(&self, status: u16, body: &[u8]) {
        self.status(status);
        self.bytes(body);
    }

    /// The transport refuses the request itself, as it would a redirect or an address.
    pub(crate) fn fail(&self, error: Error) {
        let _ = self.0.send(Piece::Failed(error));
    }
}

type Respond = dyn Fn(&Exchange, &Answer) + Send + Sync;

/// A transport that answers every request in process, with no socket, resolver or TLS: the core's
/// capability tests send through it, and the real transport's own tests live with it in
/// `luxforge-net`. Each request's responder runs on a thread of its own, the way a server answers
/// a connection, so the request's job can be cancelled while the answer is held, and the cancel
/// returns at once, leaving the responder where it is. It keeps the contract's bounds a caller
/// relies on: a cancelled job sends nothing, the request and response size limits, progress after
/// each piece, and a sink that refuses a write fails the request.
pub(crate) struct MemoryTransport {
    respond: Arc<Respond>,
    sends: AtomicUsize,
}

impl MemoryTransport {
    pub(crate) fn new(respond: impl Fn(&Exchange, &Answer) + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            respond: Arc::new(respond),
            sends: AtomicUsize::new(0),
        })
    }

    /// How many requests reached a responder.
    pub(crate) fn sends(&self) -> usize {
        self.sends.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for MemoryTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryTransport")
            .field("sends", &self.sends())
            .finish_non_exhaustive()
    }
}

impl Transport for MemoryTransport {
    fn send(
        &self,
        request: &TransportRequest,
        options: SendOptions<'_>,
        sink: &mut dyn Write,
    ) -> Result<TransportResponse, Error> {
        let SendOptions {
            max_request_bytes,
            max_response_bytes,
            control,
            progress,
            ..
        } = options;
        control.checkpoint()?;
        if request.body.len() as u64 > max_request_bytes {
            return Err(Error::resource_limit(format!(
                "the request body is larger than {max_request_bytes} bytes"
            )));
        }
        self.sends.fetch_add(1, Ordering::SeqCst);
        let exchange = Exchange {
            method: request.method,
            url: request.endpoint.url.clone(),
            headers: request
                .headers
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
                .collect(),
            body: request.body.clone(),
        };
        let (sender, pieces) = mpsc::channel();
        let respond = self.respond.clone();
        thread::Builder::new()
            .name("luxforge-test-exchange".into())
            .spawn(move || respond(&exchange, &Answer(sender)))
            .map_err(|error| Error::internal(error.to_string()))?;
        // The next piece, or `None` once the responder is done; a cancel ends the wait at once.
        let next = || loop {
            control.checkpoint()?;
            match pieces.recv_timeout(Duration::from_millis(1)) {
                Ok(Piece::Failed(error)) => return Err(error),
                Ok(piece) => return Ok(Some(piece)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            }
        };
        let status = match next()? {
            Some(Piece::Status(status)) => status,
            _ => return Err(Error::file_access("the exchange answered no status")),
        };
        let mut received = 0;
        while let Some(piece) = next()? {
            let Piece::Bytes(bytes) = piece else {
                return Err(Error::file_access("the exchange answered twice"));
            };
            if bytes.len() as u64 > max_response_bytes - received {
                return Err(Error::resource_limit(format!(
                    "the response is larger than {max_response_bytes} bytes"
                )));
            }
            sink.write_all(&bytes).map_err(|error| {
                Error::file_access(format!("cannot store the response: {}", error.kind()))
            })?;
            received += bytes.len() as u64;
            progress(received, None);
        }
        control.checkpoint()?;
        Ok(TransportResponse {
            status,
            headers: Vec::new(),
            final_url: request.endpoint.url.clone(),
            received,
        })
    }
}

/// The capability proof's fake provider, answered in process, serving this crate's side of the
/// exchange: its paths, its pinned palette and a well-formed one the pin refuses, and the size of a
/// sample grid. `luxforge_testkit::proof_protocol` hands every other crate's endpoint the same.
pub(crate) fn proof_endpoint(api_key: &str) -> ProofEndpoint {
    ProofEndpoint::in_process(
        api_key,
        ProofProtocol {
            palette_path: crate::PROOF_PALETTE_PATH,
            generate_path: crate::PROOF_GENERATE_PATH,
            palette: crate::PROOF_PALETTE.to_vec(),
            wrong_palette: crate::palette_bytes([1.0, 1.0, 1.0]).to_vec(),
            grid_bytes: super::data::SAMPLE_GRID_BYTES,
            grid_samples: super::data::SAMPLE_GRID_SAMPLES,
        },
    )
}

/// A transport that answers every request with `endpoint`, in process.
pub(crate) fn proof_transport(endpoint: Arc<ProofEndpoint>) -> Arc<MemoryTransport> {
    MemoryTransport::new(move |exchange, answer| {
        let method = match exchange.method {
            Method::Get => "GET",
            Method::Post => "POST",
        };
        let reply = endpoint.answer(method, exchange.path(), &exchange.headers, &exchange.body);
        answer.whole(reply.status, &reply.body);
    })
}

/// A running owner with an edit client and a permission client, recording every response it gives.
pub(crate) struct Owner {
    pub handle: OwnerHandle,
    join: Option<JoinHandle<()>>,
    pub edit: ClientId,
    pub admin: ClientId,
    next: Cell<u64>,
    pub observed: RefCell<Vec<String>>,
}

impl Owner {
    /// Start an owner over `catalog` and register its two clients.
    pub(crate) fn start(catalog: &Path, registry: Arc<ModuleRegistry>, host: HostConfig) -> Self {
        let (handle, join) = OwnerHandle::start_with_host(catalog, registry, host).unwrap();
        let edit = handle.register();
        let admin = handle.register_with(ClientAuthority::Permissions);
        Self {
            handle,
            join: Some(join),
            edit,
            admin,
            next: Cell::new(0),
            observed: RefCell::new(Vec::new()),
        }
    }

    /// Send one request as `client`, its envelope filled in as a client fills it ([`enveloped`]),
    /// and record the answer.
    pub(crate) fn call(&self, client: ClientId, method: &str, params: Value) -> ApiResponse {
        let id = format!("request-{}", self.next.get());
        self.next.set(self.next.get() + 1);
        let params = enveloped(method, params, &id);
        let response = self
            .handle
            .call(
                client,
                ApiRequest {
                    id,
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        self.observed
            .borrow_mut()
            .push(serde_json::to_string(&response).unwrap());
        response
    }

    pub(crate) fn ok_as(&self, client: ClientId, method: &str, params: Value) -> Value {
        let response = self.call(client, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    pub(crate) fn ok(&self, method: &str, params: Value) -> Value {
        self.ok_as(self.edit, method, params)
    }

    pub(crate) fn fail_as(&self, client: ClientId, method: &str, params: Value) -> ApiFailure {
        self.call(client, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to fail"))
    }

    pub(crate) fn fail(&self, method: &str, params: Value) -> ApiFailure {
        self.fail_as(self.edit, method, params)
    }

    /// The settings revision of `module`.
    pub(crate) fn revision(&self, module: &str) -> u64 {
        self.ok(READ, json!({"module_id": module}))["revision"]
            .as_u64()
            .unwrap()
    }

    /// A settings mutation of `module` at its current revision, under a fresh request identity.
    pub(crate) fn mutation(&self, module: &str) -> Value {
        mutation_json(self.revision(module), &uuid::Uuid::new_v4().to_string())
    }

    pub(crate) fn status(&self, module: &str) -> Value {
        self.ok(STATUS, json!({"module_id": module}))
    }

    pub(crate) fn job(&self, job_id: &Value) -> Value {
        self.ok(JOB_READ, json!({"job_id": job_id}))
    }

    /// Wait for a capability job to finish and return its record.
    pub(crate) fn finished(&self, job_id: &Value) -> Value {
        self.until(job_id, |job| {
            !matches!(job["status"].as_str(), Some("queued" | "running"))
        })
    }

    /// Wait until a job's record satisfies `done`, and return it.
    pub(crate) fn until(&self, job_id: &Value, done: impl Fn(&Value) -> bool) -> Value {
        wait_for(&format!("job {job_id} getting there"), || {
            Some(self.job(job_id)).filter(|job| done(job))
        })
    }

    /// Every recorded event: its method and the request that caused it.
    pub(crate) fn events(&self) -> Vec<(String, String)> {
        self.ok("events.since", json!({"after": 0}))["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                (
                    event["method"].as_str().unwrap().to_owned(),
                    event["request_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// The method of every recorded event, in order.
    pub(crate) fn methods(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .map(|(method, _)| method)
            .collect()
    }

    /// Stop the owner and return every response it gave.
    pub(crate) fn stop(mut self) -> Vec<String> {
        self.handle.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
        self.observed.take()
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            self.handle.stop();
            let _ = join.join();
        }
    }
}

/// Every file under `root`, read whole.
pub(crate) fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                found.push((path, bytes));
            }
        }
    }
    found
}
