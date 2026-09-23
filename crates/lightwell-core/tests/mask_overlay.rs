//! The mask overlay end to end: a preview job asks for one mask's coverage grid, the worker returns
//! it with the frame it describes under that frame's generation, and the per-client view state that
//! asks for it commits nothing.
//!
//! Everything here goes through the same owner every client reaches — `catalog.import`,
//! `mask.create-linear`, `mask.add-linear`, `edit.set-basic`, `workspace.set` and `session.state` — and
//! the frames come from the real [`PreviewQueue`], so nothing is proved against a hand-built job.
//! The expected bytes are computed from [`CompiledMask`] directly, not from the unit that filled
//! the grid.
use lightwell_core::{
    ApiRequest, ApiResponse, AssetId, ClientId, ComponentId, MaskId, MaskOverlayRequest,
    OwnerHandle, PreviewPhase, PreviewQueue, PreviewRequest, PreviewResult, Recipe, Stage,
    StageTransform,
    analysis::{self, MASK_COVERAGE_FULL, MASK_COVERAGE_NONE, MAX_OVERLAY_CELLS},
    mask::CompiledMask,
    stage_transform,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread::JoinHandle,
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lightwell-mask-overlay-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

struct Fixture {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    asset: AssetId,
    asset_value: Value,
    dir: PathBuf,
}

impl Fixture {
    fn open(name: &str) -> Self {
        let dir = temp(name);
        let source = dir.join("orientation-1.jpg");
        std::fs::copy(fixture(), &source).unwrap();
        let (owner, join) = OwnerHandle::start(&dir.join("catalog.sqlite")).unwrap();
        let client = owner.register();
        let queued = ok(
            &owner,
            client,
            "import",
            "catalog.import",
            json!({"path": source}),
        );
        let job_id = queued["job_id"].as_str().expect("a job id").to_owned();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let status = ok(
                &owner,
                client,
                "status",
                "job.status",
                json!({"job_id": job_id}),
            );
            match status["state"].as_str() {
                Some("ready") => break,
                Some("queued" | "preparing") => {
                    assert!(
                        Instant::now() < deadline,
                        "the import never settled: {status}"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                other => panic!("unexpected import job {other:?}: {status}"),
            }
        }
        let asset_value = ok(
            &owner,
            client,
            "adopt",
            "job.adopt",
            json!({"job_id": job_id}),
        )["asset"]["asset"]["id"]
            .clone();
        let asset = AssetId::parse(asset_value.as_str().unwrap()).unwrap();
        Self {
            owner,
            join: Some(join),
            client,
            asset,
            asset_value,
            dir,
        }
    }

    fn call(&self, id: &str, method: &str, params: Value) -> Value {
        ok(&self.owner, self.client, id, method, params)
    }

    fn revision(&self) -> Value {
        self.call(
            "state",
            "asset.state",
            json!({"asset_id": self.asset_value}),
        )["revision"]
            .clone()
    }

    fn mutation(&self, request: &str) -> Value {
        json!({
            "expected_revision": self.revision(),
            "request_id": request,
            "actor": "mask-overlay-test",
        })
    }

    /// One `mask.*` command through the JSON method table.
    fn mask_command(&self, method: &str, mut params: Value, request: &str) -> Value {
        let object = params.as_object_mut().expect("an object of fields");
        object.insert("asset_id".into(), self.asset_value.clone());
        object.insert("mutation".into(), self.mutation(request));
        self.call(request, method, params)
    }

    fn edit(&self, action: &str, mut params: Value, request: &str) {
        let object = params.as_object_mut().expect("an object of fields");
        object.insert("asset_id".into(), self.asset_value.clone());
        object.insert("mutation".into(), self.mutation(request));
        self.call(request, &format!("edit.{action}"), params);
    }

    /// The preview job the owner plans for this client, with whatever the caller asks of it.
    fn job(&self, request: PreviewRequest) -> lightwell_core::PreviewJob {
        self.owner.preview_job(request).expect("a preview job")
    }

    fn preview(&self) -> PreviewRequest {
        PreviewRequest::new(self.client, self.asset.clone())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ok(owner: &OwnerHandle, client: ClientId, id: &str, method: &str, params: Value) -> Value {
    let response: ApiResponse = owner
        .call(
            client,
            ApiRequest {
                id: id.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("the owner answered");
    assert!(response.error.is_none(), "{id}: {:?}", response.error);
    response.result.expect("a result")
}

/// Run one job through the real queue and return its exact-phase result.
fn exact(job: lightwell_core::PreviewJob) -> PreviewResult {
    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(result) = queue.poll()
            && result.phase == PreviewPhase::Exact
        {
            assert_eq!(result.generation, generation);
            return result;
        }
        assert!(Instant::now() < deadline, "the exact phase never arrived");
        std::thread::yield_now();
    }
}

/// The cell arithmetic, transcribed here rather than shared with the unit under test: the pixel at
/// the centre of one cell's own span.
fn cell_pixel(cell: u32, extent: u32, cells: u32) -> u32 {
    let pixel = (2 * u64::from(cell) + 1) * u64::from(extent) / (2 * u64::from(cells));
    (pixel as u32).min(extent - 1)
}

/// The grid this mask should have produced over this frame, computed from [`CompiledMask`] and the
/// geometry tail alone.
fn expected_grid(
    registry: &lightwell_core::ModuleRegistry,
    recipe: &Recipe,
    mask: &lightwell_core::Mask,
    source: (u32, u32),
    cells_w: u32,
    cells_h: u32,
) -> (Vec<u8>, StageTransform) {
    let transform = stage_transform(registry, source.0, source.1, recipe).expect("a tail");
    let stage = Stage {
        width: transform.content.width,
        height: transform.content.height,
    };
    let compiled = CompiledMask::new(mask, stage, &lightwell_core::path::StrokeTable::default())
        .expect("the mask compiles");
    let mut grid = Vec::with_capacity((cells_w * cells_h) as usize);
    for cy in 0..cells_h {
        let py = cell_pixel(cy, transform.output.height, cells_h);
        for cx in 0..cells_w {
            let px = cell_pixel(cx, transform.output.width, cells_w);
            let (ox, oy) = (f64::from(px) + 0.5, f64::from(py) + 0.5);
            let inverse = transform.inverse;
            let x = inverse[0] * ox + inverse[1] * oy + inverse[2];
            let y = inverse[3] * ox + inverse[4] * oy + inverse[5];
            let inside =
                x >= 0.0 && y >= 0.0 && x < f64::from(stage.width) && y < f64::from(stage.height);
            grid.push(if inside {
                analysis::quantize_coverage(compiled.coverage(x.floor() as u32, y.floor() as u32))
            } else {
                MASK_COVERAGE_NONE
            });
        }
    }
    (grid, transform)
}

fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1})
}

fn mask_id(result: &Value) -> MaskId {
    MaskId::parse(result["mask"].as_str().expect("a mask id")).unwrap()
}

// -------------------------------------------------------------------------------------------
// The grid, and the frame it describes
// -------------------------------------------------------------------------------------------

/// The returned grid's bytes are the mask's own coverage field at the cells it names, and it comes
/// back under the generation of the very frame it describes.
#[test]
fn the_returned_grid_is_the_masks_field_over_the_frame_it_arrived_with() {
    let f = Fixture::open("field");
    let created = f.mask_command("mask.create-linear", linear(0.5, 0.0, 0.5, 1.0), "create");
    let mask = mask_id(&created);
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.5}),
        "lift",
    );

    let (cells_w, cells_h) = (31, 19);
    let job = f.job(f.preview().mask_overlay(MaskOverlayRequest {
        mask: mask.clone(),
        component: None,
        cells_w,
        cells_h,
    }));
    let registry = job.registry.clone();
    let recipe = job.recipe.clone();
    let source = job.source.dimensions();
    let held = recipe
        .masks
        .iter()
        .find(|held| held.id == mask)
        .expect("the stack holds the mask")
        .clone();

    let result = exact(job);
    let generation = result.generation;
    let raster = result.result.as_ref().expect("a frame");
    let overlay = result
        .mask_overlay
        .as_ref()
        .expect("the job asked for a coverage grid");

    assert_eq!(overlay.mask, mask);
    assert_eq!(overlay.component, None);
    assert_eq!((overlay.cells_w, overlay.cells_h), (cells_w, cells_h));
    assert_eq!(overlay.coverage.len(), (cells_w * cells_h) as usize);

    let (expected, transform) = expected_grid(&registry, &recipe, &held, source, cells_w, cells_h);
    assert_eq!(
        (transform.output.width, transform.output.height),
        (raster.width, raster.height),
        "the grid's frame is the frame that came back"
    );
    assert_eq!(
        overlay.coverage, expected,
        "every cell is the quantized coverage of the content pixel it represents"
    );

    // A vertical gradient: coverage never decreases down the frame, and the two ends of the field
    // are reached at the two ends of the picture.
    let row = cells_w as usize;
    for cy in 1..cells_h as usize {
        for cx in 0..row {
            assert!(
                overlay.coverage[cy * row + cx] >= overlay.coverage[(cy - 1) * row + cx],
                "coverage falls between cell rows {} and {cy}",
                cy - 1
            );
        }
    }
    assert!(overlay.coverage[..row].iter().all(|cell| *cell < 4));
    assert!(
        overlay.coverage[(cells_h as usize - 1) * row..]
            .iter()
            .all(|cell| *cell > 250)
    );
    assert_eq!(
        analysis::quantize_coverage(1.0),
        MASK_COVERAGE_FULL,
        "the field's full end is a whole byte"
    );

    // The grid is the frame's, not a frame's: the generation is the one the raster arrived under,
    // and a second job is a second generation carrying its own grid.
    let again = exact(f.job(f.preview().mask_overlay(MaskOverlayRequest {
        mask: mask.clone(),
        component: None,
        cells_w,
        cells_h,
    })));
    assert!(
        again.generation > 0,
        "a fresh queue's first generation is its own"
    );
    assert_eq!(
        again.mask_overlay.as_ref().expect("a grid").coverage.len(),
        overlay.coverage.len()
    );
    assert_eq!(generation, result.generation);
}

