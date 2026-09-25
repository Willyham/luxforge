//! A module declaring every kind of capability, shared by the descriptor, settings and host tests;
//! a module whose activation, deactivation and resource check the lifecycle tests steer; a
//! loopback server that serves or refuses a resource; and a network that counts every lookup and
//! connection, so a test can prove a path made none.
use super::{
    context::ModuleContext,
    descriptor::{
        ActivationDescriptor, AdapterAuth, AdapterCost, AdapterDescriptor, CapabilityDescriptor,
        CapabilityKind, DataClass, ProfilesDescriptor, ResourceDescriptor, SettingDescriptor,
        SettingKind, SettingsDescriptor, TaskApply, TaskDescriptor,
    },
    transport::{Connect, EndpointClass, Resolve},
};
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, Control, EffectDescriptor, EffectStage, Error,
    ErrorKind, ModuleDescriptor, ParameterDescriptor, ParameterKind, Processing, Stage,
    StageContext, ToolModule,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

pub(crate) const MODULE: &str = "test.capabilities";
pub(crate) const TASK: &str = "generate-test-tint";
pub(crate) const ADAPTER: &str = "echo-adapter";

/// A request's parameters with the `request` mutation envelope filled in as a client fills it: a
/// host method that carries one and was sent none gets `{request_id, actor: "test"}` under the
/// caller's fresh `request_id`, so each call is a new request. Anything else is sent as written, so
/// a test that retries a request, or sends a malformed envelope, writes its own.
pub(crate) fn enveloped(method: &str, params: Value, request_id: &str) -> Value {
    match params {
        Value::Object(mut fields)
            if crate::api::host_envelope(method) == crate::api::params::Envelope::Request
                && !fields.contains_key("mutation") =>
        {
            fields.insert(
                "mutation".into(),
                json!({"request_id": request_id, "actor": "test"}),
            );
            Value::Object(fields)
        }
        params => params,
    }
}

