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

/// The pixel value a geometric component is handed and ignores (proposal P12 of
/// `docs/design/range-study.md`). These masks hold gradients and brushes, whose coverage is a
/// function of position alone, so the value here is arbitrary and the same at every call;
/// `mask_range.rs` proves that ignoring it is exact rather than approximate.
const ANY_PIXEL: [f64; 3] = [0.25, 0.5, 0.75];

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
            json!({"path": source, "mutation": {"request_id": format!("import-{}", uuid::Uuid::new_v4().simple()), "actor": "test"}}),
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
            match status["status"].as_str() {
                Some("ready") => break,
                Some("queued" | "running") => {
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
            && result.phase() == PreviewPhase::Exact
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
                analysis::quantize_coverage(compiled.coverage(
                    x.floor() as u32,
                    y.floor() as u32,
                    ANY_PIXEL,
                ))
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
    let raster = result.raster().expect("a frame");
    let overlay = result
        .exact()
        .and_then(|exact| exact.mask_overlay.grid.as_ref())
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
        again
            .exact()
            .and_then(|exact| exact.mask_overlay.grid.as_ref())
            .expect("a grid")
            .coverage
            .len(),
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
    let raster = result.raster().expect("a frame");
    let overlay = result
        .exact()
        .and_then(|exact| exact.mask_overlay.grid.as_ref())
        .expect("a grid");
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
        let overlay = result
            .exact()
            .and_then(|exact| exact.mask_overlay.grid.clone())
            .expect("a grid");
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
    assert!(
        plain
            .exact()
            .and_then(|exact| exact.mask_overlay.grid.clone())
            .is_none()
    );
    assert!(
        plain
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_none()
    );

    // The mask as drawn does describe something.
    let drawn = exact(f.job(f.preview().mask_overlay(request(&mask))));
    let covered = drawn
        .exact()
        .and_then(|exact| exact.mask_overlay.grid.clone())
        .expect("a grid");
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
        silent
            .exact()
            .and_then(|exact| exact.mask_overlay.grid.clone())
            .is_none(),
        "an amount of zero is no grid at all, not a grid of zeros"
    );
    assert!(silent.raster().is_ok(), "the frame still came back");
    // Nothing to describe is an ordinary absence and carries no reason: there is nothing to tell a
    // client that a frame of the photograph does not already say.
    assert!(
        silent
            .exact()
            .and_then(|exact| exact.mask_overlay.absent.clone())
            .is_none(),
        "{:?}",
        silent
            .exact()
            .and_then(|exact| exact.mask_overlay.absent.clone())
    );
    assert!(
        plain
            .exact()
            .and_then(|exact| exact.mask_overlay.absent.clone())
            .is_none()
    );
}

/// A mask that reads pixels and that **no layer is bound to** has no coverage grid, and the frame
/// says so in the host's own words rather than arriving with a silent absence.
///
/// A value-based component's coverage is a function of the pixel the masked operation *receives*, and
/// with no layer bound there is no operation to be the input of: a pixel read anywhere else would be
/// in a different domain from the one the selection is evaluated in, which is the same rule
/// `mask.sample-input` and the constrained brush's seed are refused by. So the grid is refused, the
/// reason names both halves — that the coverage depends on the pixel it reads and that no layer is
/// bound — and it says where such a selection *can* be read. What is asserted here is that the reason
/// reaches the client with the frame: a client that asked for an overlay and waits for its texture has
/// nothing else to stop waiting on.
#[test]
fn a_value_based_mask_no_layer_is_bound_to_says_why_it_has_no_grid() {
    let f = Fixture::open("reads-pixels");
    let created = f.mask_command("mask.create-linear", linear(0.5, 0.0, 0.5, 1.0), "create");
    let mask = mask_id(&created);
    let request = MaskOverlayRequest {
        mask: mask.clone(),
        component: None,
        cells_w: 12,
        cells_h: 9,
    };

    // The gradient alone has a grid, bound or not, because its coverage is position alone: the
    // difference below is the range component and nothing else.
    let geometric = exact(f.job(f.preview().mask_overlay(request.clone())));
    assert!(
        geometric
            .exact()
            .and_then(|exact| exact.mask_overlay.grid.clone())
            .is_some()
    );
    assert!(
        geometric
            .exact()
            .and_then(|exact| exact.mask_overlay.absent.clone())
            .is_none()
    );

    f.mask_command(
        "mask.add-luminance-range",
        json!({"mask": mask.as_str(), "mode": "intersect", "low": 20.0, "low_feather": 5.0,
               "high": 80.0, "high_feather": 5.0}),
        "add-range",
    );
    let reading = exact(f.job(f.preview().mask_overlay(request.clone())));
    assert!(
        reading.raster().is_ok(),
        "the frame itself still renders: only the overlay is refused"
    );
    assert!(
        reading
            .exact()
            .is_some_and(|exact| exact.mask_overlay.grid.is_none())
    );
    let reason = reading
        .exact()
        .and_then(|exact| exact.mask_overlay.absent.clone())
        .expect("the host's own reason travels with the frame");
    assert!(
        reason.contains("depends on the pixel it reads")
            && reason.contains("no layer is bound to mask")
            && reason.contains("100%"),
        "{reason}"
    );

    // Bind an adjustment through the mask and the same request has a grid: the refusal was the
    // missing operation and not the component's kind.
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.0}),
        "lift",
    );
    let bound = exact(f.job(f.preview().mask_overlay(request)));
    assert!(
        bound
            .exact()
            .and_then(|exact| exact.mask_overlay.grid.clone())
            .is_some(),
        "a bound value-based mask has a grid: {:?}",
        bound
            .exact()
            .and_then(|exact| exact.mask_overlay.absent.clone())
    );
    assert!(
        bound
            .exact()
            .and_then(|exact| exact.mask_overlay.absent.clone())
            .is_none()
    );
}

