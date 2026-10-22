//! The host-planned reference read (`read.rs`) driven through the catalog owner as a client drives
//! a task that declares `reads: repair-input`: its values are byte-identical to the reference
//! renderer's own read of the same stage, whatever tile provider the owner holds; it reports the
//! input identity a candidate made from it is checked by, so accepting one succeeds and a stale one
//! is refused `conflict`; a cancel stops it; a second open reader and a read past the pixel bound
//! are refused `resource-limit`; and a task that declares no read cannot open one.
use super::{
    descriptor::{ReadBoundary, TaskDescriptor},
    host::{HostConfig, TASK_PREFIX},
    read::MAX_MODULE_READ_PIXELS,
    secrets::MemorySecretStore,
    testing::{MemoryTransport, Owner, temp},
};
use crate::{
    ActionInput, ActionPlan, AssetId, Cancel, CapabilityModule, EditorService, Error,
    ModuleDescriptor, ModuleRegistry, ParameterDescriptor, Processing, Region, Stage, StageContext,
    ToolModule,
    artifacts::candidates::{CandidatePatch, CandidateShape, PatchColour},
    capabilities::context::ModuleContext,
    editor::mutation_json,
    jobs::JOB_CANCEL,
    tiles::{
        BandStream, ReadStage, ReferenceReads, TileCall, TileFallback, TileService, TileStatus,
        region::{self, RegionRequest, RegionStage},
    },
};
use luxforge_testbase::{Gate, wait_for};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, sync::Arc};

const MODULE: &str = "test.reads";
const TASK: &str = "read-test-stage";
const BLIND: &str = "read-test-nothing";

fn integer(name: &str, max: i64, default: i64) -> ParameterDescriptor {
    ParameterDescriptor::integer(name, 0, max)
        .default(default)
        .notes("test")
}

fn task(id: &str, reads: Option<ReadBoundary>) -> TaskDescriptor {
    TaskDescriptor {
        id: id.into(),
        title: id.into(),
        notes: "test".into(),
        asset: true,
        profile: false,
        uses: Vec::new(),
        parameters: vec![
            integer("x", 100_000, 0),
            integer("y", 100_000, 0),
            integer("width", 100_000, 16),
            integer("height", 100_000, 16),
            integer("publish", 2, 0),
            ParameterDescriptor::boolean("second")
                .default(false)
                .notes("open a second reader while the first is open"),
            ParameterDescriptor::boolean("hold")
                .default(false)
                .notes("wait at the module's gate with the reader open"),
        ],
        apply: None,
        reads,
        flags: Vec::new(),
    }
}

/// A module whose task reads one rectangle of the repair stage's input, answers its values' hash
/// and its input identity, and may publish candidates made from it.
struct ReadModule {
    descriptor: ModuleDescriptor,
    gate: Arc<Gate>,
}

impl ReadModule {
    fn shared(gate: Arc<Gate>) -> Arc<dyn ToolModule> {
        Arc::new(Self {
            descriptor: ModuleDescriptor {
                id: MODULE.into(),
                title: "Read test".into(),
                tasks: vec![
                    task(TASK, Some(ReadBoundary::RepairInput)),
                    task(BLIND, None),
                ],
                ..ModuleDescriptor::default()
            },
            gate,
        })
    }
}

