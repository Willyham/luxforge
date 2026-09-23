//! Developer proof of the shared module capabilities. `lightwell.capabilities` declares a setting
//! of each plain and file kind, a bearer provider adapter whose profiles hold an endpoint and a
//! secret key, the three implemented capabilities, one pinned resource its activation loads, and
//! one worker task that reads the selected file, sends the photo's sample grid to the profile's
//! endpoint and publishes a tint artifact, which its colour-stage effect applies by multiplying
//! linear channels. It is a test fixture: the desktop registers it only in developer mode with
//! `--proof-endpoint`, `lightwell-json` with `--proof-endpoint`, and tests directly, always against
//! a [`ProofEndpoint`] a harness started. See `docs/design/module-capabilities.md#proof-module`.
mod endpoint;

pub use endpoint::{ProofEndpoint, ProofRequest};

use super::{
    ActionInput, ActionPlan, ColorOperation, ModuleDescriptor, PointwiseColor, Processing, Stage,
    StageContext, ToolModule,
};
use crate::{
    ArtifactId, EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId,
    artifacts::{ArtifactMeta, PreparedArtifact},
    capabilities::context::ModuleContext,
};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    thread,
    time::{Duration, Instant},
};

pub const PROOF_MODULE: &str = "lightwell.capabilities";
pub const PROOF_EFFECT: &str = "lightwell.capabilities.tint";
pub const PROOF_TASK: &str = "generate-proof-tint";
pub const APPLY_PROOF_TINT: &str = "apply-proof-tint";
pub const RESET_PROOF_TINT: &str = "reset-proof-tint";
pub const PROOF_ADAPTER: &str = "proof-echo";
pub const PROOF_RESOURCE: &str = "proof-palette";
/// The version the resource is pinned at, which names its install directory.
pub const PROOF_RESOURCE_VERSION: &str = "1";
/// The kind of the artifact the task publishes.
pub const PROOF_TINT_KIND: &str = "lightwell.capabilities.tint";
/// Where the proof endpoint serves the palette and answers the sample grid, under its base URL.
pub const PROOF_PALETTE_PATH: &str = "/proof-palette.bin";
pub const PROOF_GENERATE_PATH: &str = "/generate";

/// The palette's magic.
const PALETTE_MAGIC: [u8; 8] = *b"LWPAL001";
/// The three linear gains the palette holds.
pub const PROOF_PALETTE_GAINS: [f32; 3] = [1.04, 1.0, 0.94];
/// The palette: its magic, then the three gains as little-endian `f32`.
pub const PROOF_PALETTE: [u8; 20] = palette_bytes(PROOF_PALETTE_GAINS);
/// The SHA-256 the resource is pinned at; a test recomputes it from [`PROOF_PALETTE`].
pub const PROOF_PALETTE_SHA256: &str =
    "34b622d7abf5eea277c35a33a1e7f0aaa43189a6aa7f7b441ec3347510719945";
/// A tint artifact: three little-endian `f32` linear gains.
const TINT_BYTES: usize = 12;

/// The palette bytes of three gains, as the proof endpoint serves them.
pub const fn palette_bytes(gains: [f32; 3]) -> [u8; 20] {
    let mut bytes = [0; 20];
    let mut index = 0;
    while index < PALETTE_MAGIC.len() {
        bytes[index] = PALETTE_MAGIC[index];
        index += 1;
    }
    let mut channel = 0;
    while channel < 3 {
        let gain = gains[channel].to_le_bytes();
        let mut byte = 0;
        while byte < 4 {
            bytes[PALETTE_MAGIC.len() + channel * 4 + byte] = gain[byte];
            byte += 1;
        }
        channel += 1;
    }
    bytes
}

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Three little-endian `f32` values starting at `offset`, when all are finite and positive.
fn gains_at(bytes: &[u8], offset: usize) -> Option<[f32; 3]> {
    let gain = |channel: usize| {
        let start = offset + channel * 4;
        bytes
            .get(start..start + 4)
            .map(|four| f32::from_le_bytes(four.try_into().expect("four bytes")))
    };
    let gains = [gain(0)?, gain(1)?, gain(2)?];
    gains
        .iter()
        .all(|gain| gain.is_finite() && *gain > 0.0)
        .then_some(gains)
}