/// A fresh directory path under the system temporary directory; nothing is created.
pub(crate) fn temp(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    std::env::temp_dir().join(format!(
        "luxforge-capabilities-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

pub(crate) fn setting(id: &str, kind: SettingKind, default: Option<Value>) -> SettingDescriptor {
    SettingDescriptor {
        id: id.into(),
        label: id.replace('-', " "),
        help: None,
        kind,
        required: false,
        default,
        invalidates_activation: false,
    }
}

fn parameter(name: &str, kind: ParameterKind, required: bool) -> ParameterDescriptor {
    ParameterDescriptor {
        name: name.into(),
        kind,
        required,
        default: None,
        unit: None,
        step: None,
        precision: None,
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
        notes: "test".into(),
    }
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
/// a secret and a choice, the two implemented capabilities, one resource, an activation that needs
/// a required setting and the resource, and one task that uses both capabilities and applies its
/// artifact.
pub(crate) fn capability_descriptor() -> ModuleDescriptor {
    let required = |setting: SettingDescriptor| SettingDescriptor {
        required: true,
        ..setting
    };
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
            maskable: false,
        }],
        actions: vec![
            ActionDescriptor {
                id: "apply-test-tint".into(),
                title: "Apply tint".into(),
                notes: "test".into(),
                summary: None,
                patch: false,
                parameters: vec![parameter("tint", ParameterKind::Artifact, true)],
            },
            ActionDescriptor {
                id: "reset-test-tint".into(),
                title: "Reset tint".into(),
                notes: "test".into(),
                summary: None,
                patch: false,
                parameters: Vec::new(),
            },
        ],
        controls: vec![Control::Task {
            task: TASK.into(),
            label: "Generate tint".into(),
        }],
        settings: Some(SettingsDescriptor {
            schema: 1,
            fields: vec![
                setting(
                    "strength",
                    SettingKind::Number {
                        min: 0.0,
                        max: 1.0,
                        step: Some(0.01),
                        precision: Some(2),
                    },
                    Some(json!(0.5)),
                ),
                setting(
                    "mode",
                    SettingKind::Enum {
                        options: vec!["fast".into(), "exact".into()],
                    },
                    Some(json!("exact")),
                ),
                setting(
                    "count",
                    SettingKind::Integer { min: 1, max: 8 },
                    Some(json!(2)),
                ),
                setting("enabled", SettingKind::Boolean, Some(json!(true))),
                setting("note", SettingKind::Text { max_length: 16 }, None),
                SettingDescriptor {
                    invalidates_activation: true,
                    ..required(setting("label", SettingKind::Text { max_length: 32 }, None))
                },
                setting(
                    "local-service",
                    SettingKind::Endpoint {
                        classes: vec![EndpointClass::Loopback],
                    },
                    None,
                ),
                setting("token", SettingKind::Secret { max_length: 64 }, None),
            ],
            profiles: Some(ProfilesDescriptor {
                label: "Providers".into(),
                max: 2,
                adapters: vec![adapter()],
                fields: vec![
                    SettingDescriptor {
                        invalidates_activation: true,
                        ..required(setting(
                            "endpoint",
                            SettingKind::Endpoint {
                                classes: vec![EndpointClass::Remote, EndpointClass::Loopback],
                            },
                            None,
                        ))
                    },
                    required(setting(
                        "api-key",
                        SettingKind::Secret { max_length: 128 },
                        None,
                    )),
                    setting(
                        "model",
                        SettingKind::Enum {
                            options: vec!["small".into(), "large".into()],
                        },
                        Some(json!("small")),
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
        activation: Some(ActivationDescriptor {
            requires_settings: vec!["label".into()],
            requires_resources: vec!["palette".into()],
            notes: "Loads the palette.".into(),
        }),
        tasks: vec![TaskDescriptor {
            id: TASK.into(),
            title: "Generate tint".into(),
            notes: "test".into(),
            asset: true,
            profile: true,
            requires_active: true,
            uses: vec!["echo".into(), "palette".into()],
            parameters: vec![parameter(
                "gain",
                ParameterKind::Number { min: 0.0, max: 2.0 },
                false,
            )],
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

/// A module with an activation that requires nothing, for filling the module lane.
pub(crate) fn lane_descriptor(index: usize) -> ModuleDescriptor {
    ModuleDescriptor {
        id: format!("test.lane{index}"),
        title: format!("Lane test {index}"),
        activation: Some(ActivationDescriptor {
            requires_settings: Vec::new(),
            requires_resources: Vec::new(),
            notes: "Waits while held.".into(),
        }),
        ..ModuleDescriptor::default()
    }
}

/// What a lifecycle test steers and observes of a [`LifecycleModule`].
#[derive(Default)]
pub(crate) struct Probe {
    /// While set, an activation waits after loading, checking its context between short sleeps.
    pub hold: AtomicBool,
    /// An activation fails once it stops waiting.
    pub fail: AtomicBool,
    /// `validate_resource` refuses the staged bytes.
    pub refuse: AtomicBool,
    pub activations: AtomicUsize,
    pub deactivations: AtomicUsize,
    /// An activation is between its first and last instruction.
    pub running: AtomicBool,
    /// What the activation loaded: its required resources' bytes. `deactivate` drops it.
    pub loaded: Mutex<Option<Vec<u8>>>,
    /// The activation read the module's `token` secret through its context. The value itself is
    /// never kept.
    pub secret_read: AtomicBool,
}

/// A module whose activation loads its required resources and then waits while its probe holds
/// it, so a test can observe and cancel it mid-way.
pub(crate) struct LifecycleModule {
    descriptor: ModuleDescriptor,
    probe: Arc<Probe>,
}

impl LifecycleModule {
    pub(crate) fn shared(descriptor: ModuleDescriptor, probe: Arc<Probe>) -> Arc<dyn ToolModule> {
        Arc::new(Self { descriptor, probe })
    }

    fn load(&self, context: &ModuleContext) -> Result<(), Error> {
        let mut loaded = Vec::new();
        for resource in self
            .descriptor
            .activation
            .iter()
            .flat_map(|activation| activation.requires_resources.iter())
        {
            let path = context.resource_path(resource)?;
            std::fs::File::open(path)
                .and_then(|mut file| file.read_to_end(&mut loaded))
                .map_err(|error| Error::new(ErrorKind::FileAccess, error.to_string()))?;
        }
        *self.probe.loaded.lock().unwrap() = Some(loaded);
        if let Ok(secret) = context.secret("token") {
            self.probe
                .secret_read
                .store(!secret.expose().is_empty(), Ordering::SeqCst);
        }
        context.progress(Some(0.5), "loaded");
        while self.probe.hold.load(Ordering::SeqCst) {
            context.checkpoint()?;
            thread::sleep(Duration::from_millis(2));
        }
        context.checkpoint()?;
        if self.probe.fail.load(Ordering::SeqCst) {
            return Err(Error::new(ErrorKind::Decode, "the palette is corrupt"));
        }
        Ok(())
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
    fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
        Ok(format!("test layer of {effect_id}"))
    }
    fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
        Err(Error::new(
            ErrorKind::Internal,
            "the lifecycle module never renders",
        ))
    }
    fn activate(&self, context: &ModuleContext) -> Result<(), Error> {
        self.probe.activations.fetch_add(1, Ordering::SeqCst);
        self.probe.running.store(true, Ordering::SeqCst);
        let result = self.load(context);
        self.probe.running.store(false, Ordering::SeqCst);
        result
    }
    fn deactivate(&self) {
        self.probe.deactivations.fetch_add(1, Ordering::SeqCst);
        *self.probe.loaded.lock().unwrap() = None;
    }
    fn validate_resource(&self, resource_id: &str, path: &Path) -> Result<(), Error> {
        if self.probe.refuse.load(Ordering::SeqCst) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("{resource_id} at {} is not a palette", path.display()),
            ));
        }
        Ok(())
    }
}

/// A loopback HTTP server: each connection is answered on its own thread by `respond`, which
/// receives the request path.
pub(crate) struct Server {
    pub address: SocketAddr,
    hits: Arc<AtomicUsize>,
}

impl Server {
    pub(crate) fn start(respond: impl Fn(&str, &mut TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let respond = Arc::new(respond);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let respond = respond.clone();
                let counted = counted.clone();
                thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut head = Vec::new();
                    let mut byte = [0; 1];
                    while !head.ends_with(b"\r\n\r\n") && head.len() < 16 * 1024 {
                        match stream.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    counted.fetch_add(1, Ordering::SeqCst);
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                    respond(&path, &mut stream);
                });
            }
        });
        Self { address, hits }
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    /// Requests answered so far.
    pub(crate) fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Write one complete response.
pub(crate) fn reply(stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// A resolver and connector that count every lookup and connection and otherwise behave like the
/// system's, so a test can prove a path touched no network.
#[derive(Default)]
pub(crate) struct CountingNet {
    pub resolves: AtomicUsize,
    pub connects: AtomicUsize,
}

impl CountingNet {
    pub(crate) fn total(&self) -> usize {
        self.resolves.load(Ordering::SeqCst) + self.connects.load(Ordering::SeqCst)
    }
}

impl Resolve for CountingNet {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        Ok((host, port).to_socket_addrs()?.collect())
    }
}

impl Connect for CountingNet {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        TcpStream::connect_timeout(&address, timeout)
    }
}
