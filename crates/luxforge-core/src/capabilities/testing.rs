//! A module declaring every kind of capability, shared by the descriptor, settings and host tests;
//! a module whose resource check the lifecycle tests steer; and the in-memory transport every
//! capability test sends through, which counts its requests, so a test can prove a path sent none,
//! and answers the capability proof's fake provider in process.
use super::{
    descriptor::{
        AdapterAuth, AdapterCost, AdapterDescriptor, CapabilityDescriptor, CapabilityKind,
        DataClass, ProfilesDescriptor, ResourceDescriptor, SettingDescriptor, SettingsDescriptor,
        TaskApply, TaskDescriptor,
    },
    endpoint::EndpointClass,
    transport::{Method, SendOptions, Transport, TransportRequest, TransportResponse},
};
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, CapabilityModule, Control, EffectDescriptor,
    EffectStage, Error, ModuleDescriptor, ParameterDescriptor, Processing, Stage, StageContext,
    ToolModule,
};
use luxforge_testkit::ProofEndpoint;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
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
pub(crate) use luxforge_testkit::fixtures::temp_path as temp;

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

/// Every remaining setting kind at module level, a bearer adapter whose profiles hold an endpoint,
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
                setting(ParameterDescriptor::endpoint(
                    "local-service",
                    [EndpointClass::Loopback],
                )),
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
    fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
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