/// The palette's gains: exactly the magic and three finite, positive gains.
fn parse_palette(bytes: &[u8]) -> Result<[f32; 3], Error> {
    if bytes.len() != PROOF_PALETTE.len() || bytes[..PALETTE_MAGIC.len()] != PALETTE_MAGIC {
        return Err(validation(
            "the proof palette does not start with its magic",
        ));
    }
    gains_at(bytes, PALETTE_MAGIC.len())
        .ok_or_else(|| validation("the proof palette's gains are not finite and positive"))
}

/// Read an installed or staged palette, at most one byte more than its length.
fn read_palette(path: &Path) -> Result<[f32; 3], Error> {
    let mut bytes = Vec::with_capacity(PROOF_PALETTE.len() + 1);
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(PROOF_PALETTE.len() as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| {
            Error::new(
                ErrorKind::FileAccess,
                format!("cannot read the proof palette: {}", error.kind()),
            )
        })?;
    parse_palette(&bytes)
}

/// The small factor the input file contributes: 0.9 to 1.1 by the first byte of its SHA-256.
pub fn input_factor(sha256: &[u8]) -> f32 {
    0.9 + f32::from(sha256.first().copied().unwrap_or(0)) / 255.0 * 0.2
}

/// The endpoint's answer, `{"rgb": [r, g, b]}` with three codes of 1 to 255 and nothing else.
fn parse_answer(body: &[u8]) -> Result<[u8; 3], Error> {
    let refused = || validation("the proof endpoint answered something other than a tint");
    let answer: Value = serde_json::from_slice(body).map_err(|_| refused())?;
    let object = answer
        .as_object()
        .filter(|object| object.len() == 1)
        .ok_or_else(refused)?;
    let channels = object
        .get("rgb")
        .and_then(Value::as_array)
        .filter(|channels| channels.len() == 3)
        .ok_or_else(refused)?;
    let mut rgb = [0; 3];
    for (slot, channel) in rgb.iter_mut().zip(channels) {
        *slot = channel
            .as_u64()
            .filter(|code| (1..=255).contains(code))
            .and_then(|code| u8::try_from(code).ok())
            .ok_or_else(refused)?;
    }
    Ok(rgb)
}

/// Three linear-light gains, one per channel.
struct Tint([f32; 3]);

impl PointwiseColor for Tint {
    fn apply_row(&self, _: u32, _: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            for (channel, gain) in pixel.iter_mut().zip(self.0) {
                *channel *= gain;
            }
        }
    }
    fn is_finite(&self) -> bool {
        self.0.iter().all(|gain| gain.is_finite())
    }
    fn describe(&self) -> String {
        format!("proof tint {:?}", self.0)
    }
}

/// The developer capability proof. Its activation loads the palette's gains and keeps them until it
/// is deactivated.
pub struct CapabilitiesProofModule {
    descriptor: ModuleDescriptor,
    palette: Mutex<Option<[f32; 3]>>,
    activation_delay: Duration,
}

