//! The Generative tier through the catalog owner exactly as a client drives it: the flag, the
//! developer models installed through the ordinary grant and install job, Generate and Regenerate,
//! the candidates' listing and chunks, an accept from another client, undo, redo and a reopen that
//! renders the accepted patch with no model; cancellation, the lock a running fill holds on its
//! photograph and nothing else, and every refusal by name. The fill pipeline's own exact tests are
//! in `fill/tests.rs`.
use super::{
    AiModule, DeveloperModels,
    models::{DEVELOPER_ORIGIN, FAKE_GENERATOR, FAKE_TEXT_ENCODER, ModelFile},
    module::{AI_MODULE, FILL_GENERATIVE},
};
use crate::{
    AssetId, EditorService, ModuleRegistry,
    api::ApiFailure,
    artifacts::PREPARED_ARTIFACT_BYTES,
    capabilities::{
        grants::GRANT,
        host::HostConfig,
        resources::INSTALL,
        secrets::MemorySecretStore,
        testing::{MemoryTransport, Owner, temp},
    },
    editor::mutation_json,
    inference::{
        CompositeInference,
        fake::{FAKE_BACKEND, FAKE_HOST_BACKEND, FakeControls, FakeInference},
    },
    modules::{CapabilityModule, Stage, TaskAdmission},
};
use luxforge_testbase::{Gate, wait_for};
use serde_json::{Map, Value, json};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};

const TASK: &str = "task.fill-generative";
const ACCEPT: &str = "edit.accept-correction-patch";
const READ_CANDIDATE: &str = "job.read-candidate";

