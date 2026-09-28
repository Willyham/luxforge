//! The capability proof end to end through the catalog owner's JSON methods, exactly as a client
//! drives it: discovery, settings and a profile, consent for the resource download, install,
//! consent for the per-asset remote send, the task with its sample grid, the published
//! artifact, its application and the render, against a [`ProofEndpoint`] answered in process by
//! the in-memory transport, isolated directories and an in-memory secret store. Nothing here opens
//! a socket.
use super::{
    data::SAMPLE_GRID_BYTES,
    grants::{DENY, GRANT, REVOKE},
    host::{HostConfig, STATUS},
    resources::{INSTALL, REMOVE},
    secrets::MemorySecretStore,
    settings::{CREATE_PROFILE, READ, SET, SET_SECRET},
    testing::{enveloped, proof_transport, temp},
};
use crate::{
    ApiFailure, ApiRequest, ApiResponse, ArtifactId, AssetId, CapabilitiesProofModule,
    CapabilityModule, ClientAuthority, ClientId, EditorService, EntryId, Error, Layer, LayerUpdate,
    ModuleDescriptor, ModuleRegistry, OwnerHandle, PROOF_PALETTE_GAINS, PROOF_TASK,
    ParameterDescriptor, Processing, Stage, StageContext, ToolModule,
    capabilities::{context::ModuleContext, descriptor::TaskDescriptor},
    colour::srgb::{decode_u8, quantize_pixel},
    jobs::{JOB_CANCEL, JOB_READ},
    modules::{ActionInput, ActionPlan},
    redact_request,
};
use luxforge_testbase::wait_for;
use luxforge_testkit::ProofEndpoint;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread::JoinHandle,
    time::Instant,
};

const MODULE: &str = "luxforge.capabilities";
const TASK: &str = "task.generate-proof-tint";
const STRENGTH: f64 = 0.75;

/// A module whose one task publishes an artifact and then fails or succeeds as asked, so a test can
/// see that only a successful task's artifact is recorded.
struct Publisher(ModuleDescriptor);

const PUBLISHER_TASK: &str = "publish-then-answer";
const PUBLISHED: &[u8] = b"published by a task";

impl Publisher {
    fn shared() -> Arc<dyn ToolModule> {
        let descriptor = ModuleDescriptor {
            id: "test.publisher".into(),
            title: "Publisher".into(),
            tasks: vec![TaskDescriptor {
                id: PUBLISHER_TASK.into(),
                title: "Publish then answer".into(),
                notes: "publishes an artifact, then fails when asked to".into(),
                asset: false,
                profile: false,
                uses: Vec::new(),
                parameters: vec![
                    ParameterDescriptor::boolean("fail")
                        .default(true)
                        .notes("fail after publishing"),
                ],
                apply: None,
            }],
            ..ModuleDescriptor::default()
        };
        descriptor.validate().unwrap();
        Arc::new(Self(descriptor))
    }
}

impl ToolModule for Publisher {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }
    fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Err(Error::validation(format!("unknown action {action_id}")))
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::NoOp)
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe(&self, _: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
        Ok(crate::LayerReport::new("none"))
    }
    fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
        Err(Error::internal("the publisher never renders"))
    }
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        Some(self)
    }
}

impl CapabilityModule for Publisher {
    fn run_task(
        &self,
        _: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        let id = context.publish_artifact(
            PUBLISHED,
            crate::artifacts::ArtifactMeta {
                kind: "test".into(),
                width: None,
                height: None,
                colour: None,
            },
        )?;
        if parameters["fail"] == json!(true) {
            return Err(Error::decode("failed after publishing"));
        }
        Ok(json!({"published": id}))
    }
}

/// A catalog with two imported photos, a settings directory and a resource directory under one
/// temporary root, a proof endpoint whose key is a sentinel, and the registry the owner is started
/// with.
struct Fixture {
    root: PathBuf,
    endpoint: Arc<ProofEndpoint>,
    key: String,
    secrets: Arc<MemorySecretStore>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = temp(name);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let key = format!("SENTINEL-{}", uuid::Uuid::new_v4().simple());
        Self {
            endpoint: Arc::new(ProofEndpoint::in_process(&key)),
            key,
            root,
            secrets: Arc::new(MemorySecretStore::new()),
        }
    }

    fn catalog(&self) -> PathBuf {
        self.root.join("catalog.sqlite")
    }

    fn registry(&self) -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(CapabilitiesProofModule::new(
                &self.endpoint.base_url(),
            )))
            .unwrap();
        registry.register(Publisher::shared()).unwrap();
        Arc::new(registry)
    }

    fn host(&self) -> HostConfig {
        HostConfig {
            config_dir: Some(self.root.join("config").join("modules")),
            resource_dir: Some(self.root.join("data").join("modules").join("resources")),
            secrets: self.secrets.clone(),
            transport: proof_transport(self.endpoint.clone()),
            ..HostConfig::unconfigured()
        }
    }

    /// Import the two fixture photos with a direct service before any owner runs, so the owner's
    /// first sample of either needs a preparation job.
    fn import(&self) -> [AssetId; 2] {
        let photo = |name: &str| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/s0")
                .join(name)
        };
        let mut service = EditorService::open_with(&self.catalog(), self.registry()).unwrap();
        ["orientation-1.jpg", "orientation-6.jpg"]
            .map(|name| service.import(&photo(name)).unwrap().asset.id)
    }

    fn start(&self) -> Owner {
        let (handle, join) =
            OwnerHandle::start_with_host(&self.catalog(), self.registry(), self.host()).unwrap();
        let edit = handle.register();
        let admin = handle.register_with(ClientAuthority::Permissions);
        Owner {
            handle,
            join: Some(join),
            edit,
            admin,
            next: Cell::new(0),
            observed: RefCell::new(Vec::new()),
        }
    }

    /// Every file under the root, read whole.
    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![self.root.clone()];
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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A running owner with an edit client and a permission client, recording every response.
struct Owner {
    handle: OwnerHandle,
    join: Option<JoinHandle<()>>,
    edit: ClientId,
    admin: ClientId,
    next: Cell<u64>,
    observed: RefCell<Vec<String>>,
}

