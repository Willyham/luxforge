//! The `local-runtime` capability driven through the catalog owner exactly as a client drives it,
//! against the deterministic fake runtime: nothing loads on a listing, a status or a launch; a load
//! and a task's first use open a model once and keep it between tasks; an unload, a resource's
//! removal, the idle interval and the memory signal release it; the resident budget evicts idle
//! models and refuses one that cannot fit; and every refusal names what is missing.
use super::{
    descriptor::{
        CapabilityDescriptor, CapabilityKind, ModelKind, ResourceDescriptor, TaskDescriptor,
    },
    grants::GRANT,
    host::{HostConfig, TASK_PREFIX},
    resources::{INSTALL, REMOVE},
    runtime::{LOAD, UNLOAD},
    secrets::MemorySecretStore,
    testing::{MemoryTransport, Owner, sha256_hex, temp},
};
use crate::{
    ActionInput, ActionPlan, CapabilityModule, Error, ModuleDescriptor, ModuleRegistry,
    ParameterDescriptor, Processing, StageContext, ToolModule,
    capabilities::context::ModuleContext,
    inference::{
        Conditioning, GenerateRequest, Precision, Tensor, TensorData,
        fake::{FAKE_INPUT, FakeControls, FakeInference},
    },
};
use luxforge_test_macros::slow;
use luxforge_testbase::{Gate, wait_for, wait_until};
use serde_json::{Map, Value, json};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};

const MODULE: &str = "test.runtime";
const FILL: &str = "fill-model";
const EMBED: &str = "embed-model";
const FILL_TASK: &str = "run-test-fill";
const EMBED_TASK: &str = "run-test-embed";
const FILL_BYTES: &[u8] = b"fake fill weights";
const EMBED_BYTES: &[u8] = b"fake embedding weights";
/// Each model's declared resident size; two fit the tests' small budget only one at a time.
const RESIDENT: u64 = 600;
const SMALL_BUDGET: u64 = 1000;
const SERVED: &str = "https://models.example";

fn resource(id: &str, bytes: &[u8]) -> ResourceDescriptor {
    ResourceDescriptor {
        id: id.into(),
        title: id.into(),
        version: "1".into(),
        url: format!("{SERVED}/{id}"),
        bytes: bytes.len() as u64,
        sha256: sha256_hex(bytes),
        format: "fake-model".into(),
        license: "CC0-1.0".into(),
        provenance: "Generated for tests".into(),
        ..ResourceDescriptor::default()
    }
}

fn capability(id: &str, kind: CapabilityKind) -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: id.into(),
        kind,
        purpose: format!("{id} for tests"),
    }
}

fn local(resource: &str, model: ModelKind) -> CapabilityKind {
    CapabilityKind::LocalRuntime {
        resource: resource.into(),
        model,
        resident_bytes: RESIDENT,
        backends: vec!["coreml".into(), "cpu".into()],
        precision: Precision::Fp32,
        with: Vec::new(),
    }
}

fn task(id: &str, uses: &str) -> TaskDescriptor {
    TaskDescriptor {
        id: id.into(),
        title: id.into(),
        notes: "test".into(),
        asset: false,
        profile: false,
        uses: vec![uses.into()],
        parameters: vec![
            ParameterDescriptor::integer("seed", 0, 1000)
                .default(1)
                .notes("test"),
            ParameterDescriptor::enumeration("hold", ["none", "before", "while"])
                .default("none")
                .notes("where the task waits at the module's gate"),
        ],
        apply: None,
        reads: None,
        flags: Vec::new(),
    }
}

/// Two models, each installed by its own download capability and held by its own local-runtime
/// capability, and a task that uses each.
fn descriptor() -> ModuleDescriptor {
    ModuleDescriptor {
        id: MODULE.into(),
        title: "Runtime test".into(),
        capabilities: vec![
            capability(
                "fill-download",
                CapabilityKind::DownloadArtifact {
                    resource: FILL.into(),
                },
            ),
            capability(
                "embed-download",
                CapabilityKind::DownloadArtifact {
                    resource: EMBED.into(),
                },
            ),
            capability("fill", local(FILL, ModelKind::Generator)),
            capability("embed", local(EMBED, ModelKind::Session)),
        ],
        resources: vec![resource(FILL, FILL_BYTES), resource(EMBED, EMBED_BYTES)],
        tasks: vec![task(FILL_TASK, "fill"), task(EMBED_TASK, "embed")],
        ..ModuleDescriptor::default()
    }
}