impl CapabilitiesProofModule {
    /// The proof module whose resource is pinned at `<endpoint_base>/proof-palette.bin`, the proof
    /// endpoint's palette. Nothing is contacted: registration validates the URL, which must be
    /// HTTPS or HTTP to loopback.
    pub fn new(endpoint_base: &str) -> Self {
        let url = format!(
            "{}{PROOF_PALETTE_PATH}",
            endpoint_base.trim().trim_end_matches('/')
        );
        let descriptor = serde_json::from_value(json!({
            "id": PROOF_MODULE,
            "title": "Capabilities proof",
            "hint": "Settings, consent, a resource and a worker task",
            "effects": [{"id": PROOF_EFFECT, "format": EFFECT_FORMAT, "stage": "color", "artifacts": true}],
            "actions": [
                {
                    "id": APPLY_PROOF_TINT,
                    "title": "Apply proof tint",
                    "notes": "Applies a tint generate-proof-tint published: its three linear gains multiply the channels. The first apply commits the layer and later ones update it in place",
                    "parameters": [{
                        "name": "artifact",
                        "kind": "artifact",
                        "required": true,
                        "notes": "a lightwell.capabilities.tint artifact: three little-endian f32 linear gains",
                    }],
                },
                {
                    "id": RESET_PROOF_TINT,
                    "title": "Reset proof tint",
                    "notes": "Returns the proof tint layer to neutral and drops its artifact reference; a no-op without a tint",
                    "parameters": [],
                },
            ],
            "controls": [
                {"kind": "task", "task": PROOF_TASK, "label": "Generate tint"},
                {"kind": "action", "action": RESET_PROOF_TINT, "label": "Reset tint"},
            ],
            "reset": {"action": RESET_PROOF_TINT},
            "canvas": null,
            "developer": true,
            "availability": {"kind": "available"},
            "settings": {
                "schema": 1,
                "fields": [
                    {
                        "id": "strength", "label": "Strength",
                        "help": "How far the tint moves the photo away from neutral",
                        "kind": "number", "min": 0.0, "max": 1.0, "step": 0.05, "precision": 2,
                        "required": false, "default": 0.5, "invalidates_activation": false,
                    },
                    {
                        "id": "input-file", "label": "Input file",
                        "help": "Any file; its SHA-256 picks a small factor of the tint",
                        "kind": "file", "mode": "file",
                        "required": false, "invalidates_activation": false,
                    },
                ],
                "profiles": {
                    "label": "Endpoint",
                    "max": 4,
                    "adapters": [{
                        "id": PROOF_ADAPTER,
                        "title": "Proof echo",
                        "auth": "bearer",
                        "data": ["sample-grid-8"],
                        "max_request_bytes": 4096,
                        "max_response_bytes": 4096,
                        "timeout_ms": 5000,
                        "retention": "The proof endpoint keeps nothing",
                        "cost": "free",
                    }],
                    "fields": [
                        {
                            "id": "endpoint", "label": "Endpoint", "kind": "endpoint",
                            "classes": ["remote", "loopback"],
                            "required": true, "invalidates_activation": false,
                        },
                        {
                            "id": "api-key", "label": "API key", "kind": "secret", "max_length": 256,
                            "required": true, "invalidates_activation": false,
                        },
                    ],
                },
            },
            "capabilities": [
                {
                    "id": "input", "kind": "read-user-file", "setting": "input-file",
                    "purpose": "Read the file you chose; its hash picks a small factor of the tint.",
                },
                {
                    "id": "echo", "kind": "remote-image-request", "adapter": PROOF_ADAPTER,
                    "data": "sample-grid-8",
                    "purpose": "Send an 8 × 8 grid of this photo's colours to the proof endpoint, which answers a tint.",
                },
                {
                    "id": "palette", "kind": "download-artifact", "resource": PROOF_RESOURCE,
                    "purpose": "Download the proof palette the activation loads.",
                },
            ],
            "resources": [{
                "id": PROOF_RESOURCE,
                "title": "Proof palette",
                "version": PROOF_RESOURCE_VERSION,
                "url": url,
                "bytes": PROOF_PALETTE.len(),
                "sha256": PROOF_PALETTE_SHA256,
                "format": "lightwell-proof-palette",
                "license": "GPL-3.0-or-later",
                "provenance": "Generated by Lightwell's capability proof",
            }],
            "activation": {
                "requires_settings": [],
                "requires_resources": [PROOF_RESOURCE],
                "notes": "Loads the proof palette's three gains.",
            },
            "tasks": [{
                "id": PROOF_TASK,
                "title": "Generate proof tint",
                "notes": "Reads the input file, sends this photo's sample grid to the profile's endpoint and publishes a tint of three linear gains for apply-proof-tint.",
                "asset": true,
                "profile": true,
                "requires_active": true,
                "uses": ["input", "echo"],
                "parameters": [],
                "apply": {"action": APPLY_PROOF_TINT, "parameter": "artifact"},
            }],
        }))
        .expect("the proof descriptor's shape is static");
        Self {
            descriptor,
            palette: Mutex::new(None),
            activation_delay: Duration::ZERO,
        }
    }