/// A geometry tail moves the mask with the picture, and the grid follows: the same stored mask
/// under a quarter turn describes the turned frame, cell for cell.
#[test]
fn the_grid_follows_the_picture_through_the_geometry_tail() {
    let f = Fixture::open("tail");
    let created = f.mask_command("mask.create-linear", linear(0.5, 0.0, 0.5, 1.0), "create");
    let mask = mask_id(&created);
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.0}),
        "lift",
    );
    f.edit("transform", json!({"transform": "rotate-right"}), "turn");

    let (cells_w, cells_h) = (24, 32);
    let job = f.job(f.preview().mask_overlay(MaskOverlayRequest {
        mask: mask.clone(),
        component: None,
        cells_w,
        cells_h,
    }));
    let registry = job.registry.clone();
    let recipe = job.recipe.clone();
    let source = job.source.dimensions();
    let held = recipe
        .masks
        .iter()
        .find(|held| held.id == mask)
        .expect("the mask survived the turn")
        .clone();
    let result = exact(job);
    let raster = result.result.as_ref().expect("a frame");
    let overlay = result.mask_overlay.as_ref().expect("a grid");
    let (expected, transform) = expected_grid(&registry, &recipe, &held, source, cells_w, cells_h);
    assert_eq!(
        (transform.output.width, transform.output.height),
        (raster.width, raster.height)
    );
    assert_ne!(
        transform.forward,
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        "the turn really is in the tail"
    );
    assert_eq!(overlay.coverage, expected);
    // A quarter turn puts the gradient's uncovered end on one side of the frame instead of the top.
    let row = cells_w as usize;
    assert_eq!(overlay.coverage[..row], overlay.coverage[row..2 * row]);
}