/// Every cell of a value-based mask's grid is the mask's own field at the pixel **the masked
/// operation receives** — the pixel `mask.sample-input` answers at the same coordinate, which is the
/// pixel the constrained brush's seed is read from.
///
/// That is the shared-rule proof: the overlay resolves the layer through
/// `mask::commands::input_layer_index` and reads its input through the same prefix evaluation the
/// host's own read-only command does, so the two cannot disagree about which pixel a mask reads. The
/// expected byte is composed here from the host's answer and [`CompiledMask`], not from the unit that
/// filled the grid.
#[test]
fn a_value_based_grid_is_read_on_the_pixel_mask_sample_input_answers() {
    let f = Fixture::open("value-based");
    let created = f.mask_command(
        "mask.create-luminance-range",
        json!({"low": 25.0, "low_feather": 15.0, "high": 80.0, "high_feather": 15.0}),
        "create",
    );
    let mask = mask_id(&created);
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.0}),
        "lift",
    );

    let (cells_w, cells_h) = (13, 9);
    let job = f.job(f.preview().mask_overlay(MaskOverlayRequest {
        mask: mask.clone(),
        component: None,
        cells_w,
        cells_h,
    }));
    let registry = job.registry.clone();
    let recipe = job.recipe.clone();
    let (width, height) = job.source.dimensions();
    let held = recipe
        .masks
        .iter()
        .find(|held| held.id == mask)
        .expect("the stack holds the mask")
        .clone();
    let result = exact(job);
    let overlay = result
        .exact()
        .and_then(|exact| exact.mask_overlay.grid.as_ref())
        .expect("a bound value-based mask has a grid");

    let transform = stage_transform(&registry, width, height, &recipe).expect("a tail");
    let stage = Stage {
        width: transform.content.width,
        height: transform.content.height,
    };
    let compiled = CompiledMask::new(&held, stage, &recipe.strokes).expect("the mask compiles");
    // No geometry layer, so the frame is its content stage and a cell's own pixel is a content pixel.
    assert_eq!(
        (transform.output.width, transform.output.height),
        (stage.width, stage.height)
    );

    let mut seen_none = false;
    let mut seen_covered = false;
    for cy in 0..cells_h {
        let py = cell_pixel(cy, stage.height, cells_h);
        for cx in 0..cells_w {
            let px = cell_pixel(cx, stage.width, cells_w);
            let answered = f.call(
                &format!("sample-{cx}-{cy}"),
                "mask.sample-input",
                json!({"asset_id": f.asset_value, "mask": mask.as_str(), "x": px, "y": py}),
            );
            let pixel = [
                answered["r"].as_f64().expect("a linear red"),
                answered["g"].as_f64().expect("a linear green"),
                answered["b"].as_f64().expect("a linear blue"),
            ];
            let expected = analysis::quantize_coverage(compiled.coverage(px, py, pixel));
            assert_eq!(
                overlay.coverage[(cy * cells_w + cx) as usize],
                expected,
                "cell ({cx}, {cy}) over pixel ({px}, {py}) reads {pixel:?}"
            );
            seen_none |= expected == MASK_COVERAGE_NONE;
            seen_covered |= expected > MASK_COVERAGE_NONE;
        }
    }
    // The band selects some of this photograph and not all of it, so the equality above is over a
    // grid with both answers in it rather than a uniform one.
    assert!(
        seen_none && seen_covered,
        "the band's grid is uniform, so this proves nothing about its argument"
    );
}