impl ToolModule for ReadModule {
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

/// The hex SHA-256 of linear values' little-endian bytes.
fn digest(linear: &[[f32; 3]]) -> String {
    let mut hash = Sha256::new();
    for value in linear.iter().flatten() {
        hash.update(value.to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}

impl CapabilityModule for ReadModule {
    fn run_task(
        &self,
        _: &str,
        parameters: &Map<String, Value>,
        context: &ModuleContext,
    ) -> Result<Value, Error> {
        let number = |name: &str| {
            parameters
                .get(name)
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32
        };
        let flag = |name: &str| parameters.get(name).and_then(Value::as_bool) == Some(true);
        let mut reader = context.stage_reader()?;
        let second = flag("second").then(|| match context.stage_reader() {
            Ok(_) => json!("opened"),
            Err(error) => json!(error.kind.code()),
        });
        if flag("hold") {
            self.gate.pass_unless(|| context.is_cancelled());
        }
        let read = reader.read(Region {
            x0: number("x"),
            y0: number("y"),
            width: number("width"),
            height: number("height"),
        })?;
        assert_eq!(read.boundary, reader.boundary());
        assert_eq!(read.stage, reader.stage());
        assert_eq!(&read.input, reader.input());
        drop(reader);
        let mut candidates = Vec::new();
        for candidate in 0..number("publish") {
            let (width, height) = (read.rect.width, read.rect.height);
            let reservation = context.reserve_candidate(CandidateShape {
                width,
                height,
                preview_width: 1,
                preview_height: 1,
            })?;
            let linear: Vec<f32> = read
                .linear
                .iter()
                .flatten()
                .map(|value| 1.0 - value + candidate as f32 * 0.1)
                .collect();
            let coverage = vec![1.0; (width * height) as usize];
            candidates.push(reservation.publish(CandidatePatch {
                rect: [read.rect.x0, read.rect.y0, width, height],
                colour: PatchColour::LinearSrgb(&linear),
                coverage: &coverage,
                preview: &[128, 128, 128],
                preview_width: 1,
                preview_height: 1,
                label: "Read",
                provenance: Map::new(),
            })?);
        }
        Ok(json!({
            "boundary": read.boundary.name(),
            "stage": [read.stage.width, read.stage.height],
            "rect": [read.rect.x0, read.rect.y0, read.rect.width, read.rect.height],
            "pixels": read.linear.len(),
            "digest": digest(&read.linear),
            "input": read.input,
            "second": second,
            "candidates": candidates,
        }))
    }
}

/// A tile provider that claims the GPU and refuses every read and stream, so any read that went
/// through the owner's tile queue would fail.
struct RefusingTiles;

impl TileService for RefusingTiles {
    fn status(&self) -> TileStatus {
        TileStatus::Gpu
    }
    fn submit(&self, call: TileCall) {
        call.refuse(Error::internal("the refusing provider reads no pixels"));
    }
    fn disconnect(&self, _: crate::ClientId) {}
    fn stream(
        &self,
        _: &crate::Evaluation,
        _: crate::tiles::OutputSpec,
        _: &Cancel,
    ) -> Result<BandStream, TileFallback> {
        Err(TileFallback::Budget {
            requested: 1,
            budget: 0,
        })
    }
    fn stop(&self) {}
}

/// One imported photograph, the module's gate and an owner whose tile provider refuses.
struct Fixture {
    root: PathBuf,
    asset: AssetId,
    gate: Arc<Gate>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = temp(&format!("reads-{name}"));
        fs::create_dir_all(&root).unwrap();
        let gate = Arc::new(Gate::new());
        let mut service =
            EditorService::open_with(&root.join("catalog.sqlite"), Self::registry(&gate)).unwrap();
        let asset = service
            .import(&luxforge_testbase::paths::fixture("s0/orientation-1.jpg"))
            .unwrap()
            .asset
            .id;
        Self { root, asset, gate }
    }

    fn catalog(&self) -> PathBuf {
        self.root.join("catalog.sqlite")
    }

    fn registry(gate: &Arc<Gate>) -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
        registry.register(ReadModule::shared(gate.clone())).unwrap();
        Arc::new(registry)
    }

    fn start(&self) -> Owner {
        Owner::start(
            &self.catalog(),
            Self::registry(&self.gate),
            HostConfig {
                config_dir: Some(self.root.join("config")),
                resource_dir: Some(self.root.join("resources")),
                secrets: Arc::new(MemorySecretStore::new()),
                transport: MemoryTransport::new(|_, answer| answer.whole(404, b"")),
                tiles: Some(Arc::new(RefusingTiles)),
                ..HostConfig::unconfigured()
            },
        )
    }

    /// The asset's current entry as a service of its own binds it, prepared.
    fn service(&self) -> EditorService {
        let mut service =
            EditorService::open_with(&self.catalog(), Self::registry(&self.gate)).unwrap();
        service
            .prepare(&service.entry_needs(&self.asset, None).unwrap())
            .unwrap();
        service
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.open();
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Request `task` of the fixture's photograph, preparing its source first when the owner asks, and
/// answer the queued job's identity.
fn request(owner: &Owner, fixture: &Fixture, task: &str, mut params: Value) -> Value {
    params["asset_id"] = json!(fixture.asset);
    wait_for("the task to be queued", || {
        let response = owner.call(owner.edit, &format!("{TASK_PREFIX}{task}"), params.clone());
        match (response.result, response.error) {
            (Some(result), _) => Some(result["job_id"].clone()),
            (_, Some(error)) if error.code == "preparation-required" => {
                let job = error.job_id.expect("a queued preparation");
                owner.finished(&json!(job));
                None
            }
            (_, error) => panic!("{task}: {error:?}"),
        }
    })
}

/// Run `task` to its end and answer its record.
fn run(owner: &Owner, fixture: &Fixture, task: &str, params: Value) -> Value {
    let job = request(owner, fixture, task, params);
    owner.finished(&job)
}

#[test]
fn a_read_is_the_reference_renderers_own_and_reports_the_identity_accept_checks() {
    let fixture = Fixture::new("identity");
    let owner = fixture.start();
    // A colour edit, which sits after the repair stage: the read is the stage before it.
    let revision = owner.ok("asset.state", json!({"asset_id": fixture.asset}))["revision"]
        .as_u64()
        .unwrap();
    owner.ok(
        "edit.set-basic",
        json!({"asset_id": fixture.asset, "mutation": mutation_json(revision, "exposure"),
               "exposure": 0.5}),
    );
    let rect = json!({"x": 30, "y": 20, "width": 24, "height": 16});
    let mut params = rect.clone();
    params["publish"] = json!(2);
    let job = run(&owner, &fixture, TASK, params);
    assert_eq!(job["status"], "ready", "{job}");
    let result = job["result"]["result"].clone();
    assert_eq!(result["boundary"], "repair-input");
    assert_eq!(result["rect"], json!([30, 20, 24, 16]));
    assert_eq!(result["pixels"], 24 * 16);
    // The candidates were made from the read, and carry the identity it reported.
    let rows = owner.job(&job["job_id"])["candidates"].clone();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    for row in rows.as_array().unwrap() {
        assert_eq!(row["input"], result["input"], "{row}");
    }
    // Accepting one succeeds; the other was made against the stack without it.
    let revision = owner.ok("asset.state", json!({"asset_id": fixture.asset}))["revision"]
        .as_u64()
        .unwrap();
    let accept = |candidate: u64, revision: u64, request: &str| {
        json!({"asset_id": fixture.asset, "mutation": mutation_json(revision, request),
               "job_id": job["job_id"], "candidate": candidate})
    };
    let accepted = owner.ok(
        "edit.accept-correction-patch",
        accept(0, revision, "accept"),
    );
    assert_eq!(accepted["outcome"], "applied");
    let stale = owner.fail(
        "edit.accept-correction-patch",
        accept(1, revision + 1, "stale"),
    );
    assert_eq!(stale.code, "conflict", "{stale:?}");
    // A read now reports the new top correction's identity: only the earlier corrections changed.
    let after = run(&owner, &fixture, TASK, rect.clone())["result"]["result"].clone();
    let (before_input, after_input) = (&result["input"], &after["input"]);
    for same in ["source", "dimensions", "upstream"] {
        assert_eq!(before_input[same], after_input[same], "{same}");
    }
    assert_ne!(before_input["earlier"], after_input["earlier"]);
    owner.stop();
    // With a repair layer, the repair stage's output is the stage after it, named by the same
    // identity as its input.
    {
        let service = fixture.service();
        let plan = |boundary| service.stage_plan(&fixture.asset, boundary).unwrap();
        let (input, output) = (
            plan(ReadBoundary::RepairInput),
            plan(ReadBoundary::RepairOutput),
        );
        assert!(matches!(input.stage, ReadStage::Before { layer: 0, .. }));
        assert!(matches!(output.stage, ReadStage::Before { layer: 1, .. }));
        assert_eq!(output.input, input.input);
        assert_eq!(output.size, input.size);
    }

    // Undone, the read again answers what it answered first, and the reference renderer's own read
    // of the same stage of an independently bound entry holds exactly the same values.
    let owner = fixture.start();
    let revision = owner.ok("asset.state", json!({"asset_id": fixture.asset}))["revision"]
        .as_u64()
        .unwrap();
    owner.ok(
        "history.undo",
        json!({"asset_id": fixture.asset, "mutation": mutation_json(revision, "undo")}),
    );
    let again = run(&owner, &fixture, TASK, rect)["result"]["result"].clone();
    assert_eq!(again["digest"], result["digest"]);
    assert_eq!(again["input"], result["input"]);
    owner.stop();
    let service = fixture.service();
    let plan = service
        .stage_plan(&fixture.asset, ReadBoundary::RepairInput)
        .unwrap();
    assert_eq!(serde_json::to_value(&plan.input).unwrap(), result["input"]);
    let ReadStage::Before { layer, .. } = plan.stage else {
        panic!("the repair stage's input is the stage before a layer");
    };
    assert_eq!(layer, 0, "the repair stage comes before the Basic layer");
    // Without a repair layer its output is its input.
    let output = service
        .stage_plan(&fixture.asset, ReadBoundary::RepairOutput)
        .unwrap();
    assert!(matches!(output.stage, ReadStage::Before { layer: 0, .. }));
    assert_eq!(
        result["stage"],
        json!([plan.input.dimensions[0], plan.input.dimensions[1]])
    );
    let reference = region::read(
        &plan.evaluation,
        &RegionRequest {
            stage: RegionStage::Before(layer),
            rect: Region {
                x0: 30,
                y0: 20,
                width: 24,
                height: 16,
            },
            reduction: 1,
            mode: None,
        },
        &ReferenceReads,
        &Cancel::never(),
    )
    .unwrap();
    assert_eq!(reference.size, [24, 16]);
    let expected: Vec<[f32; 3]> = reference
        .linear
        .iter()
        .map(|mean| mean.map(|channel| channel as f32))
        .collect();
    assert!(
        reference
            .linear
            .iter()
            .zip(&expected)
            .all(|(mean, value)| mean.iter().zip(value).all(|(a, b)| *a == f64::from(*b))),
        "a one-pixel mean is the pixel's own value"
    );
    assert_eq!(result["digest"], digest(&expected));
}

#[test]
fn a_cancelled_read_stops_and_releases_its_place() {
    let fixture = Fixture::new("cancel");
    let owner = fixture.start();
    fixture.gate.shut();
    let job = request(&owner, &fixture, TASK, json!({"hold": true}));
    wait_for("the reader to be open at the gate", || {
        Some(()).filter(|_| fixture.gate.holding())
    });
    owner.ok(JOB_CANCEL, json!({"job_id": job}));
    let cancelled = owner.finished(&job);
    assert_eq!(cancelled["status"], "cancelled", "{cancelled}");
    // The reader's place was returned: the next task opens one, and a second beside it is refused.
    fixture.gate.open();
    let next = run(&owner, &fixture, TASK, json!({"second": true}));
    assert_eq!(next["status"], "ready", "{next}");
    assert_eq!(next["result"]["result"]["second"], "resource-limit");
}

#[test]
fn a_read_past_the_pixel_bound_is_refused_and_a_task_without_reads_cannot_open_one() {
    let fixture = Fixture::new("bounds");
    let owner = fixture.start();
    // A rectangle past the stage is clipped to it.
    let clipped = run(
        &owner,
        &fixture,
        TASK,
        json!({"x": 470, "y": 310, "width": 100, "height": 100}),
    );
    assert_eq!(
        clipped["result"]["result"]["rect"],
        json!([470, 310, 10, 10])
    );
    let blind = run(&owner, &fixture, BLIND, json!({}));
    assert_eq!(blind["status"], "failed", "{blind}");
    assert_eq!(blind["error"]["code"], "validation", "{blind}");
    owner.stop();

    // A stage larger than one read answers is refused before anything is read.
    let service = fixture.service();
    let mut plan = service
        .stage_plan(&fixture.asset, ReadBoundary::RepairInput)
        .unwrap();
    plan.size = Stage {
        width: 4096,
        height: 4096,
    };
    let granted = super::read::GrantedRead {
        plan,
        reads: Arc::default(),
    };
    let cancel = Cancel::never();
    let mut reader = super::read::StageReader::open(&granted, &cancel).unwrap();
    let refused = reader
        .read(Region {
            x0: 0,
            y0: 0,
            width: 2049,
            height: 2048,
        })
        .unwrap_err();
    assert_eq!(refused.kind, crate::ErrorKind::ResourceLimit, "{refused:?}");
    const { assert!(2049 * 2048 > MAX_MODULE_READ_PIXELS) };
    assert_eq!(granted.reads.open(), 1);
    drop(reader);
    assert_eq!(granted.reads.open(), 0);
}