/// A catalog with two imported photographs (480 x 320 each), module and preference directories
/// under one temporary root, the fake runtime and the gate the module passes after each candidate.
struct Fixture {
    root: PathBuf,
    assets: [AssetId; 2],
    gate: Arc<Gate>,
    fake: Arc<FakeInference>,
    secrets: Arc<MemorySecretStore>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = temp(&format!("ai-{name}"));
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let gate = Arc::new(Gate::new());
        let mut service =
            EditorService::open_with(&root.join("catalog.sqlite"), registry(&gate)).unwrap();
        let assets = ["s0/orientation-1.jpg", "s0/jpeg-scan-444-interleaved.jpg"].map(|name| {
            service
                .import(&luxforge_testbase::paths::fixture(name))
                .unwrap()
                .asset
                .id
        });
        drop(service);
        Self {
            root,
            assets,
            gate,
            fake: Arc::new(FakeInference::new()),
            secrets: Arc::new(MemorySecretStore::new()),
        }
    }

    fn controls(&self) -> Arc<FakeControls> {
        self.fake.controls()
    }

    fn host(&self, inference: bool) -> HostConfig {
        HostConfig {
            preferences_dir: Some(self.root.join("config")),
            config_dir: Some(self.root.join("config").join("modules")),
            resource_dir: Some(self.root.join("resources")),
            secrets: self.secrets.clone(),
            // Nothing past the developer models answers.
            transport: DeveloperModels::wrap(MemoryTransport::new(|_, answer| {
                answer.whole(404, b"")
            })),
            // The fake under the name a developer host serves it by.
            inference: inference.then(|| {
                CompositeInference::new()
                    .with_alias(FAKE_HOST_BACKEND, self.fake.clone(), FAKE_BACKEND)
                    .unwrap()
                    .shared()
            }),
            ..HostConfig::unconfigured()
        }
    }

    fn start(&self) -> Owner {
        Owner::start(
            &self.root.join("catalog.sqlite"),
            registry(&self.gate),
            self.host(true),
        )
    }

    /// The owner of a host with no `luxforge.ai` and no runtime: the built-in modules without it.
    fn start_without_ai(&self) -> Owner {
        Owner::start(
            &self.root.join("catalog.sqlite"),
            Arc::new(without_ai()),
            self.host(false),
        )
    }

    /// The flag on and the generator installed: the tier offered.
    fn ready(&self) -> Owner {
        let owner = self.start();
        owner.flag(true);
        owner.install(&FAKE_GENERATOR);
        owner
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.open();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The product modules without `luxforge.ai`.
fn without_ai() -> ModuleRegistry {
    let mut registry = ModuleRegistry::new();
    for module in crate::modules::registry::linked_modules(false) {
        if module.descriptor().id != AI_MODULE {
            registry.register(module).unwrap();
        }
    }
    registry
}

/// The product modules with the developer `luxforge.ai`, whose jobs pass `gate`.
fn registry(gate: &Arc<Gate>) -> Arc<ModuleRegistry> {
    let mut registry = without_ai();
    registry
        .register(Arc::new(AiModule::gated(gate.clone())))
        .unwrap();
    Arc::new(registry)
}

/// A horizontal stroke across the middle of the photograph: about 96 x 52 pixels of 480 x 320.
fn selection(asset: &AssetId) -> Value {
    json!({
        "asset_id": asset,
        "selection-points": [[0.4, 0.5], [0.6, 0.5]],
        "selection-radius": 0.08,
        "model": FAKE_GENERATOR.resource,
    })
}

fn with(mut params: Value, extra: Value) -> Value {
    for (key, value) in extra.as_object().unwrap() {
        params[key] = value.clone();
    }
    params
}

/// What these tests ask of the owner besides the shared harness.
trait Ai {
    fn flag(&self, on: bool);
    fn install(&self, file: &ModelFile);
    /// The fill, waiting through any preparation its read asks for, as a client does.
    fn fill(&self, params: Value) -> Result<Value, ApiFailure>;
    /// [`Self::fill`] that must be queued, finished: its job's record.
    fn generate(&self, params: Value) -> Value;
    fn asset_revision(&self, asset: &AssetId) -> u64;
    fn sample(&self, asset: &AssetId, x: u32, y: u32) -> Value;
    fn accept_as(
        &self,
        client: crate::api::ClientId,
        asset: &AssetId,
        job: &Value,
        candidate: u64,
    ) -> Result<Value, ApiFailure>;
    fn states(&self, job: &Value) -> Vec<String>;
    fn history_labels(&self, asset: &AssetId) -> Vec<String>;
}

impl Ai for Owner {
    fn flag(&self, on: bool) {
        self.ok("flags.set", json!({"flag": "ai.generative", "value": on}));
    }

    fn install(&self, file: &ModelFile) {
        self.ok_as(
            self.admin,
            GRANT,
            json!({
                "module_id": AI_MODULE,
                "capability": file.download,
                "scope": {"resource": file.resource, "version": file.version, "origin": DEVELOPER_ORIGIN},
            }),
        );
        let queued = self.ok(
            INSTALL,
            json!({"module_id": AI_MODULE, "resource_id": file.resource}),
        );
        let job = self.finished(&queued["job_id"]);
        assert_eq!(job["status"], "ready", "{job}");
    }

    fn fill(&self, params: Value) -> Result<Value, ApiFailure> {
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
        panic!("the fill kept asking for preparation");
    }

    fn generate(&self, params: Value) -> Value {
        let queued = self.fill(params).expect("the fill is queued");
        let job = self.finished(&queued["job_id"]);
        assert_eq!(job["status"], "ready", "{job}");
        job
    }

    fn asset_revision(&self, asset: &AssetId) -> u64 {
        self.ok("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    }

    fn sample(&self, asset: &AssetId, x: u32, y: u32) -> Value {
        let params = json!({"asset_id": asset, "x": x, "y": y});
        for _ in 0..4 {
            let response = self.call(self.edit, "render.sample", params.clone());
            match response.error {
                None => return response.result.unwrap()["rgba"].clone(),
                Some(error) if error.code == "preparation-required" => {
                    let job = self.finished(&json!(error.job_id.unwrap()));
                    assert_eq!(job["status"], "ready", "{job}");
                }
                Some(error) => panic!("render.sample: {error:?}"),
            }
        }
        panic!("the sample kept asking for preparation");
    }

    fn accept_as(
        &self,
        client: crate::api::ClientId,
        asset: &AssetId,
        job: &Value,
        candidate: u64,
    ) -> Result<Value, ApiFailure> {
        let revision = self.asset_revision(asset);
        let response = self.call(
            client,
            ACCEPT,
            json!({
                "asset_id": asset,
                "mutation": mutation_json(revision, &uuid::Uuid::new_v4().to_string()),
                "job_id": job,
                "candidate": candidate,
            }),
        );
        match response.error {
            None => Ok(response.result.unwrap()),
            Some(error) => Err(error),
        }
    }

    fn states(&self, job: &Value) -> Vec<String> {
        self.job(job)["candidates"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| row["state"].as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn history_labels(&self, asset: &AssetId) -> Vec<String> {
        self.ok("history.list", json!({"asset_id": asset, "limit": 8}))["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["label"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn generate_regenerate_accept_undo_redo_and_reopen_without_the_model() {
    let fixture = Fixture::new("journey");
    let [asset, _] = fixture.assets.clone();
    let owner = fixture.ready();
    let tiers = &owner.status(AI_MODULE)["fill_tiers"];
    assert_eq!(tiers[0]["offered"], true, "{tiers}");
    assert_eq!(tiers[0]["missing"], json!([]));
    assert_eq!(tiers[0]["models"][0]["state"], "installed");
    assert_eq!(
        tiers[0]["models"][0]["text_encoder"]["state"],
        "not-installed"
    );
    // A pixel the region never covers, and its centre.
    let (outside, centre) = ((20, 20), (240, 160));
    let before = (
        owner.sample(&asset, outside.0, outside.1),
        owner.sample(&asset, centre.0, centre.1),
    );
    let revision = owner.asset_revision(&asset);

    // Generate makes one candidate unless asked for more (owner, 2026-10-11).
    let single = owner.generate(with(selection(&asset), json!({"seed": 7})));
    let result = &single["result"]["result"];
    assert_eq!(result["candidates"], json!([0]), "{single}");
    assert_eq!(result["seeds"], json!([7]));
    assert_eq!(result["next_seed"], 8);
    assert_eq!(single["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(fixture.controls().generations(), 1);

    // Three candidates from consecutive seeds in one job.
    let first = owner.generate(with(selection(&asset), json!({"seed": 7, "candidates": 3})));
    let result = &first["result"]["result"];
    assert_eq!(result["candidates"], json!([0, 1, 2]), "{first}");
    assert_eq!(result["seeds"], json!([7, 8, 9]));
    assert_eq!(result["next_seed"], 10);
    assert_eq!(result["tier"], "generative");
    assert_eq!(result["prompt"], false);
    assert_eq!(fixture.controls().generations(), 4);
    let rows = first["candidates"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row["state"], "live");
        assert_eq!(row["label"], "Generative fill");
        assert_eq!(row["rect"], result["rect"]);
        assert_eq!(row["asset_id"], json!(asset));
        let provenance = &row["provenance"];
        assert_eq!(provenance["seed"], json!(7 + index), "{provenance}");
        assert_eq!(provenance["tier"], "generative");
        assert_eq!(provenance["model"], FAKE_GENERATOR.resource);
        assert_eq!(provenance["model_sha256"], FAKE_GENERATOR.sha256);
        assert_eq!(provenance["sampler"], "fake-blocks");
        assert_eq!(provenance["encoding"], "srgb");
        assert!(provenance.get("prompt_hash").is_none());
    }
    let rect = result["rect"].as_array().unwrap();
    let (width, height) = (rect[2].as_u64().unwrap(), rect[3].as_u64().unwrap());
    assert!(
        rect[0].as_u64().unwrap() <= 240 - 40 && width >= 96 && height >= 40,
        "{rect:?}"
    );
    // The patch reads back in chunks to its whole length, and the preview too.
    let whole = owner.ok(
        READ_CANDIDATE,
        json!({"job_id": first["job_id"], "candidate": 1, "part": "patch"}),
    );
    assert_eq!(
        whole["total"],
        json!(width * height * 8),
        "{}",
        whole["total"]
    );
    let slice = owner.ok(
        READ_CANDIDATE,
        json!({"job_id": first["job_id"], "candidate": 1, "part": "patch", "offset": 8, "length": 16}),
    );
    assert_eq!(slice["length"], 16);
    let preview = owner.ok(
        READ_CANDIDATE,
        json!({"job_id": first["job_id"], "candidate": 1, "part": "preview"}),
    );
    let size = &rows[1]["preview"];
    assert_eq!(
        preview["total"],
        json!(size["width"].as_u64().unwrap() * size["height"].as_u64().unwrap() * 3)
    );
    // The seeds differ, so the candidates do.
    let other = owner.ok(
        READ_CANDIDATE,
        json!({"job_id": first["job_id"], "candidate": 2, "part": "preview"}),
    );
    assert_ne!(preview["data"], other["data"]);
    assert_eq!(
        owner.asset_revision(&asset),
        revision,
        "generating edits nothing"
    );

    // Regenerate: another job from the next seed; the first job's candidates stay live.
    let second = owner.generate(with(
        selection(&asset),
        json!({"seed": result["next_seed"], "candidates": 2}),
    ));
    assert_eq!(second["result"]["result"]["seeds"], json!([10, 11]));
    assert_eq!(owner.states(&first["job_id"]), ["live", "live", "live"]);
    assert_eq!(owner.states(&second["job_id"]), ["live", "live"]);
    // The same request again makes the same candidate.
    let again = owner.generate(with(selection(&asset), json!({"seed": 7, "candidates": 1})));
    let read = |job: &Value| {
        owner.ok(
            READ_CANDIDATE,
            json!({"job_id": job, "candidate": 0, "part": "patch"}),
        )["data"]
            .clone()
    };
    assert_eq!(read(&again["job_id"]), read(&first["job_id"]));
    assert_eq!(read(&single["job_id"]), read(&first["job_id"]));

    // Accept the second job's second candidate from the other client.
    let accepted = owner
        .accept_as(owner.admin, &asset, &second["job_id"], 1)
        .unwrap();
    assert_eq!(accepted["outcome"], "applied", "{accepted}");
    assert_eq!(owner.asset_revision(&asset), revision + 1);
    assert_eq!(owner.states(&second["job_id"]), ["live", "accepted"]);
    assert_eq!(owner.history_labels(&asset)[0], "Add Generative fill 1");
    let after = (
        owner.sample(&asset, outside.0, outside.1),
        owner.sample(&asset, centre.0, centre.1),
    );
    assert_eq!(after.0, before.0, "a pixel the region does not cover");
    assert_ne!(after.1, before.1, "the region is filled");
    // Every other candidate was generated against the stack without the patch.
    let stale = owner
        .accept_as(owner.edit, &asset, &first["job_id"], 0)
        .unwrap_err();
    assert_eq!(stale.code, "conflict", "{stale:?}");
    assert_eq!(stale.data, Some(json!({"reasons": ["earlier-correction"]})));
    assert_eq!(owner.asset_revision(&asset), revision + 1);

    // Undo and redo.
    let undo = |method: &str| {
        let revision = owner.asset_revision(&asset);
        owner.ok(
            method,
            json!({"asset_id": asset, "mutation": mutation_json(revision, method)}),
        );
    };
    undo("history.undo");
    assert_eq!(owner.sample(&asset, centre.0, centre.1), before.1);
    undo("history.redo");
    assert_eq!(owner.sample(&asset, centre.0, centre.1), after.1);
    drop(owner);

    // A host with no luxforge.ai and no runtime renders the accepted patch.
    let reopened = fixture.start_without_ai();
    assert_eq!(reopened.sample(&asset, centre.0, centre.1), after.1);
    assert_eq!(reopened.sample(&asset, outside.0, outside.1), before.0);
    assert_eq!(fixture.controls().generations(), 7);
}

#[test]
fn a_cancel_mid_generation_leaves_the_recipe_untouched_and_publishes_nothing() {
    let fixture = Fixture::new("cancel");
    let [asset, _] = fixture.assets.clone();
    let owner = fixture.ready();
    let revision = owner.asset_revision(&asset);
    fixture.controls().set_step_delay(Duration::from_millis(20));
    let queued = owner
        .fill(with(selection(&asset), json!({"seed": 1})))
        .unwrap();
    let job = queued["job_id"].clone();
    owner.until(&job, |record| {
        record["progress"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("step"))
    });
    owner.ok("job.cancel", json!({"job_id": job}));
    let record = owner.finished(&job);
    assert_eq!(record["status"], "cancelled", "{record}");
    assert_eq!(fixture.controls().generations(), 0);
    assert!(owner.states(&job).is_empty(), "{record}");
    assert_eq!(owner.asset_revision(&asset), revision);
    assert_eq!(
        owner
            .accept_as(owner.edit, &asset, &job, 0)
            .unwrap_err()
            .code,
        "validation"
    );
}

#[test]
fn a_running_fill_locks_its_photograph_only_and_a_cancel_releases_it() {
    let fixture = Fixture::new("lock");
    let [asset, other] = fixture.assets.clone();
    let owner = fixture.ready();
    let revision = owner.asset_revision(&asset);
    fixture.gate.shut();
    let queued = owner
        .fill(with(selection(&asset), json!({"seed": 3, "candidates": 2})))
        .unwrap();
    let job = queued["job_id"].clone();
    wait_for("the first candidate to be published", || {
        Some(()).filter(|_| fixture.gate.holding())
    });
    // Progress reads while the job runs.
    let record = owner.job(&job);
    assert_eq!(record["status"], "running", "{record}");
    assert_eq!(record["asset_id"], json!(asset));
    assert_eq!(owner.states(&job), ["live"]);

    let locked = |failure: ApiFailure| {
        assert_eq!(failure.code, "conflict", "{failure:?}");
        let data = failure.data.expect("the lock is named");
        assert_eq!(data["reason"], "locked");
        assert_eq!(data["job_id"], job);
        assert_eq!(data["method"], TASK);
        assert_eq!(data["asset_id"], json!(asset));
    };
    let basic = |asset: &AssetId, revision: u64| json!({"asset_id": asset, "mutation": mutation_json(revision, &uuid::Uuid::new_v4().to_string()), "exposure": 0.5});
    locked(owner.fail("edit.set-basic", basic(&asset, revision)));
    locked(owner.accept_as(owner.edit, &asset, &job, 0).unwrap_err());
    locked(owner.fail(
        "history.undo",
        json!({"asset_id": asset, "mutation": mutation_json(revision, "undo")}),
    ));
    locked(owner.fill(selection(&asset)).unwrap_err());
    // Every other photograph stays editable.
    let other_revision = owner.asset_revision(&other);
    owner.ok("edit.set-basic", basic(&other, other_revision));
    assert_eq!(owner.asset_revision(&other), other_revision + 1);

    // A cancel releases the lock at once; the held worker stops at its next boundary, and its
    // late work reaches neither the candidates nor the recipe.
    owner.ok("job.cancel", json!({"job_id": job}));
    owner.ok("edit.set-basic", basic(&asset, revision));
    assert_eq!(owner.asset_revision(&asset), revision + 1);
    fixture.gate.open();
    let record = owner.finished(&job);
    assert_eq!(record["status"], "cancelled", "{record}");
    assert_eq!(owner.states(&job), ["released"]);
    assert_eq!(record["candidates"][0]["released"], "cancelled");
    assert_eq!(fixture.controls().generations(), 1);
    let late = owner.accept_as(owner.edit, &asset, &job, 0).unwrap_err();
    assert_eq!(late.code, "conflict", "{late:?}");
    assert_eq!(owner.asset_revision(&asset), revision + 1);
}

#[test]
fn every_refusal_is_named_and_generates_nothing() {
    let fixture = Fixture::new("refusals");
    let [asset, _] = fixture.assets.clone();
    let owner = fixture.start();
    let requirements = |failure: &ApiFailure| -> Vec<(String, String)> {
        failure.data.as_ref().expect("requirements")["requirements"]
            .as_array()
            .unwrap_or_else(|| panic!("{failure:?}"))
            .iter()
            .map(|row| {
                (
                    row["kind"].as_str().unwrap().to_owned(),
                    row["id"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };

    // Nothing set up: the flag is off and the model is not installed, listed together.
    let tiers = owner.status(AI_MODULE)["fill_tiers"].clone();
    assert_eq!(tiers[0]["offered"], false);
    assert_eq!(
        tiers[0]["missing"],
        json!([
            {"kind": "flag", "id": "ai.generative", "state": "off"},
            {"kind": "model", "id": "generative", "state": "not-installed"},
        ])
    );
    let refused = owner.fill(selection(&asset)).unwrap_err();
    assert_eq!(refused.code, "not-ready", "{refused:?}");
    let missing = requirements(&refused);
    // Declared by the task and named by its needs, the flag is one requirement.
    assert_eq!(
        missing
            .iter()
            .filter(|(kind, id)| kind == "flag" && id == "ai.generative")
            .count(),
        1,
        "{missing:?}"
    );
    assert!(
        missing.iter().any(|(_, id)| id.contains("fake-generator")),
        "{missing:?}"
    );

    owner.flag(true);
    owner.install(&FAKE_GENERATOR);
    // A prompt needs the text encoder: refused by name, so a client offers its download.
    let replace = with(selection(&asset), json!({"prompt": "a red bicycle"}));
    let refused = owner.fill(replace.clone()).unwrap_err();
    assert_eq!(refused.code, "not-ready", "{refused:?}");
    assert!(
        requirements(&refused)
            .iter()
            .any(|(_, id)| id == FAKE_TEXT_ENCODER.download || id == FAKE_TEXT_ENCODER.resource),
        "{refused:?}"
    );
    // A blank prompt is none.
    owner.generate(with(
        selection(&asset),
        json!({"prompt": "   ", "candidates": 1}),
    ));
    // A prompt past 256 characters, by the declared schema.
    let long = with(selection(&asset), json!({"prompt": "a".repeat(257)}));
    assert_eq!(owner.fill(long).unwrap_err().code, "validation");
    // A region past half the frame.
    let wide = with(
        selection(&asset),
        json!({"selection-points": [[0.0, 0.5], [1.0, 0.5]], "selection-radius": 0.25}),
    );
    let refused = owner.fill(wide).unwrap_err();
    assert_eq!(refused.code, "resource-limit", "{refused:?}");
    assert_eq!(refused.data.unwrap()["limit"], "half-frame");
    // A selection wholly outside the photograph covers nothing.
    let outside = with(
        selection(&asset),
        json!({"selection-points": [[-0.5, -0.5]], "selection-radius": 0.01}),
    );
    let refused = owner.fill(outside).unwrap_err();
    assert_eq!(refused.code, "validation", "{refused:?}");
    // An unknown model and an effect radius without its path.
    let unknown = with(selection(&asset), json!({"model": "elsewhere"}));
    assert_eq!(owner.fill(unknown).unwrap_err().code, "validation");
    let effect = with(selection(&asset), json!({"effect-radius": 0.05}));
    assert_eq!(owner.fill(effect).unwrap_err().code, "validation");
    assert_eq!(fixture.controls().generations(), 1);

    // With the encoder installed the prompt makes the fill Replace.
    owner.install(&FAKE_TEXT_ENCODER);
    let job = owner.generate(with(replace, json!({"candidates": 1, "seed": 5})));
    assert_eq!(job["result"]["result"]["prompt"], true);
    let row = &job["candidates"][0];
    assert_eq!(row["label"], "Replace \"a red bicycle\"");
    assert!(row["provenance"]["prompt_hash"].is_string(), "{row}");
    // Replace is the generator's, held by its own slot with the encoder, and is not blended.
    assert_eq!(row["provenance"]["model"], FAKE_GENERATOR.resource);
    assert_eq!(row["provenance"]["blend"], "none");
    let held: Vec<Value> = owner.status(AI_MODULE)["runtime"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["resource_id"].clone())
        .collect();
    assert!(
        held.contains(&json!(FAKE_TEXT_ENCODER.resource)),
        "{held:?}"
    );
    owner
        .accept_as(owner.edit, &asset, &job["job_id"], 0)
        .unwrap();
    assert_eq!(
        owner.history_labels(&asset)[0],
        "Add Replace \"a red bicycle\" 1"
    );

    // The flag off again refuses before anything runs.
    owner.flag(false);
    let refused = owner.fill(selection(&asset)).unwrap_err();
    assert_eq!(refused.code, "not-ready");
    assert_eq!(
        requirements(&refused),
        [("flag".into(), "ai.generative".into())]
    );
    assert_eq!(fixture.controls().generations(), 2);
}

#[test]
fn admission_refuses_a_region_past_a_candidate_or_the_artifact_budget() {
    let module = AiModule::developer();
    let task = |points: Value, radius: f64| -> Map<String, Value> {
        json!({"selection-points": points, "selection-radius": radius, "model": "fake-generator"})
            .as_object()
            .unwrap()
            .clone()
    };
    let admit = |parameters: &Map<String, Value>, stage: Stage, artifact_bytes: u64| {
        module.admit_task(
            FILL_GENERATIVE,
            parameters,
            &TaskAdmission {
                stage: Some(stage),
                artifact_bytes,
            },
        )
    };
    // A 100-megapixel frame: a quarter-height stroke across it is past one candidate long before
    // it is past half the frame.
    let large = Stage {
        width: 12_000,
        height: 8_400,
    };
    let across = task(json!([[0.1, 0.5], [0.6, 0.5]]), 0.25);
    let refused = admit(&across, large, 0).unwrap_err();
    assert_eq!(refused.kind.code(), "resource-limit", "{refused}");
    assert_eq!(refused.data.unwrap()["limit"], "candidate");

    // A small region admits, until the stack's artifacts leave no room for its patch.
    let small = task(json!([[0.5, 0.5]]), 0.05);
    let stage = Stage {
        width: 4000,
        height: 3000,
    };
    admit(&small, stage, 0).unwrap();
    admit(&small, stage, PREPARED_ARTIFACT_BYTES / 2).unwrap();
    let refused = admit(&small, stage, PREPARED_ARTIFACT_BYTES).unwrap_err();
    assert_eq!(refused.kind.code(), "resource-limit");
    assert_eq!(refused.data.unwrap()["limit"], "artifacts");
}

#[test]
fn the_developer_transport_serves_only_the_pinned_models_and_install_checks_their_bytes() {
    let module = AiModule::developer();
    let root = temp("ai-validate");
    fs::create_dir_all(&root).unwrap();
    for file in [FAKE_GENERATOR, FAKE_TEXT_ENCODER] {
        use sha2::{Digest, Sha256};
        assert_eq!(
            hex(&Sha256::digest(file.bytes)),
            file.sha256,
            "{}",
            file.resource
        );
        let path = root.join(file.resource);
        fs::write(&path, file.bytes).unwrap();
        module.validate_resource(file.resource, &path).unwrap();
        fs::write(&path, b"something else").unwrap();
        assert_eq!(
            module
                .validate_resource(file.resource, &path)
                .unwrap_err()
                .kind
                .code(),
            "validation"
        );
        assert!(file.url().starts_with(DEVELOPER_ORIGIN));
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn an_ordinary_run_offers_klein_pinned_and_held_against_the_machine_floor() {
    use super::models::{
        CONDITIONING_ASSET, CONDITIONING_RELEASE_TAG, CONDITIONING_URL, KLEIN_GENERATOR,
        KLEIN_TEXT_ENCODER,
    };
    use crate::capabilities::descriptor::{CapabilityKind, machine_floor};
    let registry = ModuleRegistry::builtin();
    let module = registry
        .module(AI_MODULE)
        .expect("every run serves luxforge.ai");
    let descriptor = module.descriptor();
    assert!(!descriptor.developer);
    descriptor.validate().unwrap();
    let resources: Vec<_> = descriptor.resources.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        resources,
        [KLEIN_GENERATOR.resource, KLEIN_TEXT_ENCODER.resource]
    );
    // The fill task declares its experiment, so a client hides it without reading the status.
    let task = descriptor.task(FILL_GENERATIVE).unwrap();
    assert_eq!(task.flags, ["ai.generative"]);
    for resource in &descriptor.resources {
        assert!(
            resource.url.starts_with("https://huggingface.co/"),
            "{}",
            resource.url
        );
        assert!(
            resource.url.contains("/resolve/"),
            "pinned by revision: {}",
            resource.url
        );
        assert_eq!(resource.license, "Apache-2.0");
        // Offered only under the tier's experiment, which a client reads from the descriptor.
        assert_eq!(resource.flag.as_deref(), Some("ai.generative"));
        assert!(resource.use_restriction.is_none());
        assert!(resource.tradeoff.is_some());
        assert_eq!(resource.sha256.len(), 64);
        for file in &resource.files {
            assert_eq!(file.sha256.len(), 64, "{}", file.name);
            assert!(file.bytes > 0);
        }
    }
    // The removal conditioning is the project's own release asset, named by its hash.
    assert_eq!(
        CONDITIONING_URL,
        format!(
            "https://github.com/Willyham/luxforge/releases/download/{CONDITIONING_RELEASE_TAG}/{CONDITIONING_ASSET}"
        )
    );
    let conditioning = KLEIN_GENERATOR
        .files
        .iter()
        .find(|file| file.url == CONDITIONING_URL)
        .unwrap();
    assert!(CONDITIONING_ASSET.contains(&conditioning.sha256[..8]));
    // Removal holds the generator, Replace the encoder with the generator, each refused below
    // the memory its resource asks for.
    let runtimes: Vec<_> = descriptor
        .capabilities
        .iter()
        .filter_map(|capability| match &capability.kind {
            CapabilityKind::LocalRuntime {
                resource,
                resident_bytes,
                backends,
                with,
                ..
            } => Some((
                capability.id.as_str(),
                resource.as_str(),
                *resident_bytes,
                backends.clone(),
                with.clone(),
            )),
            _ => None,
        })
        .collect();
    const GIB: u64 = 1 << 30;
    assert_eq!(
        runtimes,
        [
            (
                "run-klein",
                KLEIN_GENERATOR.resource,
                5 * GIB,
                vec!["metal".to_owned(), "diffusion-cpu".to_owned()],
                vec![]
            ),
            (
                "run-klein-replace",
                KLEIN_TEXT_ENCODER.resource,
                9 * GIB,
                vec!["metal".to_owned(), "diffusion-cpu".to_owned()],
                vec![KLEIN_GENERATOR.resource.to_owned()]
            ),
        ]
    );
    for (file, resident) in [(KLEIN_GENERATOR, 5 * GIB), (KLEIN_TEXT_ENCODER, 9 * GIB)] {
        let min = file.min_memory_bytes.unwrap();
        assert!(min >= machine_floor(resident), "{}", file.resource);
    }
    assert_eq!(KLEIN_GENERATOR.min_memory_bytes, Some(16 * GIB));
    assert_eq!(KLEIN_TEXT_ENCODER.min_memory_bytes, Some(24 * GIB));
    // No developer fixture reaches an ordinary run.
    assert!(
        descriptor
            .resources
            .iter()
            .all(|resource| !resource.url.starts_with(DEVELOPER_ORIGIN))
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