/// The coverage the grid reports is the coverage the **render applies** at the same cell, measured
/// from the rendered photograph rather than asserted by inspection.
///
/// One `-1 EV` Basic layer through a band-only mask, so the render's own arithmetic at a pixel is
/// `out = in · (1 + M · (g - 1))` in linear light with `g = 0.5`, which inverts to
/// `M = (out/in - 1)/(g - 1)`. A darkening lift is chosen because it cannot clip: every cell of the
/// picture still carries the coverage that produced it. `in` is the source pixel this frame's content
/// stage holds and `out` is the byte the frame holds, so the measured coverage comes from the
/// photograph and from nothing this change wrote.
///
/// The two ends of the field are exact, because the blend's endpoints are: an uncovered cell's byte is
/// the source's own and a fully covered cell's is the full lift. The middle is compared against the
/// resolution the picture itself has — one output code at that brightness — because no measurement
/// read off an 8-bit frame can be finer than that.
#[test]
fn the_value_based_grid_is_the_coverage_the_render_applies() {
    let f = Fixture::open("measured");
    // A **mixed** mask: a vertical gradient intersected with a luminance band. The gradient puts a
    // continuous field over the frame, so the picture has coverages between the ends to measure at
    // all, and the band gates it on the pixel, so a cell's byte is still a function of what the
    // operation receives there. The fixture is flat colour fields, on which a band alone is very
    // nearly binary. The gradient runs between a quarter and three quarters of the frame, so the grid
    // holds rows at each exact end of the field as well as the ramp between them.
    let created = f.mask_command("mask.create-linear", linear(0.5, 0.25, 0.5, 0.75), "create");
    let mask = mask_id(&created);
    f.mask_command(
        "mask.add-luminance-range",
        json!({"mask": mask.as_str(), "mode": "intersect", "low": 15.0, "low_feather": 20.0,
               "high": 90.0, "high_feather": 20.0}),
        "add-range",
    );
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": -1.0}),
        "darken",
    );

    let (cells_w, cells_h) = (23, 15);
    let job = f.job(f.preview().mask_overlay(MaskOverlayRequest {
        mask: mask.clone(),
        component: None,
        cells_w,
        cells_h,
    }));
    let source = match &job.source {
        lightwell_core::PreviewSource::Jpeg(image) => image.clone(),
        other => panic!("the JPEG fixture is not a {other:?}"),
    };
    let result = exact(job);
    let raster = result.raster().expect("a frame").clone();
    let overlay = result
        .exact()
        .and_then(|exact| exact.mask_overlay.grid.as_ref())
        .expect("a grid")
        .clone();
    assert_eq!((raster.width, raster.height), (source.width, source.height));

    // The lift the layer applies, in linear light: one stop down.
    let gain = 0.5_f64;
    let mut compared = 0usize;
    let mut ends = 0usize;
    let mut worst = 0.0f64;
    for cy in 0..cells_h {
        let py = cell_pixel(cy, raster.height, cells_h);
        for cx in 0..cells_w {
            let px = cell_pixel(cx, raster.width, cells_w);
            let offset = (py as usize * raster.width as usize + px as usize) * 4;
            let reported = f64::from(overlay.coverage[(cy * cells_w + cx) as usize]) / 255.0;
            // The green channel alone: one channel is enough to measure a coverage the blend applies
            // to all three.
            let source_code = u32::from(source.rgba[offset + 1]);
            let output_code = u32::from(raster.rgba[offset + 1]);
            let input = code_to_linear(source_code);
            let output = code_to_linear(output_code);
            let full = linear_to_code(input * gain);
            if reported == 0.0 {
                // An uncovered cell is the identity, bit for bit, wherever the lift would have moved
                // the byte at all.
                if full != source_code {
                    assert_eq!(
                        output_code, source_code,
                        "cell ({cx}, {cy}) reports no coverage and the picture changed there"
                    );
                    ends += 1;
                }
                continue;
            }
            if reported == 1.0 {
                assert_eq!(
                    output_code, full,
                    "cell ({cx}, {cy}) reports full coverage and the picture is not fully lifted"
                );
                ends += 1;
                continue;
            }
            // One output code here is worth `dlinear / (in · |g - 1|)` of coverage. The tolerance is
            // two of them: the frame's own rounding, and the reported byte's.
            let code = code_to_linear(output_code + 1) - code_to_linear(output_code);
            let span = input * (1.0 - gain);
            if span <= 2.0 * code {
                // A cell so dark that the whole field spans less than the codes the frame can tell
                // apart. The picture cannot measure a coverage there, and pretending otherwise would
                // make this a test of rounding.
                continue;
            }
            let measured = (output / input - 1.0) / (gain - 1.0);
            let resolution = 2.0 * code / span;
            let difference = (measured - reported).abs();
            worst = worst.max(difference / resolution);
            assert!(
                difference <= resolution,
                "cell ({cx}, {cy}) reports {reported:.4} and the picture measures {measured:.4}, \
                 past the {resolution:.4} that one output code buys at input {input:.4}"
            );
            compared += 1;
        }
    }
    assert!(
        compared > 20 && ends > 20,
        "{compared} cells measured in the middle of the field and {ends} at its ends is not enough \
         of this grid to have proved anything"
    );
    println!(
        "{compared} cells measured against the picture and {ends} at the field's exact ends; worst \
         disagreement {worst:.3} of what one output code buys"
    );
}