/// A module whose tasks run its two models, waiting at its gate where a test asks.
struct RuntimeModule {
    descriptor: ModuleDescriptor,
    gate: Arc<Gate>,
}

impl ToolModule for RuntimeModule {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
        Err(Error::validation(format!("unknown action {action_id}")))
    }
    fn plan(&self, input: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Err(Error::validation(format!(
            "unknown action {}",
            input.action_id
        )))
    }
    fn validate_payload(&self, effect_id: &str, _: u32, _: &Value) -> Result<(), Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn describe(&self, effect_id: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn compile(
        &self,
        effect_id: &str,
        _: u32,
        _: &Value,
        _: crate::CompileStage,
    ) -> Result<Processing, Error> {
        Err(Error::unavailable_effect(effect_id, &[]))
    }
    fn capabilities(&self) -> Option<&dyn CapabilityModule> {
        Some(self)
    }
}

impl CapabilityModule for RuntimeModule {
    fn run_task(
        &self,
        task_id: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        let hold = parameters.get("hold").and_then(Value::as_str);
        let seed = parameters.get("seed").and_then(Value::as_u64).unwrap_or(1);
        let wait = |at: &str| {
            if hold == Some(at) {
                self.gate.pass_unless(|| context.is_cancelled());
            }
            context.checkpoint()
        };
        wait("before")?;
        match task_id {
            FILL_TASK => {
                let generator = context.generator("fill")?;
                wait("while")?;
                let (width, height) = (16, 16);
                let image = vec![0.5; 3 * width as usize * height as usize];
                let mask: Vec<f32> = (0..width * height)
                    .map(|pixel| if pixel % width < 8 { 1.0 } else { 0.0 })
                    .collect();
                let request = GenerateRequest {
                    width,
                    height,
                    image: &image,
                    mask: &mask,
                    conditioning: Conditioning::None,
                    seed,
                    steps: 2,
                };
                let generated = generator.generate(
                    &request,
                    &mut |step| context.progress(Some(step.fraction()), "generating"),
                    context.cancel(),
                )?;
                Ok(json!({
                    "provenance": generated.provenance.fields(),
                    "image": generated.image,
                }))
            }
            EMBED_TASK => {
                let session = context.session("embed")?;
                wait("while")?;
                let input = Tensor::new(
                    FAKE_INPUT,
                    vec![1, 1, 1, 2],
                    TensorData::F32(vec![0.25, seed as f32]),
                )?;
                let output = session.run(&[input], context.cancel())?;
                Ok(json!({"output": output[0].f32().unwrap_or_default()}))
            }
            other => Err(Error::validation(format!("unknown task {other}"))),
        }
    }
}