    /// Make the activation take at least `delay` after it loads the palette, checking its context
    /// about every 10 ms, so a test can observe and cancel a running activation.
    pub fn with_activation_delay(mut self, delay: Duration) -> Self {
        self.activation_delay = delay;
        self
    }

    /// Whether an activation's palette is loaded.
    pub fn is_loaded(&self) -> bool {
        lock(&self.palette).is_some()
    }

    /// A stored payload: `{}` for neutral, or `{"artifact": <id>}`.
    fn payload(effect_id: &str, format: u32, payload: &Value) -> Result<Option<ArtifactId>, Error> {
        if effect_id != PROOF_EFFECT {
            return Err(Error::new(
                ErrorKind::Incompatible,
                format!("unavailable effect {effect_id}"),
            ));
        }
        if format != EFFECT_FORMAT {
            return Err(Error::new(
                ErrorKind::Incompatible,
                format!("unsupported effect format {format}"),
            ));
        }
        let object = payload
            .as_object()
            .ok_or_else(|| validation("a proof tint payload is an object"))?;
        match (object.len(), object.get("artifact")) {
            (0, _) => Ok(None),
            (1, Some(Value::String(id))) => ArtifactId::parse(id.as_str()).map(Some),
            _ => Err(validation(
                "a proof tint payload is {} or {\"artifact\": <artifact id>}",
            )),
        }
    }

