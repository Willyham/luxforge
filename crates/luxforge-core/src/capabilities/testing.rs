//! A module declaring every kind of capability, shared by the descriptor, settings and host tests;
//! a module whose resource check the lifecycle tests steer; and a
//! network that counts every lookup and connection, so a test can prove a path made none. The
//! loopback servers the tests answer downloads with are `luxforge-testkit`'s.
use super::{
    descriptor::{
        AdapterAuth, AdapterCost, AdapterDescriptor, CapabilityDescriptor, CapabilityKind,
        DataClass, ProfilesDescriptor, ResourceDescriptor, SettingDescriptor, SettingsDescriptor,
        TaskApply, TaskDescriptor,
    },
    transport::{Connect, EndpointClass, Resolve},
};
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, CapabilityModule, Control, EffectDescriptor,
    EffectStage, Error, ModuleDescriptor, ParameterDescriptor, Processing, Stage, StageContext,
    ToolModule,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    io,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

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
                summary: None,
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
    fn describe_layer(&self, effect_id: &str, _: u32, _: &Value) -> Result<String, Error> {
        Ok(format!("test layer of {effect_id}"))
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