/// A mask a person **painted**, with one stroke limited to a colour, has a grid too.
///
/// This is the half TASK-024 widened: a brush is a position-based kind whose *stroke* can read the
/// pixel, so before P16 was built a mask a person had painted could lose its overlay where only a
/// typed one could before. It is the same rule and the same implementation, which is why nothing here
/// is a second code path — only a second kind of component reaching it.
#[test]
fn a_painted_mask_with_a_limited_stroke_has_a_grid() {
    let f = Fixture::open("painted");
    let drawn = f.mask_command(
        "mask.add-stroke",
        json!({"points": [[0.3, 0.3], [0.7, 0.55]], "size": 0.25, "feather": 40.0,
               "flow": 100.0, "erase": false}),
        "paint",
    );
    let mask = mask_id(&drawn);
    let component = drawn["component"]
        .as_str()
        .expect("the stroke made a component")
        .to_owned();
    f.edit(
        "set-basic",
        json!({"mask": mask.as_str(), "exposure": 1.0}),
        "lift",
    );
    // A second stroke over the first, held to the colour under where it starts.
    f.mask_command(
        "mask.add-stroke",
        json!({"mask": mask.as_str(), "component": component, "points": [[0.35, 0.33], [0.6, 0.5]],
               "size": 0.2, "feather": 40.0, "flow": 100.0, "erase": false,
               "limit_to_colour": true, "colour_refine": 50.0}),
        "limit",
    );

    let (cells_w, cells_h) = (17, 11);
    let job = f.job(f.preview().mask_overlay(MaskOverlayRequest {
        mask: mask.clone(),
        component: None,
        cells_w,
        cells_h,
    }));
    let registry = job.registry.clone();
    let recipe = job.recipe.clone();
    let (width, height) = job.source.dimensions();
    let held = recipe
        .masks
        .iter()
        .find(|held| held.id == mask)
        .expect("the stack holds the mask")
        .clone();
    let compiled = CompiledMask::new(&held, Stage { width, height }, &recipe.strokes)
        .expect("the painted mask compiles");
    assert!(
        compiled.reads_pixels(),
        "a stroke limited to a colour makes the mask read pixels"
    );
    let result = exact(job);
    let overlay = result
        .exact()
        .and_then(|exact| exact.mask_overlay.grid.as_ref())
        .expect("a painted mask a person can see");
    assert!(
        result
            .exact()
            .and_then(|exact| exact.mask_overlay.absent.clone())
            .is_none()
    );
    assert!(
        overlay
            .coverage
            .iter()
            .any(|cell| *cell > MASK_COVERAGE_NONE),
        "the painted mask's grid is all zeros"
    );

    // And it is the same field, read on the same input the host's own command answers.
    let transform = stage_transform(&registry, width, height, &recipe).expect("a tail");
    for cy in 0..cells_h {
        let py = cell_pixel(cy, transform.output.height, cells_h);
        for cx in 0..cells_w {
            let px = cell_pixel(cx, transform.output.width, cells_w);
            let answered = f.call(
                &format!("painted-sample-{cx}-{cy}"),
                "mask.sample-input",
                json!({"asset_id": f.asset_value, "mask": mask.as_str(), "x": px, "y": py}),
            );
            let pixel = [
                answered["r"].as_f64().unwrap(),
                answered["g"].as_f64().unwrap(),
                answered["b"].as_f64().unwrap(),
            ];
            assert_eq!(
                overlay.coverage[(cy * cells_w + cx) as usize],
                analysis::quantize_coverage(compiled.coverage(px, py, pixel)),
                "cell ({cx}, {cy}) over pixel ({px}, {py})"
            );
        }
    }
}

fn code_to_linear(code: u32) -> f64 {
    let encoded = f64::from(code) / 255.0;
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_code(linear: f64) -> u32 {
    let clamped = linear.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.003_130_8 {
        12.92 * clamped
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (255.0 * encoded + 0.5).floor() as u32
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
            analysis::MaskPixels::Unavailable("this mask reads no pixel"),
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
        analysis::MaskPixels::Unavailable("this mask reads no pixel"),
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
        .exact()
        .and_then(|exact| exact.report.clone())
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
            .exact()
            .and_then(|exact| exact.report.clone())
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