    fn artifact(parameters: &Map<String, Value>) -> Result<ArtifactId, Error> {
        ArtifactId::parse(
            parameters
                .get("artifact")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    }

    /// The gains of one generation: the endpoint's answer over 255, the palette and the input's
    /// factor multiplied per channel, moved from neutral by `strength`, all in `f32`.
    fn gains(rgb: [u8; 3], palette: [f32; 3], factor: f32, strength: f32) -> [f32; 3] {
        let mut gains = [1.0; 3];
        for channel in 0..3 {
            let target = f32::from(rgb[channel]) / 255.0 * palette[channel] * factor;
            gains[channel] = 1.0 + strength * (target - 1.0);
        }
        gains
    }
}

impl ToolModule for CapabilitiesProofModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }

    fn single_layer(&self, effect_id: &str) -> bool {
        effect_id == PROOF_EFFECT
    }

    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        let parameters = match action_id {
            APPLY_PROOF_TINT => {
                let mut normalized = Map::new();
                normalized.insert(
                    "artifact".into(),
                    json!(Self::artifact(parameters)?.as_str()),
                );
                normalized
            }
            RESET_PROOF_TINT => Map::new(),
            _ => return Err(validation(format!("unknown action {action_id}"))),
        };
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters,
        })
    }

    /// Apply commits the layer the first time and updates it in place afterwards. Reset updates a
    /// tinted layer to the neutral payload with no artifact, which keeps the layer's identity and
    /// renders as nothing; it is a no-op when there is no layer or it is already neutral.
    fn plan(&self, input: &ActionInput, context: &StageContext<'_>) -> Result<ActionPlan, Error> {
        let mut layers = context
            .layers
            .iter()
            .filter(|layer| layer.effect_id == PROOF_EFFECT);
        let existing = layers.next();
        if layers.next().is_some() {
            return Err(validation("ambiguous proof tint layers"));
        }
        match input.action_id.as_str() {
            APPLY_PROOF_TINT => {
                let artifact = Self::artifact(&input.parameters)?;
                let payload = json!({"artifact": artifact.as_str()});
                Ok(match existing {
                    Some(layer)
                        if layer.payload == payload && layer.artifacts == [artifact.clone()] =>
                    {
                        ActionPlan::NoOp
                    }
                    Some(layer) => ActionPlan::Update(Layer {
                        payload,
                        artifacts: vec![artifact],
                        ..layer.clone()
                    }),
                    None => ActionPlan::Commit(Layer {
                        id: LayerId::new(),
                        effect_id: PROOF_EFFECT.into(),
                        effect_format: EFFECT_FORMAT,
                        payload,
                        artifacts: vec![artifact],
                    }),
                })
            }
            RESET_PROOF_TINT => Ok(match existing {
                Some(layer) if layer.payload != json!({}) || !layer.artifacts.is_empty() => {
                    ActionPlan::Update(Layer {
                        payload: json!({}),
                        artifacts: Vec::new(),
                        ..layer.clone()
                    })
                }
                _ => ActionPlan::NoOp,
            }),
            other => Err(validation(format!("unknown action {other}"))),
        }
    }

    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        Self::payload(effect_id, format, payload).map(|_| ())
    }

    fn describe_layer(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<String, Error> {
        Ok(match Self::payload(effect_id, format, payload)? {
            Some(_) => "Proof tint".into(),
            None => "Proof tint (neutral)".into(),
        })
    }

    fn values(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<Map<String, Value>, Error> {
        let mut values = Map::new();
        if let Some(artifact) = Self::payload(effect_id, format, payload)? {
            values.insert("artifact".into(), json!(artifact.as_str()));
        }
        Ok(values)
    }

    /// A neutral payload is no processing at all. A tint needs its artifact's bytes, which only
    /// [`ToolModule::compile_bound`] receives.
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        _: Stage,
    ) -> Result<Processing, Error> {
        match Self::payload(effect_id, format, payload)? {
            None => Ok(Processing::Color(ColorOperation::neutral())),
            Some(artifact) => Err(validation(format!(
                "a proof tint is evaluated with the bytes of artifact {artifact}"
            ))),
        }
    }

    /// One pointwise unit multiplying the linear channels by the artifact's three gains. The layer
    /// binds exactly the artifact its payload names, of the tint kind, holding three finite
    /// positive gains; anything else is refused.
    fn compile_bound(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        _: Stage,
        artifacts: &[Arc<PreparedArtifact>],
    ) -> Result<Processing, Error> {
        let Some(named) = Self::payload(effect_id, format, payload)? else {
            return Err(validation("a neutral proof tint references no artifact"));
        };
        let [artifact] = artifacts else {
            return Err(validation("a proof tint binds exactly one artifact"));
        };
        if artifact.id != named {
            return Err(validation(format!(
                "a proof tint names {named} but binds {}",
                artifact.id
            )));
        }
        if artifact.kind != PROOF_TINT_KIND {
            return Err(validation(format!(
                "artifact {} is a {}, not a {PROOF_TINT_KIND}",
                artifact.id, artifact.kind
            )));
        }
        if artifact.bytes.len() != TINT_BYTES {
            return Err(validation(format!(
                "artifact {} holds {} bytes, not three gains",
                artifact.id,
                artifact.bytes.len()
            )));
        }
        let gains = gains_at(&artifact.bytes, 0).ok_or_else(|| {
            validation(format!(
                "artifact {} holds gains that are not finite and positive",
                artifact.id
            ))
        })?;
        Ok(Processing::Color(ColorOperation::new(vec![Arc::new(
            Tint(gains),
        )])))
    }

    fn activate(&self, context: &ModuleContext) -> Result<(), Error> {
        context.progress(Some(0.0), "loading the proof palette");
        let gains = read_palette(context.resource_path(PROOF_RESOURCE)?)?;
        let deadline = Instant::now() + self.activation_delay;
        loop {
            context.checkpoint()?;
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
        *lock(&self.palette) = Some(gains);
        context.progress(Some(1.0), "loaded");
        Ok(())
    }

    fn deactivate(&self) {
        *lock(&self.palette) = None;
    }

    fn validate_resource(&self, resource_id: &str, path: &Path) -> Result<(), Error> {
        if resource_id != PROOF_RESOURCE {
            return Err(validation(format!("unknown resource {resource_id}")));
        }
        read_palette(path).map(|_| ())
    }

    /// Read the input file, send the sample grid, and publish the tint its answer, the palette, the
    /// input's factor and the strength setting make. Returns `{gains, input_sha256,
    /// endpoint_origin, resource_version}`; the artifact is the job's.
    fn run_task(
        &self,
        task_id: &str,
        _: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        if task_id != PROOF_TASK {
            return Err(validation(format!("unknown task {task_id}")));
        }
        let palette = (*lock(&self.palette)).ok_or_else(|| {
            Error::new(
                ErrorKind::NotReady,
                "the proof palette is not loaded; activate the module",
            )
        })?;
        context.progress(Some(0.1), "reading the input file");
        let digest = Sha256::digest(context.read_file("input")?);
        context.progress(Some(0.4), "asking the proof endpoint");
        let rgb = parse_answer(&context.send("echo")?)?;
        context.checkpoint()?;
        let strength = context
            .value("strength")
            .and_then(Value::as_f64)
            .ok_or_else(|| Error::new(ErrorKind::NotReady, "setting strength has no value"))?
            as f32;
        let gains = Self::gains(rgb, palette, input_factor(&digest), strength);
        if !gains.iter().all(|gain| gain.is_finite() && *gain > 0.0) {
            return Err(validation("the tint's gains are not finite and positive"));
        }
        context.progress(Some(0.8), "publishing the tint");
        let bytes: Vec<u8> = gains.iter().flat_map(|gain| gain.to_le_bytes()).collect();
        context.publish_artifact(
            &bytes,
            ArtifactMeta {
                kind: PROOF_TINT_KIND.into(),
                width: None,
                height: None,
                colour: Some("linear-srgb".into()),
            },
        )?;
        Ok(json!({
            "gains": gains,
            "input_sha256": format!("{digest:x}"),
            "endpoint_origin": context.origin("echo")?,
            "resource_version": PROOF_RESOURCE_VERSION,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModuleRegistry, artifacts::ArtifactId};

    fn artifact(kind: &str, bytes: Vec<u8>) -> Arc<PreparedArtifact> {
        let id = ArtifactId::for_hash(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
        Arc::new(PreparedArtifact {
            id,
            kind: kind.into(),
            width: None,
            height: None,
            colour: None,
            bytes: bytes.into(),
        })
    }

    fn tint(gains: [f32; 3]) -> Vec<u8> {
        gains.iter().flat_map(|gain| gain.to_le_bytes()).collect()
    }

    const STAGE: Stage = Stage {
        width: 4,
        height: 4,
    };

    #[test]
    fn the_palette_is_pinned_at_its_bytes_and_the_descriptor_registers() {
        assert_eq!(
            format!("{:x}", Sha256::digest(PROOF_PALETTE)),
            PROOF_PALETTE_SHA256
        );
        assert_eq!(&PROOF_PALETTE[..8], b"LWPAL001");
        assert_eq!(parse_palette(&PROOF_PALETTE).unwrap(), PROOF_PALETTE_GAINS);
        let module = CapabilitiesProofModule::new("http://127.0.0.1:9/");
        let descriptor = module.descriptor();
        descriptor.validate().unwrap();
        assert_eq!(
            descriptor.resources[0].url,
            "http://127.0.0.1:9/proof-palette.bin"
        );
        assert!(descriptor.developer);
        let mut registry = ModuleRegistry::builtin();
        registry.register(Arc::new(module)).unwrap();
        assert!(registry.task(PROOF_TASK).is_some());
        // A resource URL the transport would refuse fails registration, not construction.
        let mut registry = ModuleRegistry::builtin();
        let error = registry
            .register(Arc::new(CapabilitiesProofModule::new("http://example.com")))
            .unwrap_err();
        assert!(error.detail.contains("proof-palette"), "{}", error.detail);
    }

    #[test]
    fn a_palette_or_an_answer_that_is_not_the_declared_shape_is_refused() {
        let mut wrong_magic = PROOF_PALETTE;
        wrong_magic[0] = b'X';
        for bytes in [
            &wrong_magic[..],
            &PROOF_PALETTE[..19],
            &palette_bytes([1.0, f32::NAN, 1.0])[..],
            &palette_bytes([1.0, 0.0, 1.0])[..],
            &palette_bytes([1.0, -1.0, 1.0])[..],
        ] {
            assert_eq!(
                parse_palette(bytes).unwrap_err().kind,
                ErrorKind::Validation
            );
        }
        assert_eq!(
            parse_answer(br#"{"rgb":[1,128,255]}"#).unwrap(),
            [1, 128, 255]
        );
        for answer in [
            &br#"{"rgb":[0,128,255]}"#[..],
            br#"{"rgb":[1,128,256]}"#,
            br#"{"rgb":[1,128]}"#,
            br#"{"rgb":[1,128,2.5]}"#,
            br#"{"rgb":[1,2,3],"extra":1}"#,
            br#"[1,2,3]"#,
            b"not json",
        ] {
            assert_eq!(
                parse_answer(answer).unwrap_err().kind,
                ErrorKind::Validation
            );
        }
        assert_eq!(input_factor(&[0]), 0.9);
        assert_eq!(input_factor(&[255]), 1.1);
    }

    #[test]
    fn compile_multiplies_linear_channels_by_the_one_bound_tint_and_refuses_anything_else() {
        let module = CapabilitiesProofModule::new("http://127.0.0.1:9");
        let neutral = module.compile(PROOF_EFFECT, 1, &json!({}), STAGE).unwrap();
        assert!(matches!(neutral, Processing::Color(operation) if operation.is_empty()));
        let good = artifact(PROOF_TINT_KIND, tint([0.5, 1.0, 2.0]));
        let payload = json!({"artifact": good.id.as_str()});
        assert_eq!(
            module
                .compile(PROOF_EFFECT, 1, &payload, STAGE)
                .unwrap_err()
                .kind,
            ErrorKind::Validation,
            "a tint without its bytes is refused"
        );
        let Processing::Color(operation) = module
            .compile_bound(
                PROOF_EFFECT,
                1,
                &payload,
                STAGE,
                std::slice::from_ref(&good),
            )
            .unwrap()
        else {
            panic!("a colour operation");
        };
        let mut row = [[0.2_f32, 0.4, 0.3]];
        operation.units()[0].apply_row(0, 0, &mut row);
        assert_eq!(row, [[0.1, 0.4, 0.6]]);
        let wrong_kind = artifact("tint", tint([0.5, 1.0, 2.0]));
        let short = artifact(PROOF_TINT_KIND, vec![0; 8]);
        let infinite = artifact(PROOF_TINT_KIND, tint([1.0, f32::INFINITY, 1.0]));
        for (payload, bound) in [
            (
                json!({"artifact": wrong_kind.id.as_str()}),
                vec![wrong_kind.clone()],
            ),
            (json!({"artifact": short.id.as_str()}), vec![short.clone()]),
            (
                json!({"artifact": infinite.id.as_str()}),
                vec![infinite.clone()],
            ),
            (payload.clone(), vec![good.clone(), good.clone()]),
            (payload.clone(), vec![short.clone()]),
            (json!({}), vec![good.clone()]),
        ] {
            let error = module
                .compile_bound(PROOF_EFFECT, 1, &payload, STAGE, &bound)
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{}", error.detail);
        }
        for (effect, format, payload) in [
            ("lightwell.other", 1, json!({})),
            (PROOF_EFFECT, 2, json!({})),
            (PROOF_EFFECT, 1, json!({"artifact": "artifact-nope"})),
            (PROOF_EFFECT, 1, json!({"strength": 1})),
        ] {
            assert!(module.validate_payload(effect, format, &payload).is_err());
        }
    }
}