impl Owner {
    fn call(&self, client: ClientId, method: &str, params: Value) -> ApiResponse {
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

    fn ok_as(&self, client: ClientId, method: &str, params: Value) -> Value {
        let response = self.call(client, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.ok_as(self.edit, method, params)
    }

    fn fail(&self, method: &str, params: Value) -> ApiFailure {
        self.call(self.edit, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to fail"))
    }

    fn revision(&self) -> u64 {
        self.ok(READ, json!({"module_id": MODULE}))["revision"]
            .as_u64()
            .unwrap()
    }

    fn mutation(&self) -> Value {
        json!({
            "expected_revision": self.revision(),
            "request_id": format!("settings-{}", uuid::Uuid::new_v4().simple()),
            "actor": "test",
        })
    }

    /// Commit settings of the module, or of one of its profiles.
    fn set(&self, profile: Option<&str>, values: Value) -> Value {
        let mut params =
            json!({"module_id": MODULE, "values": values, "mutation": self.mutation()});
        if let Some(profile) = profile {
            params["profile_id"] = json!(profile);
        }
        self.ok(SET, params)
    }

    fn set_key(&self, profile: &str, key: &str) {
        self.ok(
            SET_SECRET,
            json!({
                "module_id": MODULE, "profile_id": profile, "setting": "api-key", "value": key,
                "mutation": self.mutation(),
            }),
        );
    }

    /// Grant the scope a `consent-required` failure named, as the permission client.
    fn grant(&self, refused: &ApiFailure) -> Value {
        assert_eq!(refused.code, "consent-required", "{}", refused.message);
        let consent = &refused.data.as_ref().expect("consent data")["consent"];
        self.ok_as(
            self.admin,
            GRANT,
            json!({
                "module_id": consent["module_id"],
                "capability": consent["capability"],
                "scope": consent["scope"],
            }),
        )["grant"]
            .clone()
    }

    /// Wait for a capability job to finish and return its record.
    fn finished(&self, job_id: &Value) -> Value {
        wait_for(&format!("job {job_id} to finish"), || {
            let job = self.ok(JOB_READ, json!({"job_id": job_id}));
            (!matches!(job["status"].as_str(), Some("queued" | "running"))).then_some(job)
        })
    }

    /// Ask for the task, waiting through any preparation job its sampling asks for, as a client
    /// does. Returns the failure or the queued answer.
    fn task(&self, asset: &AssetId, profile: &str) -> Result<Value, ApiFailure> {
        self.task_with(json!({"asset_id": asset, "profile_id": profile}))
    }

    /// [`Self::task`] with these exact parameters: a request that names its envelope asks again
    /// under the same `request_id`, as a client that waits for a preparation does.
    fn task_with(&self, params: Value) -> Result<Value, ApiFailure> {
        for _ in 0..4 {
            let response = self.call(self.edit, TASK, params.clone());
            match response.error {
                None => return Ok(response.result.expect("a result")),
                Some(error) if error.code == "preparation-required" => {
                    let job = error.job_id.expect("a preparation names its job");
                    let status = self.finished(&json!(job));
                    assert_eq!(status["status"], "ready", "{status}");
                }
                Some(error) => return Err(error),
            }
        }
        panic!("the task kept asking for preparation");
    }

    fn state(&self, asset: &AssetId) -> Value {
        self.ok("asset.state", json!({"asset_id": asset}))
    }

    fn apply(&self, asset: &AssetId, artifact: &Value) -> Value {
        let revision = self.state(asset)["revision"].clone();
        self.ok(
            "edit.apply-proof-tint",
            json!({
                "asset_id": asset,
                "artifact": artifact,
                "mutation": {"expected_revision": revision, "request_id": format!("apply-{}", self.next.get()), "actor": "test"},
            }),
        )
    }

    fn sample(&self, asset: &AssetId, x: u32, y: u32) -> Value {
        self.ok("render.sample", json!({"asset_id": asset, "x": x, "y": y}))
    }

    fn events(&self) -> Vec<String> {
        self.ok("events.since", json!({"after": 0}))["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["method"].as_str().unwrap().to_owned())
            .collect()
    }

    fn stop(mut self) -> Vec<String> {
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

/// What a journey to a ready module leaves: the photos, the profile and the grants' identities.
struct Ready {
    assets: [AssetId; 2],
    profile: String,
}

/// Settings, a profile with the endpoint and key, the palette installed under its consent, and the
/// remote grant for the first photo.
fn ready(fixture: &Fixture, owner: &Owner, assets: [AssetId; 2]) -> Ready {
    owner.set(None, json!({"strength": STRENGTH}));
    let created = owner.ok(
        CREATE_PROFILE,
        json!({"module_id": MODULE, "adapter": "proof-echo", "label": "Local", "mutation": owner.mutation()}),
    );
    let profile = created["profile"]["id"].as_str().unwrap().to_owned();
    owner.set(
        Some(&profile),
        json!({"endpoint": fixture.endpoint.generate_url()}),
    );
    owner.set_key(&profile, &fixture.key);
    let install = json!({"module_id": MODULE, "resource_id": "proof-palette"});
    owner.grant(&owner.fail(INSTALL, install.clone()));
    let installed = owner.ok(INSTALL, install);
    assert_eq!(owner.finished(&installed["job_id"])["status"], "ready");
    owner.grant(&owner.task(&assets[0], &profile).unwrap_err());
    Ready { assets, profile }
}

/// The gains the proof task publishes, computed from the spec: the endpoint's answer over 255 and
/// the palette multiplied per channel, moved from neutral by the strength.
fn expected_gains(rgb: [u8; 3]) -> [f32; 3] {
    std::array::from_fn(|channel| {
        let target = f32::from(rgb[channel]) / 255.0 * PROOF_PALETTE_GAINS[channel];
        1.0 + STRENGTH as f32 * (target - 1.0)
    })
}

fn gains_of(result: &Value) -> [f32; 3] {
    std::array::from_fn(|channel| result["gains"][channel].as_f64().unwrap() as f32)
}

/// One source pixel tinted in linear light and quantized at the output boundary as the host does.
fn tinted(source: [u8; 4], gains: [f32; 3]) -> [u8; 4] {
    let linear = |channel: usize| decode_u8(source[channel]) as f32 * gains[channel];
    let [red, green, blue] = quantize_pixel([linear(0), linear(1), linear(2)]);
    [red, green, blue, source[3]]
}

/// Every pixel of the asset's current render equals the untinted entry's pixel tinted by `gains`,
/// through a direct service over the catalog the owner left: a reopen.
fn assert_tint_renders(
    fixture: &Fixture,
    asset: &AssetId,
    untinted: &EntryId,
    gains: [f32; 3],
) -> usize {
    let mut service = EditorService::open_with(&fixture.catalog(), fixture.registry()).unwrap();
    service
        .prepare(&service.entry_needs(asset, Some(untinted)).unwrap())
        .unwrap();
    let source = service.render_entry(asset, untinted).unwrap();
    service
        .prepare(&service.entry_needs(asset, None).unwrap())
        .unwrap();
    let rendered = service.render_current(asset).unwrap();
    assert_eq!(
        (rendered.width, rendered.height),
        (source.width, source.height)
    );
    let mut changed = 0;
    for (index, (rendered, source)) in rendered
        .rgba
        .chunks_exact(4)
        .zip(source.rgba.chunks_exact(4))
        .enumerate()
    {
        let source: [u8; 4] = source.try_into().unwrap();
        assert_eq!(rendered, tinted(source, gains), "pixel {index}");
        changed += usize::from(rendered != source);
    }
    changed
}

#[test]
fn the_proof_module_and_its_task_method_are_discovered_without_any_side_effect() {
    let fixture = Fixture::new("proof-discovery");
    let owner = fixture.start();
    let modules = owner.ok("module.list", json!({}));
    let listed = modules["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == MODULE)
        .expect("the proof module is listed")
        .clone();
    let declared = serde_json::to_value(
        CapabilitiesProofModule::new(&fixture.endpoint.base_url()).descriptor(),
    )
    .unwrap();
    assert_eq!(listed, declared, "exactly the declarations");
    assert_eq!(listed["developer"], true);
    assert_eq!(listed["title"], "Capabilities proof");
    assert_eq!(
        listed["controls"],
        json!([
            {"kind": "task", "task": PROOF_TASK, "label": "Generate tint"},
            {"kind": "action", "action": "reset-proof-tint", "label": "Reset tint", "preset": {}},
        ])
    );
    let schema = owner.ok("schema.list", json!({}));
    let task = &schema["methods"][TASK];
    assert_eq!(task["mutates"], true);
    assert_eq!(task["mutation"], "request");
    assert_eq!(
        task["required"],
        json!(["mutation", "asset_id", "profile_id"])
    );
    assert_eq!(task["optional"], json!({}));
    assert!(
        task["notes"]
            .as_str()
            .unwrap()
            .contains("{job_id, status, deduplicated}")
    );
    assert_eq!(
        schema["methods"]["task.publish-then-answer"]["optional"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["fail"]
    );
    assert_eq!(owner.handle.lane_threads(), 0);
    let observed = owner.stop();
    assert!(observed.iter().all(|text| !text.contains("\"error\"")));
    assert!(fixture.endpoint.requests().is_empty(), "no network");
    assert_eq!(fixture.secrets.calls().total(), 0, "no secret store call");
    assert!(!fixture.root.join("config").exists());
    assert!(!fixture.root.join("data").exists());
}

#[test]
fn the_capability_path_runs_from_install_to_an_applied_tint_that_renders_after_reopen() {
    let fixture = Fixture::new("proof-journey");
    let assets = fixture.import();
    let owner = fixture.start();
    owner.set(None, json!({"strength": STRENGTH}));
    let created = owner.ok(
        CREATE_PROFILE,
        json!({"module_id": MODULE, "adapter": "proof-echo", "label": "Local", "mutation": owner.mutation()}),
    );
    let profile = created["profile"]["id"].as_str().unwrap().to_owned();
    owner.set(
        Some(&profile),
        json!({"endpoint": fixture.endpoint.generate_url()}),
    );
    let key_request = json!({
        "module_id": MODULE, "profile_id": profile, "setting": "api-key", "value": fixture.key,
        "mutation": owner.mutation(),
    });
    let keyed = owner.ok(SET_SECRET, key_request.clone());
    assert_eq!(keyed["settings"]["profiles"][0]["status"], "ready");
    // The task reads its palette, so nothing runs it before the palette is installed.
    let refused = owner.task(&assets[0], &profile).unwrap_err();
    assert_eq!(refused.code, "not-ready");
    assert_eq!(
        refused.data.unwrap()["requirements"],
        json!([{"kind": "resource", "id": "proof-palette", "state": "not-installed"}])
    );

    // The download needs consent, which only the permission client can give.
    let install = json!({"module_id": MODULE, "resource_id": "proof-palette"});
    let refused = owner.fail(INSTALL, install.clone());
    assert_eq!(refused.code, "consent-required");
    let consent = &refused.data.as_ref().unwrap()["consent"];
    assert_eq!(consent["capability"], "palette");
    assert_eq!(
        consent["scope"],
        json!({"resource": "proof-palette", "version": "1", "origin": fixture.endpoint.base_url()})
    );
    assert_eq!(consent["disclosure"]["bytes"], 20);
    assert_eq!(consent["disclosure"]["license"], "GPL-3.0-or-later");
    assert_eq!(
        owner
            .call(
                owner.edit,
                GRANT,
                json!({"module_id": MODULE, "capability": "palette", "scope": consent["scope"], "mutation": {"request_id": "edit-grant", "actor": "test"}}),
            )
            .error
            .unwrap()
            .code,
        "forbidden"
    );
    owner.grant(&refused);
    let installed = owner.ok(INSTALL, install);
    let job = owner.finished(&installed["job_id"]);
    assert_eq!(job["status"], "ready", "{job}");

    // The task asks for consent to send this photo's grid.
    let asset = &assets[0];
    let refused = owner.task(asset, &profile).unwrap_err();
    assert_eq!(refused.code, "consent-required");
    let consent = &refused.data.as_ref().unwrap()["consent"];
    assert_eq!(consent["capability"], "echo");
    assert_eq!(consent["denied"], false);
    assert_eq!(
        consent["scope"],
        json!({
            "profile_id": profile, "adapter": "proof-echo", "origin": fixture.endpoint.base_url(),
            "data": "sample-grid-8", "asset_id": asset,
        })
    );
    let disclosure = &consent["disclosure"];
    assert_eq!(
        disclosure["destination"],
        json!(fixture.endpoint.base_url())
    );
    assert_eq!(disclosure["bytes"], SAMPLE_GRID_BYTES);
    assert_eq!(disclosure["retention"], "The proof endpoint keeps nothing");
    assert_eq!(disclosure["cost"], "free");
    assert!(
        fixture
            .endpoint
            .requests()
            .iter()
            .all(|request| request.path != "/generate"),
        "nothing is sent before consent"
    );
    owner.grant(&refused);
    let untinted = owner.state(asset)["current_entry"]["id"].clone();
    let queued = owner.task(asset, &profile).unwrap();
    assert!(queued["job_id"].is_string());
    assert!(
        owner
            .observed
            .borrow()
            .iter()
            .any(|text| text.contains("\"preparation-required\"") && text.contains("\"job_id\"")),
        "sampling a photo the owner has not prepared queued its preparation first"
    );
    let job = owner.finished(&queued["job_id"]);
    assert_eq!(job["status"], "ready", "{job}");
    assert_eq!(job["kind"], "task");
    let result = &job["result"]["result"];
    let artifact = job["result"]["artifacts"][0].clone();
    assert_eq!(job["result"]["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["endpoint_origin"],
        json!(fixture.endpoint.base_url())
    );
    assert_eq!(result["resource_version"], "1");

    // The endpoint received exactly the disclosed body: the grid of the photo's current entry.
    let sent = fixture
        .endpoint
        .requests()
        .into_iter()
        .rfind(|request| request.path == "/generate")
        .unwrap();
    assert!(sent.authorized);
    assert_eq!(sent.status, 200);
    assert_eq!(sent.body_bytes, SAMPLE_GRID_BYTES);
    assert_eq!(sent.header("content-type"), Some("application/json"));
    let first = owner.sample(asset, 0, 0);
    let (width, height) = (
        first["width"].as_u64().unwrap(),
        first["height"].as_u64().unwrap(),
    );
    let mut grid = Vec::new();
    for row in 0..8 {
        for column in 0..8 {
            let x = (2 * column + 1) * width / 16;
            let y = (2 * row + 1) * height / 16;
            let rgba = owner.sample(asset, x as u32, y as u32)["rgba"].clone();
            grid.push([0, 1, 2].map(|channel| rgba[channel].as_u64().unwrap() as u8));
        }
    }
    assert_eq!(sent.samples.as_deref(), Some(grid.as_slice()));
    let gains = expected_gains(sent.rgb.unwrap());
    assert_eq!(gains_of(result), gains);

    // The artifact is recorded before the job read succeeded, with the gains as its bytes.
    let artifact_id = ArtifactId::parse(artifact.as_str().unwrap()).unwrap();
    let bytes: Vec<u8> = gains.iter().flat_map(|gain| gain.to_le_bytes()).collect();
    assert_eq!(
        artifact_id.sha256(),
        format!("{:x}", Sha256::digest(&bytes))
    );
    let inspected = owner.ok("artifact.inspect", json!({"artifact_id": artifact}));
    assert_eq!(inspected["meta"]["kind"], "luxforge.capabilities.tint");
    assert_eq!(inspected["bytes"], 12);
    assert_eq!(inspected["module_id"], MODULE);
    assert_eq!(inspected["file"], "present");
    assert_eq!(inspected["references"], 0);

    // Applying it commits one layer; the render is the source tinted in linear light.
    let applied = owner.apply(asset, &artifact);
    assert_eq!(applied["outcome"], "applied", "{applied}");
    let sampled = owner.sample(asset, 5, 7)["rgba"].clone();
    let service_events = owner.events();
    assert!(service_events.contains(&TASK.to_owned()));
    assert!(service_events.contains(&"edit.apply-proof-tint".to_owned()));
    assert_eq!(
        owner.ok("artifact.inspect", json!({"artifact_id": artifact}))["references"],
        1
    );
    owner.ok(STATUS, json!({"module_id": MODULE}));
    let mut observed = owner.stop();
    let untinted = EntryId::parse(untinted.as_str().unwrap()).unwrap();
    assert!(assert_tint_renders(&fixture, asset, &untinted, gains) > 0);
    let mut service = EditorService::open_with(&fixture.catalog(), fixture.registry()).unwrap();
    service
        .prepare(&service.entry_needs(asset, Some(&untinted)).unwrap())
        .unwrap();
    let source = service.render_entry(asset, &untinted).unwrap();
    let index = (7 * source.width as usize + 5) * 4;
    let source_pixel: [u8; 4] = source.rgba[index..index + 4].try_into().unwrap();
    assert_eq!(sampled, json!(tinted(source_pixel, gains)));
    drop(service);

    // A reopened owner renders the committed tint from the artifact on disk.
    let owner = fixture.start();
    let reopened = owner.call(
        owner.edit,
        "render.sample",
        json!({"asset_id": asset, "x": 5, "y": 7}),
    );
    let reopened = match reopened.error {
        None => reopened.result.unwrap(),
        Some(error) => {
            assert_eq!(error.code, "preparation-required");
            let job = owner.finished(&json!(error.job_id.unwrap()));
            assert_eq!(job["status"], "ready", "{job}");
            owner.sample(asset, 5, 7)
        }
    };
    assert_eq!(reopened["rgba"], sampled);
    owner.ok(STATUS, json!({"module_id": MODULE}));
    observed.extend(owner.stop());

    // The sentinel key reached the endpoint and no response, event, error, job record, status or
    // file anywhere under the fixture.
    assert!(observed.len() > 30);
    for text in &observed {
        assert!(
            !text.contains(&fixture.key),
            "a response carries the key: {text}"
        );
    }
    for (path, bytes) in fixture.files() {
        assert!(
            !String::from_utf8_lossy(&bytes).contains(&fixture.key),
            "{} holds the key",
            path.display()
        );
    }
    assert!(!format!("{:?}", fixture.endpoint.requests()).contains(&fixture.key));
    let request = ApiRequest {
        id: "key".into(),
        method: SET_SECRET.into(),
        params: key_request,
        token: None,
    };
    let redacted = serde_json::to_string(&redact_request(&request)).unwrap();
    assert!(!redacted.contains(&fixture.key), "{redacted}");
    assert!(redacted.contains("<redacted>"));
}

#[test]
fn every_photo_needs_its_own_remote_grant_and_a_denial_is_reported() {
    let fixture = Fixture::new("proof-per-asset");
    let assets = fixture.import();
    let owner = fixture.start();
    let Ready { assets, profile } = ready(&fixture, &owner, assets);
    let queued = owner.task(&assets[0], &profile).unwrap();
    assert_eq!(owner.finished(&queued["job_id"])["status"], "ready");
    // The remote grant covers only the first photo.
    let refused = owner.task(&assets[1], &profile).unwrap_err();
    assert_eq!(refused.code, "consent-required");
    let consent = refused.data.unwrap()["consent"].clone();
    assert_eq!(consent["capability"], "echo");
    assert_eq!(consent["scope"]["asset_id"], json!(assets[1]));
    assert_eq!(consent["denied"], false);
    // "Don't allow" from any client is reported with the next request for that scope.
    owner.ok(
        DENY,
        json!({"module_id": MODULE, "capability": "echo", "scope": consent["scope"]}),
    );
    let refused = owner.task(&assets[1], &profile).unwrap_err();
    assert_eq!(refused.code, "consent-required");
    assert_eq!(refused.data.unwrap()["consent"]["denied"], true);
    let sent = fixture
        .endpoint
        .requests()
        .iter()
        .filter(|request| request.path == "/generate")
        .count();
    assert_eq!(sent, 1, "only the granted photo was sent");
    // Unknown parameters and a missing envelope are refused before anything else.
    assert_eq!(
        owner
            .fail(
                TASK,
                json!({"asset_id": assets[0], "profile_id": profile, "extra": 1})
            )
            .message,
        "unknown parameter extra for task generate-proof-tint"
    );
    assert_eq!(
        owner.fail(TASK, json!({"asset_id": assets[0]})).message,
        "missing field `profile_id`"
    );
    assert_eq!(
        owner
            .fail(
                TASK,
                json!({"asset_id": assets[0], "profile_id": "profile-missing"})
            )
            .message,
        "unknown profile profile-missing"
    );
    // A profile without its key is reported, before any grant is asked for.
    owner.ok(
        "module.settings.clear-secret",
        json!({"module_id": MODULE, "profile_id": profile, "setting": "api-key", "mutation": owner.mutation()}),
    );
    let refused = owner.task(&assets[0], &profile).unwrap_err();
    assert_eq!(refused.code, "not-ready");
    assert_eq!(
        refused.data.unwrap()["requirements"],
        json!([{"kind": "profile", "id": profile, "state": "missing-credentials"}])
    );
    owner.stop();
}

#[test]
fn revoking_the_remote_grant_mid_task_cancels_it_and_keeps_the_accepted_tint() {
    let fixture = Fixture::new("proof-revoke");
    let assets = fixture.import();
    let owner = fixture.start();
    let Ready { assets, profile } = ready(&fixture, &owner, assets);
    let asset = &assets[0];
    let first = owner.task(asset, &profile).unwrap();
    let first = owner.finished(&first["job_id"]);
    let accepted = first["result"]["artifacts"][0].clone();
    owner.apply(asset, &accepted);
    let before = owner.state(asset);
    let pixel = owner.sample(asset, 3, 4)["rgba"].clone();
    // The endpoint holds its answer, so the task is running inside its send when the grant goes.
    let generation = fixture.endpoint.generation();
    generation.shut();
    let sends = generation.reached();
    let queued = owner.task(asset, &profile).unwrap();
    generation.wait_reached(sends + 1, "the task's send");
    let grant = owner.ok("module.permission.list", json!({"module_id": MODULE}))["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|grant| grant["kind"] == "remote-image-request")
        .unwrap()["grant_id"]
        .clone();
    let revoked = owner.ok(REVOKE, json!({"grant_id": grant}));
    assert_eq!(revoked["cancelled_jobs"], json!([queued["job_id"]]));
    let job = owner.finished(&queued["job_id"]);
    assert!(
        generation.holding(),
        "the task stopped while its answer was still held"
    );
    assert_eq!(job["status"], "cancelled");
    assert_eq!(job["error"]["message"], "permission revoked");
    assert!(job.get("result").is_none());
    generation.open();
    // The recipe, its history and the accepted artifact are untouched.
    assert_eq!(owner.state(asset), before);
    assert_eq!(owner.sample(asset, 3, 4)["rgba"], pixel);
    assert_eq!(
        owner.ok("artifact.inspect", json!({"artifact_id": accepted}))["file"],
        "present"
    );
    // And the next request asks for consent again.
    assert_eq!(
        owner.task(asset, &profile).unwrap_err().code,
        "consent-required"
    );
    owner.stop();
}

/// A task carries the `request` envelope, so a client that retries it after a lost answer gets the
/// first job back and the endpoint is sent the photo once.
#[test]
fn a_retried_task_returns_the_first_job_and_starts_no_second() {
    let fixture = Fixture::new("proof-task-retry");
    let assets = fixture.import();
    let owner = fixture.start();
    let Ready { assets, profile } = ready(&fixture, &owner, assets);
    let generations = || {
        fixture
            .endpoint
            .requests()
            .iter()
            .filter(|request| request.path == "/generate")
            .count()
    };
    let request = |asset: &AssetId, request_id: &str| {
        json!({
            "asset_id": asset,
            "profile_id": profile,
            "mutation": {"request_id": request_id, "actor": "test"},
        })
    };
    let first = owner
        .task_with(request(&assets[0], "generate-once"))
        .unwrap();
    assert_eq!(first["deduplicated"], json!(false));
    let retried = owner.ok(TASK, request(&assets[0], "generate-once"));
    assert_eq!(retried["deduplicated"], json!(true));
    assert_eq!(retried["job_id"], first["job_id"], "the first job");
    assert_eq!(owner.finished(&first["job_id"])["status"], "ready");
    assert_eq!(generations(), 1, "the photo was sent once");
    let tasks = owner.events().iter().filter(|event| *event == TASK).count();
    assert_eq!(tasks, 1, "one task, announced once");
    // The same request_id with other input is a conflict, and starts nothing either.
    let other = owner.fail(TASK, request(&assets[1], "generate-once"));
    assert_eq!(
        (other.code.as_str(), other.message.as_str()),
        (
            "conflict",
            "request_id was already used with different input"
        )
    );
    // A task without a well-formed envelope is refused by name before anything is checked.
    let malformed = owner.fail(
        TASK,
        json!({"asset_id": assets[0], "profile_id": profile, "mutation": {"request_id": "x"}}),
    );
    assert_eq!(malformed.code, "validation");
    assert!(
        malformed.message.starts_with("mutation:"),
        "{}",
        malformed.message
    );
    assert_eq!(generations(), 1);
    owner.stop();
}

#[test]
fn a_wrong_api_key_fails_the_task_with_the_endpoints_refusal_and_commits_nothing() {
    let fixture = Fixture::new("proof-wrong-key");
    let assets = fixture.import();
    let owner = fixture.start();
    let Ready { assets, profile } = ready(&fixture, &owner, assets);
    owner.set_key(&profile, "not-the-key");
    let before = owner.state(&assets[0]);
    let queued = owner.task(&assets[0], &profile).unwrap();
    let job = owner.finished(&queued["job_id"]);
    assert_eq!(job["status"], "failed");
    assert_eq!(
        job["error"],
        json!({"code": "read-error", "message": "proof-echo answered 401"})
    );
    assert!(!fixture.endpoint.requests().last().unwrap().authorized);
    assert_eq!(owner.state(&assets[0]), before);
    let status = owner.ok("artifact.status", json!({}));
    assert_eq!(status["bytes"], 0, "no artifact was recorded: {status}");
    assert!(!owner.events().contains(&TASK.to_owned()));
    owner.stop();
}

#[test]
fn only_a_successful_task_records_what_it_published() {
    let fixture = Fixture::new("proof-publisher");
    let owner = fixture.start();
    let id = ArtifactId::for_hash(&format!("{:x}", Sha256::digest(PUBLISHED))).unwrap();
    let object = fixture
        .root
        .join("catalog.artifacts")
        .join("objects")
        .join(id.sha256());
    let queued = owner.ok("task.publish-then-answer", json!({}));
    let job = owner.finished(&queued["job_id"]);
    assert_eq!(job["status"], "failed");
    assert_eq!(job["error"]["message"], "failed after publishing");
    assert!(object.exists(), "the bytes were written");
    assert_eq!(
        owner
            .fail("artifact.inspect", json!({"artifact_id": id}))
            .message,
        format!("unknown artifact {id}"),
        "but never recorded"
    );
    let queued = owner.ok("task.publish-then-answer", json!({"fail": false}));
    let job = owner.finished(&queued["job_id"]);
    assert_eq!(job["status"], "ready");
    assert_eq!(
        job["result"],
        json!({"result": {"published": id}, "artifacts": [id]})
    );
    assert_eq!(
        owner.ok("artifact.inspect", json!({"artifact_id": id}))["file"],
        "present"
    );
    assert_eq!(
        owner
            .fail("task.publish-then-answer", json!({"fail": 1}))
            .message,
        "parameter fail must be a boolean"
    );
    owner.stop();
}

#[test]
fn changing_the_endpoint_revokes_its_grant_and_asks_for_consent_again() {
    let fixture = Fixture::new("proof-endpoint-change");
    let assets = fixture.import();
    let owner = fixture.start();
    let Ready { assets, profile } = ready(&fixture, &owner, assets);
    let elsewhere = "https://elsewhere.example";
    let moved = fixture
        .endpoint
        .generate_url()
        .replace(&fixture.endpoint.base_url(), elsewhere);
    let changed = owner.set(Some(&profile), json!({"endpoint": moved}));
    assert_eq!(changed["revoked"].as_array().unwrap().len(), 1);
    let refused = owner.task(&assets[0], &profile).unwrap_err();
    assert_eq!(refused.code, "consent-required");
    let consent = refused.data.unwrap()["consent"].clone();
    assert_eq!(consent["capability"], "echo");
    assert_eq!(consent["scope"]["origin"], json!(elsewhere));
    owner.stop();
}

#[test]
fn a_running_task_is_cancelled_at_its_checkpoint() {
    let fixture = Fixture::new("proof-cancel");
    let assets = fixture.import();
    let owner = fixture.start();
    let Ready { assets, profile } = ready(&fixture, &owner, assets);
    let generation = fixture.endpoint.generation();
    generation.shut();
    let sends = generation.reached();
    let queued = owner.task(&assets[0], &profile).unwrap();
    generation.wait_reached(sends + 1, "the task's send");
    let cancelled = owner.ok(JOB_CANCEL, json!({"job_id": queued["job_id"]}));
    assert_eq!(
        cancelled["status"], "running",
        "a running job stops at its checkpoint"
    );
    let job = owner.finished(&queued["job_id"]);
    assert!(
        generation.holding(),
        "the task stopped while its answer was still held"
    );
    assert_eq!(job["status"], "cancelled");
    assert_eq!(job["error"]["message"], "the job was cancelled");
    generation.open();
    assert_eq!(owner.ok("artifact.status", json!({}))["bytes"], 0);
    owner.stop();
}

#[test]
fn removing_the_resource_refuses_the_task_and_the_accepted_tint_still_renders() {
    let fixture = Fixture::new("proof-remove");
    let assets = fixture.import();
    let owner = fixture.start();
    let Ready { assets, profile } = ready(&fixture, &owner, assets);
    let asset = &assets[0];
    let untinted = owner.state(asset)["current_entry"]["id"].clone();
    let queued = owner.task(asset, &profile).unwrap();
    let job = owner.finished(&queued["job_id"]);
    owner.apply(asset, &job["result"]["artifacts"][0]);
    let pixel = owner.sample(asset, 10, 10)["rgba"].clone();
    let removed = owner.ok(
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "proof-palette"}),
    );
    assert_eq!(owner.finished(&removed["job_id"])["status"], "ready");
    let status = owner.ok(STATUS, json!({"module_id": MODULE}));
    assert_eq!(status["resources"][0]["state"], "not-installed");
    assert_eq!(owner.sample(asset, 10, 10)["rgba"], pixel);
    let refused = owner.task(asset, &profile).unwrap_err();
    assert_eq!(refused.code, "not-ready");
    assert_eq!(
        refused.data.unwrap()["requirements"],
        json!([{"kind": "resource", "id": "proof-palette", "state": "not-installed"}])
    );
    // Resetting returns the photo to neutral in place; a second reset changes nothing.
    let revision = owner.state(asset)["revision"].clone();
    let reset = owner.ok(
        "edit.reset-proof-tint",
        json!({"asset_id": asset, "mutation": {"expected_revision": revision, "request_id": "reset", "actor": "test"}}),
    );
    assert_eq!(reset["outcome"], "applied");
    let revision = owner.state(asset)["revision"].clone();
    let again = owner.ok(
        "edit.reset-proof-tint",
        json!({"asset_id": asset, "mutation": {"expected_revision": revision, "request_id": "reset-again", "actor": "test"}}),
    );
    assert_eq!(again["outcome"], "no-op");
    owner.stop();
    let untinted = EntryId::parse(untinted.as_str().unwrap()).unwrap();
    assert_eq!(
        assert_tint_renders(&fixture, asset, &untinted, [1.0; 3]),
        0,
        "a neutral tint is the source"
    );
}

/// The proof module's apply and reset plan against a stack exactly as the contract says: commit,
/// update in place, no-op on the same artifact, and a reset that neutralises in place.
#[test]
fn apply_commits_then_updates_in_place_and_reset_neutralises_the_same_layer() {
    let module = CapabilitiesProofModule::new("http://127.0.0.1:9");
    let first = ArtifactId::for_hash(&"a".repeat(64)).unwrap();
    let second = ArtifactId::for_hash(&"b".repeat(64)).unwrap();
    let plan = |action: &str, artifact: Option<&ArtifactId>, layers: &[Layer]| {
        let mut parameters = Map::new();
        if let Some(artifact) = artifact {
            parameters.insert("artifact".into(), json!(artifact.as_str()));
        }
        let input = module.parse(action, &parameters).unwrap();
        let stage = crate::modules::FixedStage::new(Stage {
            width: 4,
            height: 4,
        });
        let registry = crate::ModuleRegistry::builtin();
        module
            .plan(&input, &stage.context(layers, &registry))
            .unwrap()
    };
    let ActionPlan::Commit(new) = plan("apply-proof-tint", Some(&first), &[]) else {
        panic!("the first apply commits");
    };
    assert_eq!(new.payload, json!({"artifact": first.as_str()}));
    assert_eq!(new.artifacts, std::slice::from_ref(&first));
    // The layer the host would store for that commit, and for each update after it.
    let layer = Layer {
        artifacts: new.artifacts,
        ..Layer::new(new.effect_id, new.payload)
    };
    let stored = |layer: &Layer, update: LayerUpdate| Layer {
        payload: update.payload,
        artifacts: update.artifacts,
        ..layer.clone()
    };
    assert_eq!(
        plan(
            "apply-proof-tint",
            Some(&first),
            std::slice::from_ref(&layer)
        ),
        ActionPlan::NoOp
    );
    let ActionPlan::Update(updated) = plan(
        "apply-proof-tint",
        Some(&second),
        std::slice::from_ref(&layer),
    ) else {
        panic!("a second apply updates in place");
    };
    assert_eq!(updated.id, layer.id);
    assert_eq!(updated.artifacts, [second]);
    let updated = stored(&layer, updated);
    let ActionPlan::Update(neutral) =
        plan("reset-proof-tint", None, std::slice::from_ref(&updated))
    else {
        panic!("a reset updates in place");
    };
    assert_eq!(
        (neutral.payload.clone(), neutral.artifacts.len()),
        (json!({}), 0)
    );
    let neutral = stored(&updated, neutral);
    assert_eq!(
        plan("reset-proof-tint", None, std::slice::from_ref(&neutral)),
        ActionPlan::NoOp
    );
    assert_eq!(plan("reset-proof-tint", None, &[]), ActionPlan::NoOp);
}

/// The framework's own costs on this host: registration with and without the proof module, the
/// owner's answer to the capability reads, a whole task against the endpoint answered in process,
/// cancellation of a task stalled in its request, and the disk an installed resource takes.
/// Requests go through the in-memory transport, which checks a cancel every millisecond; the real
/// transport's own costs are measured in `luxforge-net`. A job's end is seen
/// by a wait that looks every millisecond, so the job measurements are accurate to about a
/// millisecond. Run explicitly, in release, on a quiet machine:
/// `cargo test --release --locked -p luxforge-core --lib capability_timing -- --ignored --nocapture`.
#[test]
#[ignore = "measurement, run explicitly in release"]
fn capability_timing() {
    const SAMPLES: usize = 30;
    let report = |name: &str, samples: &mut Vec<f64>| {
        let ms = luxforge_testbase::Distribution::of(samples.iter().copied())
            .expect("the measurement took samples");
        println!(
            "{name}: p50 {:.3} ms, p95 {:.3} ms over {} samples",
            ms.p50, ms.p95, ms.count
        );
    };

    let mut builtin = Vec::new();
    let mut with_proof = Vec::new();
    for _ in 0..200 {
        let started = Instant::now();
        let registry = ModuleRegistry::builtin();
        builtin.push(started.elapsed().as_secs_f64() * 1000.0);
        drop(registry);
        let started = Instant::now();
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(CapabilitiesProofModule::new(
                "http://127.0.0.1:9/",
            )))
            .unwrap();
        with_proof.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    report("registration, built-ins", &mut builtin);
    report(
        "registration, built-ins and the proof module",
        &mut with_proof,
    );

    let fixture = Fixture::new("proof-timing");
    let assets = fixture.import();
    let owner = fixture.start();
    let mut status = Vec::new();
    let mut settings = Vec::new();
    for _ in 0..SAMPLES {
        let started = Instant::now();
        owner.ok(STATUS, json!({"module_id": MODULE}));
        status.push(started.elapsed().as_secs_f64() * 1000.0);
        let started = Instant::now();
        owner.ok(READ, json!({"module_id": MODULE}));
        settings.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    report("module.status round trip (nothing installed)", &mut status);
    report("module.settings.read round trip", &mut settings);

    let Ready { assets, profile } = ready(&fixture, &owner, assets);
    let installed = fixture
        .files()
        .into_iter()
        .filter(|(path, _)| {
            path.components()
                .any(|part| part.as_os_str() == "resources")
        })
        .map(|(_, bytes)| bytes.len())
        .sum::<usize>();
    let staging = fixture.root.join("data/modules/resources/.staging");
    println!(
        "installed resource disk: {installed} bytes including installed.json; staging entries left: {}",
        fs::read_dir(&staging).map_or(0, |entries| entries.count())
    );

    // A whole task and a cancelled one: the owner binds the stack, the module lane reads the
    // palette, samples the 8 × 8 grid, posts it through the in-memory transport and publishes the
    // artifact, and the owner records it.
    let mut task = Vec::new();
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let queued = owner.task(&assets[0], &profile).unwrap();
        let job = owner.finished(&queued["job_id"]);
        task.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(job["status"], "ready", "{job}");
    }
    report("task request to succeeded (in-memory transport)", &mut task);
    // The task's durable part: one artifact synced and renamed into place, as the writer does it.
    let writer = EditorService::open_with(&fixture.root.join("timing.sqlite"), fixture.registry())
        .unwrap()
        .artifact_writer()
        .unwrap();
    let mut publish = Vec::new();
    for index in 0..SAMPLES as u32 {
        let bytes: Vec<u8> = (index..index + 3)
            .flat_map(|value| (value as f32).to_le_bytes())
            .collect();
        let started = Instant::now();
        writer
            .write(
                &bytes,
                crate::artifacts::ArtifactMeta {
                    kind: "timing".into(),
                    width: None,
                    height: None,
                    colour: None,
                },
                MODULE,
            )
            .unwrap();
        publish.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    report(
        "publish one 12-byte artifact (synced, renamed)",
        &mut publish,
    );
    // The endpoint holds every answer, so each task stalls in its request.
    let generation = fixture.endpoint.generation();
    generation.shut();
    let mut cancel_task = Vec::new();
    for _ in 0..10 {
        let sends = generation.reached();
        let queued = owner.task(&assets[0], &profile).unwrap();
        generation.wait_reached(sends + 1, "the task's send");
        let started = Instant::now();
        owner.ok(JOB_CANCEL, json!({"job_id": queued["job_id"]}));
        let job = owner.finished(&queued["job_id"]);
        cancel_task.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(job["status"], "cancelled", "{job}");
    }
    report(
        "cancel a task stalled in its request to cancelled",
        &mut cancel_task,
    );
    generation.open();
}