/// Hovering one row of the component list asks for that component's own contribution, and gets it:
/// the component's field alone, not the composition it takes part in.
#[test]
fn one_components_grid_is_that_components_own_contribution() {
    let f = Fixture::open("component");
    let created = f.mask_command("mask.create-linear", linear(0.5, 0.0, 0.5, 1.0), "create");
    let mask = mask_id(&created);
    let mut second = linear(0.0, 0.5, 1.0, 0.5);
    second
        .as_object_mut()
        .unwrap()
        .insert("mode".into(), Value::from("subtract"));
    second
        .as_object_mut()
        .unwrap()
        .insert("mask".into(), Value::from(mask.as_str()));
    f.mask_command("mask.add-linear", second, "add-component");
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.0}),
        "lift",
    );

    let (cells_w, cells_h) = (21, 17);
    let overlay_of = |component: Option<ComponentId>| {
        let job = f.job(f.preview().mask_overlay(MaskOverlayRequest {
            mask: mask.clone(),
            component,
            cells_w,
            cells_h,
        }));
        let registry = job.registry.clone();
        let recipe = job.recipe.clone();
        let source = job.source.dimensions();
        let result = exact(job);
        let overlay = result.mask_overlay.expect("a grid");
        (overlay, registry, recipe, source)
    };

    let (composed, registry, recipe, source) = overlay_of(None);
    let held = recipe
        .masks
        .iter()
        .find(|held| held.id == mask)
        .expect("the mask")
        .clone();
    assert_eq!(held.components.len(), 2, "two components compose this mask");

    for (index, component) in held.components.iter().enumerate() {
        let (alone, ..) = overlay_of(Some(component.id.clone()));
        assert_eq!(alone.component.as_ref(), Some(&component.id));

        // The expected field is that one component, on its own, at full amount: the same thing the
        // panel's row claims to be showing.
        let mut single = held.clone();
        single.amount = lightwell_core::Mask::FULL_AMOUNT;
        single.invert = false;
        single.components = vec![lightwell_core::Component {
            mode: lightwell_core::ComponentMode::Add,
            ..component.clone()
        }];
        let (expected, _) = expected_grid(&registry, &recipe, &single, source, cells_w, cells_h);
        assert_eq!(
            alone.coverage, expected,
            "component {index} draws its own field"
        );
        assert_ne!(
            alone.coverage, composed.coverage,
            "component {index}'s own field is not the composition it joins"
        );
    }

    // A component the mask does not hold is refused by name, never answered with another row.
    let stranger = ComponentId::new();
    let error = f
        .owner
        .preview_job(f.preview().mask_overlay(MaskOverlayRequest {
            mask: mask.clone(),
            component: Some(stranger.clone()),
            cells_w,
            cells_h,
        }))
        .expect_err("a component of no mask");
    assert_eq!(error.kind, lightwell_core::ErrorKind::Validation);
    assert!(error.detail.contains(&stranger.to_string()), "{error}");
}