/// A catalog and module directories under one temporary root, the module's gate and the fake.
struct Fixture {
    root: PathBuf,
    secrets: Arc<MemorySecretStore>,
    gate: Arc<Gate>,
    fake: Arc<FakeInference>,
    configured: bool,
    idle: Duration,
    budget: u64,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = temp(&format!("runtime-{name}"));
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            secrets: Arc::new(MemorySecretStore::new()),
            gate: Arc::new(Gate::new()),
            fake: Arc::new(FakeInference::new()),
            configured: true,
            idle: super::runtime::RUNTIME_IDLE_UNLOAD,
            budget: super::runtime::DEFAULT_RUNTIME_BUDGET_BYTES,
        }
    }

    fn controls(&self) -> Arc<FakeControls> {
        self.fake.controls()
    }

    fn registry(&self) -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(RuntimeModule {
                descriptor: descriptor(),
                gate: self.gate.clone(),
            }))
            .unwrap();
        Arc::new(registry)
    }

    fn host(&self) -> HostConfig {
        let transport = MemoryTransport::new(|exchange, answer| match exchange.path() {
            "/fill-model" => answer.whole(200, FILL_BYTES),
            "/embed-model" => answer.whole(200, EMBED_BYTES),
            _ => answer.whole(404, b""),
        });
        HostConfig {
            config_dir: Some(self.root.join("config")),
            resource_dir: Some(self.root.join("resources")),
            secrets: self.secrets.clone(),
            transport,
            inference: self
                .configured
                .then(|| self.fake.clone() as Arc<dyn crate::inference::InferenceRuntime>),
            runtime_idle: self.idle,
            runtime_budget_bytes: self.budget,
            ..HostConfig::unconfigured()
        }
    }

    fn start(&self) -> Owner {
        Owner::start(
            &self.root.join("catalog.sqlite"),
            self.registry(),
            self.host(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.open();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// What these tests ask of the owner besides the shared harness.
trait Runtime {
    fn install(&self, resource: &str);
    /// `module.runtime.load`, waited for when it queued a job.
    fn load(&self, resource: &str) -> Value;
    /// A task, waited for; its finished record.
    fn run(&self, task: &str, params: Value) -> Value;
    /// The module's resident and loading models.
    fn models(&self) -> Vec<Value>;
    /// The resource of every resident or loading model, in order.
    fn resident(&self) -> Vec<String>;
}

impl Runtime for Owner {
    fn install(&self, resource: &str) {
        self.ok_as(
            self.admin,
            GRANT,
            json!({
                "module_id": MODULE,
                "capability": if resource == FILL { "fill-download" } else { "embed-download" },
                "scope": {"resource": resource, "version": "1", "origin": SERVED},
            }),
        );
        let queued = self.ok(
            INSTALL,
            json!({"module_id": MODULE, "resource_id": resource}),
        );
        let job = self.finished(&queued["job_id"]);
        assert_eq!(job["status"], "ready", "{job}");
    }

    fn load(&self, resource: &str) -> Value {
        let answer = self.ok(LOAD, json!({"module_id": MODULE, "resource_id": resource}));
        match answer.get("job_id") {
            Some(job) => self.finished(job),
            None => answer,
        }
    }

    fn run(&self, task: &str, params: Value) -> Value {
        let queued = self.ok(&format!("{TASK_PREFIX}{task}"), params);
        self.finished(&queued["job_id"])
    }

    fn models(&self) -> Vec<Value> {
        self.status(MODULE)["runtime"]["models"]
            .as_array()
            .expect("a module with a local-runtime capability reports its models")
            .clone()
    }

    fn resident(&self) -> Vec<String> {
        self.models()
            .iter()
            .map(|model| model["resource_id"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// The method of every recorded event, in order.
fn events(owner: &Owner) -> Vec<String> {
    owner.ok("events.since", json!({"after": 0}))["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["method"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn nothing_loads_until_asked_and_a_loaded_model_is_kept_between_tasks() {
    let fixture = Fixture::new("journey");
    let controls = fixture.controls();
    let owner = fixture.start();
    // Listing, discovery and status open nothing, before or after the resource is installed.
    owner.ok("module.list", json!({}));
    let methods = owner.ok("schema.list", json!({}))["methods"].clone();
    for method in [LOAD, UNLOAD] {
        assert!(methods.get(method).is_some(), "schema.list lists {method}");
    }
    let status = owner.status(MODULE);
    assert_eq!(
        status["runtime"],
        json!({"held_bytes": 0, "budget_bytes": fixture.budget, "available": true, "models": []})
    );
    owner.install(FILL);
    owner.status(MODULE);
    assert_eq!(controls.opens(), 0, "nothing opened before a load");

    // A load queues a module-lane job that opens the model once.
    let queued = owner.ok(LOAD, json!({"module_id": MODULE, "resource_id": FILL}));
    assert_eq!(queued["state"], "loading");
    let job = owner.finished(&queued["job_id"]);
    assert_eq!(job["status"], "ready", "{job}");
    assert_eq!(
        job["result"],
        json!({"module_id": MODULE, "resource_id": FILL, "resident_bytes": RESIDENT})
    );
    assert_eq!(controls.opens(), 1);
    let models = owner.models();
    assert_eq!(models.len(), 1);
    assert_eq!(
        (
            models[0]["resource_id"].clone(),
            models[0]["capability"].clone(),
            models[0]["model"].clone(),
            models[0]["state"].clone(),
            models[0]["resident_bytes"].clone(),
            models[0]["in_use"].clone(),
            models[0]["placement"]["backend"].clone(),
            models[0]["placement"]["precision"].clone(),
        ),
        (
            json!(FILL),
            json!("fill"),
            json!("generator"),
            json!("loaded"),
            json!(RESIDENT),
            json!(false),
            json!("cpu"),
            json!("fp32"),
        ),
        "the first backend the runtime offers is chosen"
    );
    assert_eq!(owner.status(MODULE)["runtime"]["held_bytes"], RESIDENT);
    // A second load of a resident model answers at once.
    let again = owner.ok(LOAD, json!({"module_id": MODULE, "resource_id": FILL}));
    assert_eq!(again["state"], "loaded");
    assert!(again.get("job_id").is_none());

    // Tasks use the resident model: no second open, and the same seed gives the same fill.
    let first = owner.run(FILL_TASK, json!({"seed": 3}));
    let second = owner.run(FILL_TASK, json!({"seed": 3}));
    let other = owner.run(FILL_TASK, json!({"seed": 4}));
    assert_eq!(first["status"], "ready", "{first}");
    assert_eq!(controls.opens(), 1, "tasks reuse the loaded model");
    assert_eq!(controls.generations(), 3);
    assert_eq!(first["result"]["result"], second["result"]["result"]);
    assert_ne!(
        first["result"]["result"]["image"],
        other["result"]["result"]["image"]
    );
    let provenance = &first["result"]["result"]["provenance"];
    assert_eq!(
        (
            provenance["model"].clone(),
            provenance["model_sha256"].clone(),
            provenance["runtime"].clone(),
            provenance["backend"].clone(),
            provenance["seed"].clone(),
            provenance["steps"].clone(),
        ),
        (
            json!(FILL),
            json!(sha256_hex(FILL_BYTES)),
            json!("luxforge-fake"),
            json!("cpu"),
            json!(3),
            json!(2),
        )
    );

    // A relaunch loads nothing until asked again.
    owner.stop();
    assert_eq!(controls.live(), 0, "a stopped owner holds no model");
    let owner = fixture.start();
    owner.ok("module.list", json!({}));
    assert!(owner.models().is_empty());
    assert_eq!(controls.opens(), 1, "a launch opens nothing");
}

#[test]
fn a_task_loads_on_first_use_and_an_unload_releases_and_is_announced() {
    let fixture = Fixture::new("unload");
    let controls = fixture.controls();
    let owner = fixture.start();
    owner.install(EMBED);
    let ran = owner.run(EMBED_TASK, json!({"seed": 2}));
    assert_eq!(ran["status"], "ready", "{ran}");
    assert_eq!(
        ran["result"]["result"]["output"].as_array().unwrap().len(),
        2
    );
    assert_eq!(controls.opens(), 1, "the task's first use opened the model");
    assert_eq!(owner.resident(), [EMBED], "and the slot keeps it");
    owner.run(EMBED_TASK, json!({"seed": 2}));
    assert_eq!(controls.opens(), 1);

    let before = events(&owner).len();
    let unloaded = owner.ok(UNLOAD, json!({"module_id": MODULE, "resource_id": EMBED}));
    assert_eq!(
        unloaded,
        json!({"module_id": MODULE, "unloaded": [EMBED], "cancelled_jobs": []})
    );
    assert!(owner.models().is_empty());
    assert_eq!(controls.live(), 0, "the model was dropped");
    assert_eq!(
        &events(&owner)[before..],
        [UNLOAD],
        "one event for the unload"
    );
    // Nothing more to release answers empty and records nothing; every model of the module is the
    // default.
    let none = owner.ok(UNLOAD, json!({"module_id": MODULE}));
    assert_eq!(none["unloaded"], json!([]));
    assert_eq!(events(&owner).len(), before + 1);
    let undeclared = owner.fail(
        UNLOAD,
        json!({"module_id": MODULE, "resource_id": "missing"}),
    );
    assert_eq!(undeclared.code, "validation");
    assert_eq!(
        undeclared.message,
        "module test.runtime declares no local-runtime capability for resource missing"
    );
}

#[test]
#[slow]
fn an_idle_model_is_released_after_the_interval_and_every_model_on_a_memory_signal() {
    let mut fixture = Fixture::new("idle");
    fixture.idle = Duration::from_millis(150);
    let controls = fixture.controls();
    let owner = fixture.start();
    owner.install(FILL);
    owner.install(EMBED);
    owner.load(FILL);
    assert_eq!(owner.resident(), [FILL]);
    wait_for("the idle model to be released", || {
        Some(()).filter(|_| owner.models().is_empty())
    });
    assert_eq!(controls.live(), 0);

    // A model a task holds is not idle, however long the task runs.
    fixture.gate.shut();
    let queued = owner.ok(
        &format!("{TASK_PREFIX}{FILL_TASK}"),
        json!({"hold": "while"}),
    );
    wait_for("the task to hold its model", || {
        Some(()).filter(|_| fixture.gate.holding())
    });
    let held = std::time::Instant::now();
    wait_until("three idle intervals to pass", || {
        held.elapsed() > fixture.idle * 3
    });
    assert_eq!(owner.resident(), [FILL], "a model in use stays");
    assert_eq!(owner.models()[0]["in_use"], true);
    fixture.gate.open();
    assert_eq!(owner.finished(&queued["job_id"])["status"], "ready");
    // Once the task has ended the model goes unused, and is released after the interval.
    wait_for("the model the task used to be released", || {
        Some(()).filter(|_| owner.models().is_empty())
    });
    assert_eq!(controls.live(), 0);
    drop(owner);

    // The memory signal releases every model at once, from any thread.
    let fixture = Fixture::new("memory");
    let controls = fixture.controls();
    let owner = fixture.start();
    owner.install(FILL);
    owner.install(EMBED);
    owner.load(FILL);
    owner.load(EMBED);
    let mut resident = owner.resident();
    resident.sort();
    assert_eq!(resident, [EMBED, FILL]);
    assert_eq!(owner.handle.memory_signal(), 2);
    assert!(owner.models().is_empty());
    assert_eq!(controls.live(), 0);
    assert_eq!(owner.handle.memory_signal(), 0);
}

#[test]
fn the_resident_budget_evicts_an_idle_model_and_refuses_one_that_cannot_fit() {
    let mut fixture = Fixture::new("budget");
    fixture.budget = SMALL_BUDGET;
    let controls = fixture.controls();
    let owner = fixture.start();
    owner.install(FILL);
    owner.install(EMBED);
    owner.load(FILL);
    // The second model does not fit beside the first, which is idle, so the first is released.
    let job = owner.load(EMBED);
    assert_eq!(job["status"], "ready", "{job}");
    assert_eq!(owner.resident(), [EMBED]);
    assert_eq!(controls.live(), 1);

    // While a task uses one, the other is refused before anything is queued.
    fixture.gate.shut();
    let queued = owner.ok(
        &format!("{TASK_PREFIX}{EMBED_TASK}"),
        json!({"hold": "while"}),
    );
    wait_for("the task to hold its model", || {
        Some(()).filter(|_| fixture.gate.holding())
    });
    let refused = owner.fail(LOAD, json!({"module_id": MODULE, "resource_id": FILL}));
    assert_eq!(refused.code, "resource-limit", "{refused:?}");
    assert!(
        refused.message.contains("1000 bytes"),
        "{}",
        refused.message
    );
    fixture.gate.open();
    assert_eq!(owner.finished(&queued["job_id"])["status"], "ready");
    assert_eq!(owner.load(FILL)["status"], "ready");
    assert_eq!(owner.resident(), [FILL]);
}

#[test]
fn an_unload_or_a_removal_cancels_a_queued_load_and_releases_the_model() {
    let fixture = Fixture::new("cancel");
    let controls = fixture.controls();
    let owner = fixture.start();
    owner.install(FILL);
    owner.install(EMBED);
    // A task holds the module lane, so the load waits behind it.
    fixture.gate.shut();
    let held = owner.ok(
        &format!("{TASK_PREFIX}{FILL_TASK}"),
        json!({"hold": "before"}),
    );
    wait_for("the task to hold the lane", || {
        Some(()).filter(|_| fixture.gate.holding())
    });
    let queued = owner.ok(LOAD, json!({"module_id": MODULE, "resource_id": EMBED}));
    assert_eq!(queued["status"], "queued");
    // A second request joins it.
    let joined = owner.ok(LOAD, json!({"module_id": MODULE, "resource_id": EMBED}));
    assert_eq!(joined["job_id"], queued["job_id"]);
    assert_eq!(
        owner.models(),
        Vec::<Value>::new(),
        "a queued load holds no slot"
    );
    let unloaded = owner.ok(UNLOAD, json!({"module_id": MODULE, "resource_id": EMBED}));
    assert_eq!(unloaded["cancelled_jobs"], json!([queued["job_id"]]));
    let job = owner.finished(&queued["job_id"]);
    assert_eq!(job["status"], "cancelled", "{job}");
    fixture.gate.open();
    assert_eq!(owner.finished(&held["job_id"])["status"], "ready");
    assert_eq!(controls.opens(), 1, "only the task's model was opened");

    // Removing an installed resource releases its loaded model first.
    assert_eq!(owner.resident(), [FILL]);
    let removal = owner.ok(REMOVE, json!({"module_id": MODULE, "resource_id": FILL}));
    owner.finished(&removal["job_id"]);
    assert!(owner.models().is_empty());
    assert_eq!(controls.live(), 0);
    let gone = owner.fail(LOAD, json!({"module_id": MODULE, "resource_id": FILL}));
    assert_eq!(gone.code, "not-ready");
}

#[test]
fn refusals_name_the_missing_runtime_resource_or_capability_and_load_nothing() {
    let mut fixture = Fixture::new("refusals");
    fixture.configured = false;
    let owner = fixture.start();
    owner.install(FILL);
    assert_eq!(owner.status(MODULE)["runtime"]["available"], false);
    let unconfigured = owner.fail(LOAD, json!({"module_id": MODULE, "resource_id": FILL}));
    assert_eq!(
        (unconfigured.code.as_str(), unconfigured.message.as_str()),
        ("not-ready", "no local inference runtime is configured")
    );
    let task = owner.fail(&format!("{TASK_PREFIX}{FILL_TASK}"), json!({}));
    assert_eq!(task.code, "not-ready");
    assert_eq!(
        task.data.as_ref().unwrap()["requirements"],
        json!([{"kind": "runtime", "id": "fill", "state": "unavailable"}])
    );
    drop(owner);

    let fixture = Fixture::new("not-installed");
    let controls = fixture.controls();
    let owner = fixture.start();
    let missing = owner.fail(LOAD, json!({"module_id": MODULE, "resource_id": FILL}));
    assert_eq!(missing.code, "not-ready");
    assert_eq!(
        missing.data.as_ref().unwrap()["requirements"],
        json!([{"kind": "resource", "id": FILL, "state": "not-installed"}])
    );
    let task = owner.fail(&format!("{TASK_PREFIX}{FILL_TASK}"), json!({}));
    assert_eq!(task.code, "not-ready");
    let undeclared = owner.fail(LOAD, json!({"module_id": MODULE, "resource_id": "missing"}));
    assert_eq!(undeclared.code, "validation");
    let unknown = owner.fail(
        LOAD,
        json!({"module_id": "test.missing", "resource_id": FILL}),
    );
    assert_eq!(unknown.code, "validation", "{unknown:?}");

    // A failed open fails the load's job and keeps nothing.
    owner.install(FILL);
    controls.fail_opens(true);
    let failed = owner.load(FILL);
    assert_eq!(failed["status"], "failed", "{failed}");
    assert!(owner.models().is_empty());
    controls.fail_opens(false);
    assert_eq!(owner.load(FILL)["status"], "ready");
    assert_eq!(controls.opens(), 1);
}