/// Nothing to describe is absent, never a grid of zeros: a mask at amount zero has no overlay at
/// all, and a job that asks for none has none either.
#[test]
fn a_mask_with_nothing_to_describe_has_no_grid() {
    let f = Fixture::open("absent");
    let created = f.mask_command("mask.create-linear", linear(0.5, 0.0, 0.5, 1.0), "create");
    let mask = mask_id(&created);
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.0}),
        "lift",
    );

    let request = |mask: &MaskId| MaskOverlayRequest {
        mask: mask.clone(),
        component: None,
        cells_w: 12,
        cells_h: 9,
    };

    // A job that never asked carries no grid, exactly as it carries no report.
    let plain = exact(f.job(f.preview()));
    assert!(plain.mask_overlay.is_none());
    assert!(plain.report.is_none());

    // The mask as drawn does describe something.
    let drawn = exact(f.job(f.preview().mask_overlay(request(&mask))));
    let covered = drawn.mask_overlay.expect("a grid");
    assert!(
        covered
            .coverage
            .iter()
            .any(|cell| *cell > MASK_COVERAGE_NONE)
    );

    // Silenced to zero, it describes nothing, and the answer is absence rather than zeros.
    f.mask_command(
        "mask.set-amount",
        json!({"mask": mask.as_str(), "amount": 0.0}),
        "silence",
    );
    let silent = exact(f.job(f.preview().mask_overlay(request(&mask))));
    assert!(
        silent.mask_overlay.is_none(),
        "an amount of zero is no grid at all, not a grid of zeros"
    );
    assert!(silent.result.is_ok(), "the frame still came back");
}

/// The delivered cell cap bounds the grid however large the stage is, and the request that would
/// exceed it is refused before any work starts.
#[test]
fn the_cell_cap_bounds_the_grid_on_a_stage_that_exceeds_it() {
    let f = Fixture::open("cap");
    let created = f.mask_command("mask.create-linear", linear(0.5, 0.0, 0.5, 1.0), "create");
    let mask = mask_id(&created);

    for (cells_w, cells_h) in [(MAX_OVERLAY_CELLS + 1, 8), (8, MAX_OVERLAY_CELLS + 1)] {
        let error = f
            .owner
            .preview_job(f.preview().mask_overlay(MaskOverlayRequest {
                mask: mask.clone(),
                component: None,
                cells_w,
                cells_h,
            }))
            .expect_err("a grid past the cell cap");
        assert_eq!(error.kind, lightwell_core::ErrorKind::ResourceLimit);
        assert!(
            error.detail.contains(&MAX_OVERLAY_CELLS.to_string()),
            "the refusal names the bound: {error}"
        );
    }

    // At a 100% zoom over a stage far larger than the cap, the grid is still the cap's size and not
    // the stage's: a 16384 px side asks for one cell per pixel and gets the bound instead.
    let oversized = Stage {
        width: 16384,
        height: 12288,
    };
    let identity = StageTransform {
        content: lightwell_core::StageSize {
            width: oversized.width,
            height: oversized.height,
        },
        output: lightwell_core::StageSize {
            width: oversized.width,
            height: oversized.height,
        },
        forward: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
    };
    let state = f.call("state", "asset.state", json!({"asset_id": f.asset_value}));
    let _ = state;
    let job = f.job(f.preview());
    let held = job
        .recipe
        .masks
        .iter()
        .find(|held| held.id == mask)
        .expect("the mask")
        .clone();
    let compiled = CompiledMask::new(
        &held,
        oversized,
        &lightwell_core::path::StrokeTable::default(),
    )
    .expect("the mask compiles at any stage");
    assert_eq!(
        analysis::coverage_grid(
            &compiled,
            &identity,
            oversized.width,
            8,
            &Default::default()
        )
        .expect_err("one cell per pixel is past the cap")
        .kind,
        lightwell_core::ErrorKind::ResourceLimit
    );
    let grid = analysis::coverage_grid(
        &compiled,
        &identity,
        MAX_OVERLAY_CELLS,
        8,
        &Default::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(grid.len(), MAX_OVERLAY_CELLS as usize * 8);
    assert!(
        grid.len() < (oversized.width as usize) * (oversized.height as usize) / 1000,
        "the grid is display-sized, not stage-sized"
    );
}

// -------------------------------------------------------------------------------------------
// The view state
// -------------------------------------------------------------------------------------------

/// `workspace.set` takes the overlay mode and its colour, `session.state` reports them, and neither
/// touches a recipe, a revision or the histogram population. The stack is compared before and
/// after rather than inspected.
#[test]
fn the_overlay_view_state_round_trips_and_commits_nothing() {
    let f = Fixture::open("view-state");
    let created = f.mask_command("mask.create-linear", linear(0.5, 0.0, 0.5, 1.0), "create");
    let mask = mask_id(&created);
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.0}),
        "lift",
    );

    // A fresh session is off, in the default tint.
    let state = f.call("state", "session.state", json!({}));
    assert_eq!(state["workspace"]["mask_overlay"], json!("off"));
    assert_eq!(state["workspace"]["mask_overlay_colour"], json!("green"));

    let before_revision = f.revision();
    let before_recipe = f.call(
        "describe",
        "recipe.describe",
        json!({"asset_id": f.asset_value}),
    );
    let before_history = f.call(
        "history",
        "history.list",
        json!({"asset_id": f.asset_value}),
    );
    let before_masks = f.call("masks", "mask.list", json!({"asset_id": f.asset_value}));
    let before_stack = f.job(f.preview()).recipe.clone();
    let before_report = exact(f.job(f.preview().analyse()))
        .report
        .expect("the reduction of the current frame");

    for mode in ["tint", "mask-on-black", "image-on-black", "off"] {
        let set = f.call("set", "workspace.set", json!({"mask_overlay": mode}));
        assert_eq!(set["workspace"]["mask_overlay"], json!(mode));
    }
    let set = f.call(
        "both",
        "workspace.set",
        json!({"mask_overlay": "tint", "mask_overlay_colour": "white"}),
    );
    assert_eq!(set["workspace"]["mask_overlay"], json!("tint"));
    assert_eq!(set["workspace"]["mask_overlay_colour"], json!("white"));
    assert_eq!(
        set["workspace"]["clip_shadows"],
        json!(false),
        "nothing else moved"
    );
    assert_eq!(
        f.call("read", "session.state", json!({}))["workspace"],
        set["workspace"],
        "session.state reports what workspace.set stored"
    );

    // Nothing the overlay touched is edit state.
    assert_eq!(f.revision(), before_revision, "the revision did not move");
    assert_eq!(
        f.call(
            "describe",
            "recipe.describe",
            json!({"asset_id": f.asset_value})
        ),
        before_recipe,
        "the recipe is unchanged"
    );
    assert_eq!(
        f.call(
            "history",
            "history.list",
            json!({"asset_id": f.asset_value})
        ),
        before_history,
        "no history entry was appended"
    );
    assert_eq!(
        f.call("masks", "mask.list", json!({"asset_id": f.asset_value})),
        before_masks,
        "the mask table is unchanged"
    );
    assert_eq!(
        f.job(f.preview()).recipe,
        before_stack,
        "the stack a preview renders is the same stack"
    );
    assert_eq!(
        exact(f.job(f.preview().analyse()))
            .report
            .expect("a reduction"),
        before_report,
        "the histogram population is the same, counter for counter"
    );

    // Another client's overlay state is its own, and an unknown value is refused with the
    // vocabulary spelled out.
    let other = f.owner.register();
    assert_eq!(
        ok(&f.owner, other, "other", "session.state", json!({}))["workspace"]["mask_overlay"],
        json!("off")
    );
    let refused = f
        .owner
        .call(
            f.client,
            ApiRequest {
                id: "bad".into(),
                method: "workspace.set".into(),
                params: json!({"mask_overlay": "sky"}),
                token: None,
            },
        )
        .unwrap()
        .error
        .expect("an unknown overlay mode is refused");
    assert_eq!(refused.code, "validation");
    assert!(refused.message.contains("mask-on-black"), "{refused:?}");
    let refused = f
        .owner
        .call(
            f.client,
            ApiRequest {
                id: "bad-colour".into(),
                method: "workspace.set".into(),
                params: json!({"mask_overlay_colour": "red"}),
                token: None,
            },
        )
        .unwrap()
        .error
        .expect("an unknown tint is refused");
    assert_eq!(refused.code, "validation");
    assert!(refused.message.contains("green"), "{refused:?}");
}

/// The overlay is discoverable: `schema.list` publishes both fields, so a JSON client needs no
/// hand-written list and no GUI.
#[test]
fn the_overlay_fields_are_discoverable_through_schema_list() {
    let f = Fixture::open("schema");
    let schema = f.call("schema", "schema.list", json!({}));
    let workspace = &schema["methods"]["workspace.set"]["optional"];
    let mode = workspace["mask_overlay"].as_str().expect("a description");
    for spelling in ["off", "tint", "mask-on-black", "image-on-black"] {
        assert!(mode.contains(spelling), "{mode} is missing {spelling}");
    }
    let colour = workspace["mask_overlay_colour"]
        .as_str()
        .expect("a description");
    assert!(
        colour.contains("green") && colour.contains("white"),
        "{colour}"
    );
    assert_eq!(
        schema["methods"]["workspace.set"]["mutates"],
        json!(false),
        "a view change mutates nothing"
    );
}
