//! A draft's GPU preview with its preview job: the plan and its boundary planned on the catalog
//! owner, one program sequence and one boundary through a drag that leaves neutral, and the
//! boundary rendered once by the preview worker after the job's Fit frame.
use super::*;
use crate::{
    AssetId, BASIC_EFFECT, Draft, DraftStamp, EntryId, Evaluation, GpuAnswer, GpuPlan, GpuPreview,
    GpuView, HistoryEntry, Layer, ModuleRegistry, ProxyBounds, RECIPE_FORMAT, Recipe,
    RenderContext, Snapshot, SnapshotId, SourceImage,
    colour::srgb::decode_table,
    modules::{Processing, Stage},
    render::{gpu::plan_preview, tests::fitted_crop},
};
use luxforge_testbase::wait_for;
use serde_json::{Value, json};
use std::sync::Arc;

const WIDTH: u32 = 600;
const HEIGHT: u32 = 400;
const CURVE_EFFECT: &str = "luxforge.curve.tone";

/// A gradient JPEG source large enough to have a proxy at [`bounds`].
fn source() -> PreviewSource {
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            rgba.extend([
                (x * 251 / WIDTH) as u8,
                (y * 241 / HEIGHT) as u8,
                ((x * 7 + y * 3) % 256) as u8,
                255,
            ]);
        }
    }
    PreviewSource::Jpeg(SourceImage {
        width: WIDTH,
        height: HEIGHT,
        rgba: rgba.into(),
        fingerprint: "sha256:gpu-preview-fixture".into(),
        orientation: 1,
        capture: Default::default(),
    })
}

fn bounds() -> ProxyBounds {
    ProxyBounds {
        width: 160,
        height: 120,
    }
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        ..Recipe::default()
    }
}

/// A straightened crop: a resample, so the plan has a geometry tail and the proxy a window.
fn crop() -> Layer {
    Layer::crop(fitted_crop(WIDTH, HEIGHT, 4.0, [0.1, 0.12, 0.6, 0.55]))
}

fn basic(payload: Value) -> Layer {
    Layer::new(BASIC_EFFECT, payload)
}

/// The preview job of `action`'s draft at `revision`, whose effective stack is `drafted`, over an
/// entry that holds `entry`, and the draft itself.
fn draft_job(
    action: &str,
    entry: Vec<Layer>,
    drafted: Vec<Layer>,
    revision: u64,
) -> (PreviewJob, Draft) {
    draft_job_over(source(), action, entry, drafted, revision)
}

/// [`draft_job`] over `source`.
fn draft_job_over(
    source: PreviewSource,
    action: &str,
    entry: Vec<Layer>,
    drafted: Vec<Layer>,
    revision: u64,
) -> (PreviewJob, Draft) {
    draft_job_in(
        RenderContext::new(),
        source,
        action,
        entry,
        drafted,
        revision,
    )
}

/// [`draft_job_over`] in `context`, whose estimate store its frames fill and its plans read.
fn draft_job_in(
    context: RenderContext,
    source: PreviewSource,
    action: &str,
    entry: Vec<Layer>,
    drafted: Vec<Layer>,
    revision: u64,
) -> (PreviewJob, Draft) {
    draft_job_of(
        context,
        source,
        action,
        recipe(entry),
        recipe(drafted),
        revision,
    )
}

/// [`draft_job_in`] over whole recipes, masks and all.
fn draft_job_of(
    context: RenderContext,
    source: PreviewSource,
    action: &str,
    entry: Recipe,
    drafted: Recipe,
    revision: u64,
) -> (PreviewJob, Draft) {
    draft_job_with(
        ModuleRegistry::builtin(),
        context,
        source,
        action,
        entry,
        drafted,
        revision,
    )
}

/// [`draft_job_of`] over `registry`.
fn draft_job_with(
    registry: ModuleRegistry,
    context: RenderContext,
    source: PreviewSource,
    action: &str,
    entry: Recipe,
    drafted: Recipe,
    revision: u64,
) -> (PreviewJob, Draft) {
    let asset = AssetId::new();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence: 1,
        action_id: "crop".into(),
        label: "Crop".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: asset.clone(),
            recipe: entry,
        },
        undo_parent: None,
        restore_target: None,
    };
    let mut draft = Draft::new(action, asset, 1);
    draft.draft_revision = revision;
    let evaluation = Evaluation::new(
        Arc::new(registry),
        context,
        source,
        entry,
        drafted,
        Some(DraftStamp {
            draft_id: draft.draft_id.clone(),
            draft_revision: revision,
        }),
    );
    (PreviewJob::new(evaluation).unwrap(), draft)
}

fn planned(preview: &GpuPreview) -> &GpuPlan {
    match &preview.answer {
        GpuAnswer::Plan(plan) => plan,
        GpuAnswer::Fallback(reason) => panic!("expected a plan, got {reason}"),
    }
}

/// Every program the plan runs, in order: its program sequence, its spatial operation's passes and
/// applies after its colour units.
fn sequence(plan: &GpuPlan) -> Vec<&'static str> {
    plan.operations()
        .flat_map(|operation| operation.units.iter().map(|unit| unit.program.entry))
        .chain(plan.spatial.iter().flat_map(|spatial| {
            spatial
                .passes
                .iter()
                .map(|pass| pass.kernel)
                .chain(spatial.applies.iter().map(|apply| apply.function))
        }))
        .collect()
}

/// What decides a plan's pipelines, as the surface keys its program sequence: its colour units'
/// programs, then its spatial operation's clamp, mask, planes, passes and applies, but for every
/// word and whether an apply is the identity, which is what a drag changes.
fn pipelines(plan: &GpuPlan) -> Vec<String> {
    let mut keys: Vec<String> = plan
        .operations()
        .flat_map(|operation| operation.units.iter())
        .map(|unit| unit.program.entry.to_owned())
        .collect();
    for spatial in &plan.spatial {
        keys.push(format!(
            "{}: clamps {}, masked {}",
            spatial.program.entry,
            spatial.clamps,
            spatial.mask.is_some()
        ));
        keys.extend(spatial.planes.iter().map(|plane| format!("{plane:?}")));
        keys.extend(spatial.passes.iter().map(|pass| {
            format!(
                "{} {:?} -> {}, source {}, {:?}",
                pass.kernel, pass.inputs, pass.output, pass.source, pass.shape
            )
        }));
        keys.extend(spatial.applies.iter().map(|apply| {
            format!(
                "{} {:?}, words {}",
                apply.function, apply.planes, apply.words
            )
        }));
    }
    keys
}

/// A Basic drag from neutral — the layer absent, then holding a value, then another, then back at
/// neutral — plans one program sequence and one boundary throughout, while the CPU compile of the
/// same stacks keeps only the non-neutral unit.
#[test]
fn a_drag_from_neutral_keeps_one_program_sequence_and_one_boundary() {
    let crop = crop();
    let entry = vec![crop.clone()];
    let drafted = basic(json!({}));
    let at = |payload: Value| Layer {
        payload,
        ..drafted.clone()
    };
    let stacks = [
        vec![crop.clone()],
        vec![at(json!({"exposure": 0.3})), crop.clone()],
        vec![at(json!({"exposure": -1.2})), crop.clone()],
        vec![at(json!({})), crop.clone()],
    ];
    let previews: Vec<GpuPreview> = stacks
        .iter()
        .enumerate()
        .map(|(revision, stack)| {
            let (job, draft) =
                draft_job("set-basic", entry.clone(), stack.clone(), revision as u64);
            plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap()
        })
        .collect();
    let first = planned(&previews[0]);
    assert_eq!(
        first.boundary.layer, 0,
        "the Basic layer's first commit goes first"
    );
    assert_eq!(
        first.content[0].units.len(),
        4,
        "the drafted layer holds every unit, neutral ones as their identity"
    );
    assert!(
        first.geometry.affine().is_some() && first.geometry.clamps,
        "the straightened crop is the tail"
    );
    for preview in &previews {
        assert_eq!(sequence(planned(preview)), sequence(first));
        assert_eq!(preview.boundary, previews[0].boundary, "one boundary");
    }
    // The CPU compiles only the unit the value needs, and its frame is unchanged by the shape.
    let registry = ModuleRegistry::builtin();
    let compiled = registry
        .compile(WIDTH, HEIGHT, &recipe(stacks[1].clone()))
        .unwrap();
    let units: Vec<usize> = compiled.segments[0]
        .operations
        .iter()
        .filter_map(|operation| match operation {
            Processing::Color(colour) => Some(colour.len()),
            _ => None,
        })
        .collect();
    assert_eq!(units, [1]);
}

/// Every gesture is planned from the stack's first layer past its source layers, so its boundary
/// is the source itself: a drag of the Tone curve after a Basic layer starts from the Basic layer's
/// input, and its key is one whatever the curve or the Basic layer holds.
#[test]
fn the_boundary_key_follows_the_layers_before_it() {
    // One stack's layers keep their identities while a value changes, as a stored stack's do.
    let held = basic(json!({}));
    let curve = Layer::new(CURVE_EFFECT, json!({}));
    let curve = |y: f64| Layer {
        payload: json!({"luminance": [[0.0, 0.0], [0.5, y], [1.0, 1.0]]}),
        ..curve.clone()
    };
    let key = |exposure: f64, y: f64| {
        let before = Layer {
            payload: json!({"exposure": exposure}),
            ..held.clone()
        };
        let (job, draft) = draft_job("set-curve", vec![before.clone()], vec![before, curve(y)], 1);
        let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
        assert_eq!(
            planned(&preview).boundary.layer,
            0,
            "the stack's first layer's input"
        );
        assert!(!planned(&preview).boundary.continues_run);
        assert_eq!(
            planned(&preview).content.len(),
            2,
            "the Basic layer and the curve"
        );
        preview.boundary.expect("a boundary").key
    };
    assert_eq!(key(0.5, 0.6), key(0.5, 0.4));
    assert_eq!(key(0.5, 0.6), key(0.7, 0.6));
}

/// A draft that changes nothing yet and drafts no layer of its own has no plan, and says so.
#[test]
fn a_draft_that_changes_nothing_names_it() {
    let crop = crop();
    let (job, draft) = draft_job("crop", vec![crop.clone()], vec![crop], 1);
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    assert_eq!(
        preview.answer,
        GpuAnswer::Fallback(crate::GpuFallback::Unchanged)
    );
    assert!(preview.boundary.is_none());
    assert_eq!(preview.layer, None, "it names no layer");
}

/// A fallback that names a layer carries the label the recipe list gives it, read from the stack
/// the plan was made from, which holds the neutral layer a drafted layer's first commit would add.
/// A plan, and a reason that names no layer, carry none.
#[test]
fn a_fallback_carries_its_layers_label_from_the_stack_it_was_planned_over() {
    let pixel = Layer::pixel(0, 0, [9, 9, 9]);
    let (job, draft) = draft_job_with(
        ModuleRegistry::developer(),
        RenderContext::new(),
        source(),
        "set-basic",
        recipe(vec![pixel.clone()]),
        recipe(vec![pixel]),
        1,
    );
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    let Some(crate::GpuFallback::PixelStage { layer }) = preview.answer.fallback() else {
        panic!("a pixel-stage reason, not {:?}", preview.answer);
    };
    assert_eq!(*layer, 0, "the neutral Basic layer is inserted after it");
    assert_eq!(preview.layer.as_deref(), Some("Pixel"));
    let (job, draft) = draft_job(
        "set-basic",
        vec![basic(json!({}))],
        vec![basic(json!({"exposure": 0.3}))],
        1,
    );
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    assert!(preview.answer.plan().is_some());
    assert_eq!(preview.layer, None, "a plan names no layer");
}

/// At Fit a drag's boundary is the source reduced to the job's proxy: the key names the proxy plan
/// the worker's proxy phase builds at the job's bounds, with the window of it the crop reads, and
/// the plan addresses that whole proxy stage; the photo surface derives the boundary from the
/// source it holds, so the job asks the worker for nothing more than its frame.
#[test]
fn a_fit_boundary_is_the_source_reduced_to_the_jobs_proxy() {
    let (job, draft) = draft_job(
        "set-basic",
        vec![crop()],
        vec![basic(json!({"exposure": 0.3})), crop()],
        2,
    );
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    let request = preview.boundary.clone().expect("a boundary");
    assert_eq!(request.format, crate::BoundaryFormat::Half, "a JPEG's");
    assert_eq!(request.window, None, "the proxy plan names its window");
    let plan = request.key.plan().expect("a proxy");
    let exact = job.evaluation.exact(&crate::Cancel::never()).unwrap();
    let worker = exact.proxy_plan(bounds()).expect("a proxy at the bounds");
    let worker = exact
        .proxy_window(job.evaluation.registry(), job.evaluation.recipe(), worker)
        .plan();
    assert_eq!(plan, worker, "the worker's proxy, its window included");
    let window = plan.window.expect("the crop's window");
    assert_eq!(
        plan.held(),
        [window.x, window.y, window.width, window.height]
    );
    assert_eq!(
        planned(&preview).boundary.stage,
        Stage {
            width: plan.width,
            height: plan.height
        },
        "the whole proxy stage is addressed"
    );
    assert_eq!(planned(&preview).boundary.layer, 0, "the source");
}

/// Below 100% the desktop's job carries the displayed size of the whole stage as its bounds, and a
/// draft's boundary planned at them is the source reduced to that proxy, as Fit's is: its whole
/// stage, the size of the proxy frame the CPU path draws at that zoom.
#[test]
fn below_100_percent_a_boundary_is_the_source_reduced_to_the_displayed_size() {
    // 50% and 33% of the 600 × 400 source, as the desktop rounds a displayed size.
    for bounds in [
        ProxyBounds {
            width: 300,
            height: 200,
        },
        ProxyBounds {
            width: 198,
            height: 132,
        },
    ] {
        let (mut job, draft) = draft_job(
            "set-basic",
            Vec::new(),
            vec![basic(json!({"exposure": 0.3}))],
            2,
        );
        let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds)).unwrap();
        let request = preview.boundary.clone().expect("a boundary");
        let plan = request.key.plan().expect("a proxy");
        assert_eq!((plan.width, plan.height), (bounds.width, bounds.height));
        assert_eq!(
            plan.held(),
            [0, 0, plan.width, plan.height],
            "the whole stage"
        );
        // The CPU's proxy frame at the zoom is the size the surface's reduction of the source is.
        job.proxy = Some(bounds);
        job.intent = PreviewIntent::Interactive;
        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let proxy = wait_for("the proxy frame", || queue.poll());
        assert_eq!(
            (proxy.generation, proxy.phase()),
            (generation, PreviewPhase::Proxy)
        );
        let raster = &proxy.proxy().expect("the proxy phase").raster;
        assert_eq!(
            (raster.width, raster.height),
            (bounds.width, bounds.height),
            "the frame the view draws: the whole stage at its displayed size"
        );
        luxforge_testbase::wait_until("the job to end", || !queue.is_busy());
    }
}

/// A developed RAW's boundary, on the linear path, holds `f32` texels, as the plan of the linear
/// path reads them: the surface derives it from the RAW's planes as the `f32` they are.
#[test]
fn a_raw_boundary_holds_its_values_as_f32() {
    let raw = PreviewSource::Raw {
        image: crate::render::tests::varied(WIDTH, HEIGHT),
        settings: crate::LinearSettings::default(),
    };
    let (job, draft) = draft_job_over(
        raw,
        "set-basic",
        Vec::new(),
        vec![basic(json!({"exposure": 0.3}))],
        2,
    );
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    let request = preview.boundary.clone().expect("a boundary");
    assert_eq!(request.format, crate::BoundaryFormat::Float);
    assert!(planned(&preview).linear, "the linear path");
    assert!(request.key.plan().is_some(), "a proxy");
}

/// A tight crop whose output fits the display bounds is drawn at Fit at its exact stage, and its
/// boundary holds only the window of the source its output reads: through a straightening, a
/// perspective warp fused with it and a crop that runs to the source's edge, the planner's
/// window, which the worker renders after the exact Fit frame as the source's own texels there.
#[test]
fn an_exact_fit_boundary_holds_only_the_window_its_output_reads() {
    let perspective = Layer::new(
        crate::PERSPECTIVE_EFFECT,
        json!({"horizontal": 20, "vertical": -10}),
    );
    let tight = |rect: [f64; 4]| Layer::crop(fitted_crop(WIDTH, HEIGHT, 7.0, rect));
    let cases = [
        ("straightened", vec![tight([0.4, 0.4, 0.2, 0.2])], false),
        (
            "perspective",
            vec![perspective.clone(), tight([0.4, 0.4, 0.2, 0.2])],
            false,
        ),
        ("at the corner", vec![tight([0.0, 0.0, 0.22, 0.22])], true),
    ];
    let table = decode_table();
    let held = |value: f32| half::f16::from_f32(value).to_f32();
    let PreviewSource::Jpeg(image) = source() else {
        panic!("a JPEG")
    };
    for (name, geometry, edge) in cases {
        let mut drafted = vec![basic(json!({"exposure": 0.3}))];
        drafted.extend(geometry.iter().cloned());
        let (job, draft) = draft_job("set-basic", geometry, drafted, 2);
        let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
        let output = planned(&preview).geometry.output();
        assert!(
            output.width <= bounds().width && output.height <= bounds().height,
            "{name}: the output fits the display"
        );
        let request = preview.boundary.clone().expect("a boundary");
        assert_eq!(request.key.plan(), None, "{name}: drawn at the exact stage");
        let window = request.window.expect("the window the crop reads");
        assert!(
            window.pixels() < u64::from(WIDTH * HEIGHT) / 4,
            "{name}: a {window:?} window of the {WIDTH}x{HEIGHT} source"
        );
        if edge {
            assert!(
                window.x0 == 0 || window.y0 == 0,
                "{name}: {window:?} reaches the source's edge"
            );
        }
        // The CPU's reference render of the boundary over the whole output holds the window the
        // plan names, which the surface cuts from the source it holds: the source's own texels.
        let exact = job.evaluation.exact(&crate::Cancel::never()).unwrap();
        let frame = exact
            .output_boundary((0, 0), request.format)
            .expect("the CPU's reference");
        assert_eq!(frame.origin, (window.x0, window.y0), "{name}");
        assert_eq!((frame.width, frame.height), (window.width, window.height));
        assert_eq!(
            frame.stage,
            Stage {
                width: WIDTH,
                height: HEIGHT
            }
        );
        for y in 0..frame.height {
            for x in 0..frame.width {
                let at = (((y + window.y0) * WIDTH + x + window.x0) * 4) as usize;
                assert_eq!(
                    frame.texel(x, y).unwrap(),
                    [0, 1, 2].map(|channel| held(table[usize::from(image.rgba[at + channel])])),
                    "{name}: texel ({x}, {y})"
                );
            }
        }
    }
}

/// What planning the exact-stage window adds to a tick's plan on the catalog owner, which plans it
/// in the draft's own answer: `plan_preview` of a Basic drag at Fit over a photograph drawn at its
/// exact stage, whole and under a straightened crop, against the window planning alone over the
/// same compilation. Wall-clock time of the calling thread, the median and the 95th percentile of
/// many calls; a measurement, not a gate.
#[test]
#[ignore = "a timing measurement: cargo test -p luxforge-core --lib exact_fit_window_planning -- --ignored --nocapture"]
fn the_exact_fit_window_planning_cost_is_measured() {
    let PreviewSource::Jpeg(image) = source() else {
        panic!("a JPEG")
    };
    let window = crate::modules::Region {
        x0: 0,
        y0: 0,
        width: 480,
        height: 320,
    };
    let small = PreviewSource::Jpeg(image.window(window, &crate::Cancel::never()).unwrap());
    let tight = Layer::crop(fitted_crop(480, 320, 7.0, [0.3, 0.3, 0.35, 0.35]));
    for (name, geometry) in [("whole", Vec::new()), ("straightened crop", vec![tight])] {
        let mut drafted = vec![basic(json!({"exposure": 0.3}))];
        drafted.extend(geometry.iter().cloned());
        let (job, draft) = draft_job_over(small.clone(), "set-basic", geometry, drafted, 2);
        // The evidence window's Fit bounds, which the 480 × 320 photograph fits.
        let view = crate::GpuView::Fit(ProxyBounds {
            width: 1716,
            height: 1508,
        });
        let request = plan_preview(&job.evaluation, &draft, view)
            .unwrap()
            .boundary
            .expect("a boundary");
        assert_eq!(request.key.plan(), None, "{name}: drawn at the exact stage");
        let plans: Vec<f64> = (0..2000)
            .map(|_| {
                let started = std::time::Instant::now();
                std::hint::black_box(plan_preview(&job.evaluation, &draft, view).unwrap());
                started.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        let compiled = job.evaluation.compiled().unwrap();
        let stage = Stage {
            width: 480,
            height: 320,
        };
        let windows: Vec<f64> = (0..20000)
            .map(|_| {
                let started = std::time::Instant::now();
                std::hint::black_box(crate::render::gpu::output_window(compiled, stage, 0));
                started.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        let plans = luxforge_testbase::Distribution::of(plans).expect("plans");
        let windows = luxforge_testbase::Distribution::of(windows).expect("windows");
        eprintln!(
            "exact Fit, {name}: plan_preview p50 {:.1} µs, p95 {:.1} µs; the window planning in \
             it p50 {:.2} µs, p95 {:.2} µs; window {:?}",
            plans.p50, plans.p95, windows.p50, windows.p95, request.window
        );
    }
}

/// At the exact stage at Fit a stack with no crop, whose output reads every pixel, keeps the whole
/// stage; a drag under Dehaze behind a tight crop holds only the window the crop reads, since the
/// light is its light link's, computed from the whole stage whatever window the boundary holds.
#[test]
fn an_exact_fit_boundary_keeps_the_whole_stage_only_where_its_output_reads_it() {
    let tight = Layer::crop(fitted_crop(WIDTH, HEIGHT, 7.0, [0.4, 0.4, 0.2, 0.2]));
    let dehaze = Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40.0}));
    let small = || {
        let PreviewSource::Jpeg(image) = source() else {
            panic!("a JPEG")
        };
        // A photograph that fits the display bounds whole.
        let window = crate::modules::Region {
            x0: 0,
            y0: 0,
            width: 150,
            height: 100,
        };
        PreviewSource::Jpeg(image.window(window, &crate::Cancel::never()).unwrap())
    };
    for (name, source, entry, drafted, whole) in [
        (
            "under Dehaze",
            source(),
            vec![dehaze.clone(), tight.clone()],
            vec![basic(json!({"exposure": 0.3})), dehaze, tight],
            false,
        ),
        (
            "no crop",
            small(),
            Vec::new(),
            vec![basic(json!({"exposure": 0.3}))],
            true,
        ),
    ] {
        let (job, draft) = draft_job_over(source, "set-basic", entry, drafted, 2);
        let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
        let request = preview.boundary.clone().expect("a boundary");
        assert_eq!(request.key.plan(), None, "{name}: drawn at the exact stage");
        match (whole, request.window) {
            (true, None) => {}
            (false, Some(window)) => {
                assert!(
                    window.width < WIDTH && window.height < HEIGHT,
                    "{name}: the window the crop reads, {window:?}"
                );
                let [light] = &planned(&preview).lights[..] else {
                    panic!("{name}: one light");
                };
                assert_eq!(
                    light.stage,
                    Stage {
                        width: WIDTH,
                        height: HEIGHT
                    },
                    "{name}: the whole stage's light"
                );
            }
            (_, window) => panic!("{name}: {window:?}"),
        }
    }
}

/// The plans warmed for a committed stack hold the program sequence every first drag of a colour
/// or finish module draws, each once: a Basic drag on a stack without Basic draws exactly a warmed
/// sequence.
#[test]
fn the_warmed_plans_hold_every_first_drags_sequence() {
    let crop = crop();
    let (job, _) = draft_job("set-basic", vec![crop.clone()], vec![crop.clone()], 0);
    let plans =
        crate::render::gpu::plan_warm(&job.evaluation, crate::GpuView::Fit(bounds())).unwrap();
    let warmed: Vec<Vec<&'static str>> = plans.iter().map(sequence).collect();
    let keys: Vec<Vec<String>> = plans.iter().map(pipelines).collect();
    for (index, one) in keys.iter().enumerate() {
        assert!(!keys[..index].contains(one), "each sequence once");
    }
    let (job, draft) = draft_job(
        "set-basic",
        vec![crop.clone()],
        vec![basic(json!({"exposure": 0.6})), crop],
        1,
    );
    let drag = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    assert!(
        warmed.contains(&sequence(planned(&drag))),
        "the Basic drag's sequence {:?} is warmed among {warmed:?}",
        sequence(planned(&drag))
    );
    for entry in [
        "lf_curve_tone_curve",
        "lf_mixer_mixer",
        "lf_vignette_vignette",
    ] {
        assert!(
            warmed.iter().any(|one| one.contains(&entry)),
            "{entry} is warmed"
        );
    }
}

/// The warm list keys a plan as the surface keys its sequence, masks included: a Presence drag
/// through a mask runs the programs of the same drag unmasked but compiles to another sequence, so
/// a list holding both keeps both; and a Presence layer through a mask is warmed with its mask.
#[test]
fn the_warmed_plans_tell_a_masked_layer_from_an_unmasked_one() {
    let mut mask = crate::Mask::new("Mask 1");
    mask.components.push(crate::Component::new(
        "Radial 1",
        crate::ComponentMode::Add,
        "radial",
        json!({"x": 0.45, "y": 0.55, "radius_x": 0.3, "radius_y": 0.22, "angle": 18.0,
               "feather": 45.0}),
    ));
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 20}));
    let stack = |masked: bool| Recipe {
        masks: vec![mask.clone()],
        ..recipe(vec![Layer {
            mask: masked.then(|| mask.id.clone()),
            ..presence.clone()
        }])
    };
    let warm = |masked: bool| {
        let (job, _) = draft_job_of(
            RenderContext::new(),
            source(),
            "set-presence",
            stack(masked),
            stack(masked),
            0,
        );
        let plans =
            crate::render::gpu::plan_warm(&job.evaluation, crate::GpuView::Fit(bounds())).unwrap();
        // The Presence layer's own drag is the last plan: the committed stack and the first
        // drags of the colour and finish modules come before it.
        plans
            .into_iter()
            .rev()
            .find(|plan| {
                plan.content.is_empty() && !plan.spatial.is_empty() && plan.output.is_empty()
            })
            .expect("the Presence layer's drag")
    };
    let (unmasked, masked) = (warm(false), warm(true));
    assert!(
        masked.spatial.first().unwrap().mask.is_some(),
        "warmed with its mask"
    );
    assert_eq!(sequence(&masked), sequence(&unmasked), "the same programs");
    assert_ne!(
        crate::render::gpu::warm_sequence(&masked),
        crate::render::gpu::warm_sequence(&unmasked),
        "two sequences"
    );
}

/// At a percentage zoom the boundary is the window of the source the visible region reads, at full
/// scale: keyed by that region, and the window the plan names is the one the CPU's reference render
/// of the region's boundary holds, its texels the source's there exactly. Behind a straightened
/// crop the window is the crop's read of the source.
#[test]
fn a_region_boundary_holds_the_window_its_region_reads_at_full_scale() {
    let rect = crate::modules::Region {
        x0: 40,
        y0: 30,
        width: 120,
        height: 90,
    };
    for (what, entry, drafted, alone) in [
        (
            "a Basic layer alone",
            Vec::new(),
            vec![basic(json!({"exposure": 0.3}))],
            true,
        ),
        (
            "a Basic layer before a straightened crop",
            vec![crop()],
            vec![basic(json!({"exposure": 0.3})), crop()],
            false,
        ),
    ] {
        let (mut job, draft) = draft_job("set-basic", entry, drafted, 2);
        let view = crate::GpuView::Region {
            rect,
            magnification: 2.0,
        };
        let preview = plan_preview(&job.evaluation, &draft, view).unwrap();
        let request = preview.boundary.clone().expect("a boundary");
        assert_eq!(request.key.region(), Some(rect), "{what}");
        assert_eq!(request.key.plan(), None, "{what}: the exact stage");
        // Another region is another key.
        let moved = crate::GpuView::Region {
            rect: crate::modules::Region { x0: 41, ..rect },
            magnification: 2.0,
        };
        let other = plan_preview(&job.evaluation, &draft, moved).unwrap();
        assert_ne!(other.boundary.unwrap().key, request.key, "{what}");
        job.viewport = Some(rect);
        let exact = job.evaluation.exact(&crate::Cancel::never()).unwrap();
        let frame = exact
            .region_boundary(rect, (0, 0), request.format)
            .expect("the CPU's reference");
        assert_eq!(frame.format, crate::BoundaryFormat::Half);
        // The window the request named when it was planned is the one the boundary holds.
        assert_eq!(
            request.window,
            Some(crate::modules::Region {
                x0: frame.origin.0,
                y0: frame.origin.1,
                width: frame.width,
                height: frame.height,
            }),
            "{what}"
        );
        assert_eq!(
            frame.stage,
            Stage {
                width: WIDTH,
                height: HEIGHT
            },
            "{what}: the source stage the Basic layer receives"
        );
        if alone {
            assert_eq!(
                frame.origin,
                (rect.x0, rect.y0),
                "{what}: the region itself"
            );
            assert_eq!((frame.width, frame.height), (rect.width, rect.height));
        } else {
            assert!(
                frame.width < WIDTH && frame.height < HEIGHT,
                "{what}: a window of the source, {}x{}",
                frame.width,
                frame.height
            );
        }
        let PreviewSource::Jpeg(image) = source() else {
            unreachable!()
        };
        let table = decode_table();
        let held = |value: f32| half::f16::from_f32(value).to_f32();
        for y in 0..frame.height {
            for x in 0..frame.width {
                let (sx, sy) = (x + frame.origin.0, y + frame.origin.1);
                let at = ((sy * WIDTH + sx) * 4) as usize;
                assert_eq!(
                    frame.texel(x, y).unwrap(),
                    [0, 1, 2].map(|channel| held(table[usize::from(image.rgba[at + channel])])),
                    "{what}: texel ({x}, {y})"
                );
            }
        }
    }
}

/// At a percentage zoom a Dehaze drag plans over the visible region's window, its atmospheric light
/// its light link's, computed from the whole stage at full resolution whatever window the plan draws
/// over; at Fit the same draft reads the same light. A Clarity drag's window is wider than the
/// region by the margin its filters read.
#[test]
fn a_region_plan_reads_dehazes_light_from_the_whole_stage() {
    let rect = crate::modules::Region {
        x0: 200,
        y0: 120,
        width: 160,
        height: 100,
    };
    let region = crate::GpuView::Region {
        rect,
        magnification: 1.0,
    };
    let holds = |window: crate::modules::Region| {
        window.x0 <= rect.x0
            && window.y0 <= rect.y0
            && window.x1() >= rect.x1()
            && window.y1() >= rect.y1()
    };
    let dehaze = Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40.0}));
    let (job, draft) = draft_job("set-presence", Vec::new(), vec![dehaze], 1);
    let preview = plan_preview(&job.evaluation, &draft, region).unwrap();
    let plan = planned(&preview);
    let [light] = &plan.lights[..] else {
        panic!("one light: {:?}", plan.lights);
    };
    assert_eq!(
        (light.layer, light.stage),
        (
            0,
            Stage {
                width: WIDTH,
                height: HEIGHT
            }
        ),
        "the whole stage's"
    );
    assert!(light.over_source() && plan.spatial[0].light.is_some());
    let window = preview
        .boundary
        .as_ref()
        .and_then(|request| request.window)
        .expect("the region's window");
    assert!(holds(window), "{window:?} holds {rect:?}");
    let fit = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    assert_eq!(planned(&fit).lights, plan.lights, "at Fit the same light");
    assert_eq!(fit.boundary.unwrap().window, None);
    let clarity = Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 30.0}));
    let (job, draft) = draft_job("set-presence", Vec::new(), vec![clarity], 1);
    let preview = plan_preview(&job.evaluation, &draft, region).unwrap();
    // The drafted layer's GPU shape holds Dehaze at nought, which reads its light too, so a drag
    // across zero changes words alone.
    assert!(planned(&preview).reads_lights());
    let window = preview
        .boundary
        .unwrap()
        .window
        .expect("the region's window");
    assert!(
        window.x0 < rect.x0
            && window.y0 < rect.y0
            && window.x1() > rect.x1()
            && window.y1() > rect.y1(),
        "{window:?} holds {rect:?} and Clarity's margin"
    );
}

/// A drag reads the light the picture at rest draws with while it leaves the light's input as the
/// committed stack holds it — a Presence drag, on a JPEG and on a RAW — and computes it every tick
/// from the source through the drafted colour where a colour layer before it changes: a Basic drag
/// under Presence. Planning reads no pixel and reduces nothing: the estimate store stays empty.
#[test]
fn a_drag_reads_the_light_at_rest_unless_a_colour_layer_before_it_changes() {
    let raw = PreviewSource::Raw {
        image: crate::render::tests::varied(WIDTH, HEIGHT),
        settings: crate::LinearSettings::default(),
    };
    for (name, source) in [("JPEG", source()), ("RAW", raw)] {
        let context = RenderContext::new();
        let presence = |clarity: i32| {
            Layer::new(
                crate::PRESENCE_EFFECT,
                json!({"dehaze": 40, "clarity": clarity}),
            )
        };
        let entry = vec![presence(30)];
        let plan_of = |action: &str, drafted: Vec<Layer>| {
            let (job, draft) = draft_job_in(
                context.clone(),
                source.clone(),
                action,
                entry.clone(),
                drafted,
                1,
            );
            plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap()
        };
        let at_rest = crate::render::gpu::plan_rest(
            &committed_over(context.clone(), source.clone(), entry.clone()),
            GpuView::Fit(bounds()),
        )
        .unwrap();
        let at_rest = planned(&at_rest.view).lights.clone();
        assert!(
            matches!(&at_rest[..], [light] if light.over_source() && light.content.is_empty()),
            "{name}: {at_rest:?}"
        );
        let clarity = plan_of("set-presence", vec![presence(60)]);
        assert!(
            clarity.boundary.as_ref().unwrap().key.plan().is_some(),
            "{name}: a proxy"
        );
        assert_eq!(
            planned(&clarity).lights,
            at_rest,
            "{name}: the light at rest"
        );
        let under = plan_of(
            "set-basic",
            vec![basic(json!({"exposure": 0.3})), presence(30)],
        );
        let plan = planned(&under);
        assert_eq!(plan.boundary.layer, 0, "{name}");
        let [light] = &plan.lights[..] else {
            panic!("{name}: one light");
        };
        assert!(
            light.layer == 1 && light.over_source() && light.content.len() == 1,
            "{name}: the drafted exposure before Presence, every tick"
        );
        assert_eq!(context.estimates().len(), 0, "{name}: nothing reduced");
    }
}

/// A drag of a restoration or spatial layer the stack holds is warmed by one plan, the layer in
/// its GPU shape, which every drag of it draws: a Presence layer's Clarity drag and the first
/// Texture and Dehaze drags of either sign, and a Detail layer's Sharpening drag and first
/// noise-reduction drags, all find their pipelines warmed, Dehaze's reading the light its light
/// link computes as the warmed plan does.
#[test]
fn the_warmed_plans_hold_a_spatial_layers_drags() {
    for (action, effect, held, drags) in [
        (
            "set-presence",
            crate::PRESENCE_EFFECT,
            json!({"clarity": 30, "dehaze": 40}),
            [
                json!({"clarity": 60, "dehaze": 40}),
                json!({"clarity": 30, "dehaze": 40, "texture": 20}),
                json!({"clarity": 30, "dehaze": -25}),
                json!({"clarity": 30}),
            ],
        ),
        (
            "set-detail",
            crate::DETAIL_EFFECT,
            json!({"sharpening": 40}),
            [
                json!({"sharpening": 80}),
                json!({"sharpening": 40, "luminance": 30}),
                json!({"sharpening": 40, "colour": 30}),
                json!({}),
            ],
        ),
    ] {
        let layer = Layer::new(effect, held);
        let entry = vec![layer.clone()];
        let context = RenderContext::new();
        let job_of = |stack: Vec<Layer>, revision| {
            draft_job_in(
                context.clone(),
                source(),
                action,
                entry.clone(),
                stack,
                revision,
            )
        };
        let (job, _) = job_of(entry.clone(), 0);
        let plans =
            crate::render::gpu::plan_warm(&job.evaluation, crate::GpuView::Fit(bounds())).unwrap();
        // The plans that draft the layer itself; the colour candidates before it, and the
        // vignette's first drag after it, hold it too.
        let own: Vec<&GpuPlan> = plans
            .iter()
            .filter(|plan| {
                !plan.spatial.is_empty() && plan.content.is_empty() && plan.output.is_empty()
            })
            .collect();
        assert_eq!(
            own.len(),
            1,
            "{effect}: one plan, the layer in its GPU shape"
        );
        for drafted in drags {
            let payload = drafted.clone();
            let (job, draft) = job_of(
                vec![Layer {
                    payload,
                    ..layer.clone()
                }],
                1,
            );
            let drag =
                plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
            assert_eq!(
                pipelines(planned(&drag)),
                pipelines(own[0]),
                "the drag to {drafted} is warmed"
            );
        }
    }
}

/// A drag of a restoration or spatial layer across zero — a unit joining and leaving, or noise
/// reduction's coarsest level with Colour — plans one program sequence and one boundary
/// throughout, from the layer absent or held, Detail's and Presence's alike, while the CPU compile
/// of the same stacks holds only the units the values need.
#[test]
fn a_spatial_drag_across_zero_keeps_one_program_sequence_and_one_boundary() {
    let registry = ModuleRegistry::builtin();
    // How many stages the CPU compile of `stack` opens: one for a restoration or spatial layer it
    // holds units of.
    let stages = |stack: &[Layer]| -> usize {
        let compiled = registry
            .compile(WIDTH, HEIGHT, &recipe(stack.to_vec()))
            .unwrap();
        compiled
            .segments
            .iter()
            .filter(|segment| segment.entry.is_some())
            .count()
    };
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({}));
    let detail = Layer::new(crate::DETAIL_EFFECT, json!({}));
    // Each gesture: its action and layer, the values it starts from (`None` for a layer the stack
    // does not hold yet), and the field it drags from zero, through two values and back.
    for (action, layer, held, field, values) in [
        ("set-presence", &presence, None, "dehaze", [40.0, -30.0]),
        (
            "set-presence",
            &presence,
            Some(json!({"texture": 10})),
            "clarity",
            [20.0, -20.0],
        ),
        ("set-detail", &detail, None, "luminance", [30.0, 80.0]),
        (
            "set-detail",
            &detail,
            Some(json!({"sharpening": 40})),
            "colour",
            [30.0, 60.0],
        ),
        (
            "set-detail",
            &detail,
            Some(json!({"luminance": 30})),
            "sharpening",
            [50.0, 150.0],
        ),
    ] {
        let at = |value: f64| {
            let mut payload = held.clone().unwrap_or_else(|| json!({}));
            payload[field] = json!(value);
            Layer {
                payload,
                ..layer.clone()
            }
        };
        let entry: Vec<Layer> = held
            .iter()
            .map(|payload| Layer {
                payload: payload.clone(),
                ..layer.clone()
            })
            .collect();
        let stacks = [
            entry.clone(),
            vec![at(values[0])],
            vec![at(values[1])],
            vec![at(0.0)],
        ];
        let previews: Vec<GpuPreview> = stacks
            .iter()
            .enumerate()
            .map(|(revision, stack)| {
                let (job, draft) = draft_job(action, entry.clone(), stack.clone(), revision as u64);
                plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap()
            })
            .collect();
        let first = planned(&previews[0]);
        for (preview, stack) in previews.iter().zip(&stacks) {
            assert_eq!(
                pipelines(planned(preview)),
                pipelines(first),
                "{action} {field} over {held:?}: {stack:?}"
            );
            assert_eq!(preview.boundary, previews[0].boundary, "one boundary");
        }
        // The CPU's shape: a layer at zero opens no stage; a non-neutral one holds only the units
        // its values need, which the modules' own tests count.
        assert_eq!(
            stages(&stacks[3]),
            usize::from(held.is_some()),
            "{action} {field}"
        );
        assert_eq!(stages(&stacks[1]), 1);
    }
}

/// A 100% region's held boundary identity changes when a spatial drag needs more input support.
#[test]
fn a_region_spatial_window_growth_requests_a_new_boundary() {
    let detail = Layer::new(
        crate::DETAIL_EFFECT,
        json!({"sharpening": 100, "radius": 0.5}),
    );
    let view = crate::GpuView::Region {
        rect: crate::modules::Region {
            x0: 150,
            y0: 100,
            width: 200,
            height: 150,
        },
        magnification: 1.0,
    };
    let request = |radius: f64| {
        let changed = Layer {
            payload: json!({"sharpening": 100, "radius": radius}),
            ..detail.clone()
        };
        let (job, draft) = draft_job("set-detail", vec![detail.clone()], vec![changed], 1);
        plan_preview(&job.evaluation, &draft, view)
            .unwrap()
            .boundary
            .expect("a spatial boundary")
    };
    let small = request(0.5);
    let large = request(3.0);
    assert_eq!(small.key.region(), large.key.region());
    assert_ne!(
        small.window, large.window,
        "the required input support grows"
    );
    assert_ne!(small.key, large.key, "the old boundary must be released");
}

/// At a percentage zoom a restoration or spatial layer's drag is planned in its GPU shape, with
/// the plan of its CPU shape beside it, from the same boundary, for the desktop to draw when only
/// that one fits the budget; the CPU shape holds less. A layer every unit of which is moved has
/// no smaller shape, and at Fit there is no choice to make.
#[test]
fn at_a_percentage_zoom_a_spatial_drag_carries_its_cpu_shape_too() {
    let region = crate::GpuView::Region {
        rect: crate::modules::Region {
            x0: 40,
            y0: 30,
            width: 120,
            height: 90,
        },
        magnification: 1.0,
    };
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"texture": 10}));
    let drag = |payload: Value, view| {
        let dragged = vec![Layer {
            payload,
            ..presence.clone()
        }];
        let (job, draft) = draft_job("set-presence", vec![presence.clone()], dragged, 1);
        plan_preview(&job.evaluation, &draft, view).unwrap()
    };
    let applies = |plan: &GpuPlan| plan.spatial.iter().map(|s| s.applies.len()).sum::<usize>();
    let zoomed = drag(json!({"texture": 10, "clarity": 20}), region);
    assert_eq!(applies(planned(&zoomed)), 3, "every unit");
    let cpu = zoomed
        .cpu_shape
        .as_deref()
        .expect("the CPU's shape beside it");
    assert_eq!(applies(cpu), 2, "Texture and Clarity");
    assert_eq!(cpu.boundary, planned(&zoomed).boundary, "the same boundary");
    let window = zoomed.boundary.as_ref().unwrap().window.unwrap();
    let bytes = |plan: &GpuPlan| {
        plan.spatial
            .first()
            .unwrap()
            .plane_bytes((window.x0, window.y0), (window.width, window.height))
    };
    assert!(bytes(cpu) < bytes(planned(&zoomed)));
    // Detail with both strengths and Colour moved: both units and every level either way.
    let detail = Layer::new(crate::DETAIL_EFFECT, json!({"sharpening": 40}));
    let moved = vec![Layer {
        payload: json!({"sharpening": 40, "luminance": 20, "colour": 30}),
        ..detail.clone()
    }];
    let (job, draft) = draft_job("set-detail", vec![detail], moved, 1);
    let every = plan_preview(&job.evaluation, &draft, region).unwrap();
    assert!(!planned(&every).spatial.is_empty() && every.cpu_shape.is_none());
    let fit = drag(
        json!({"texture": 10, "clarity": 20}),
        crate::GpuView::Fit(bounds()),
    );
    assert!(fit.cpu_shape.is_none(), "no choice at Fit");
}

/// With Detail and Presence both in the stack, the warm list holds a Detail drag's sequence, which
/// chains Presence's operation after Detail's, and a Presence drag's.
#[test]
fn the_warmed_plans_hold_a_drag_of_each_of_two_spatial_layers() {
    let detail = Layer::new(crate::DETAIL_EFFECT, json!({"sharpening": 40}));
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 30, "dehaze": 20}));
    let entry = vec![detail.clone(), presence.clone()];
    let (job, _) = draft_job("set-detail", entry.clone(), entry.clone(), 0);
    let warmed: Vec<Vec<&'static str>> =
        crate::render::gpu::plan_warm(&job.evaluation, crate::GpuView::Fit(bounds()))
            .unwrap()
            .iter()
            .map(sequence)
            .collect();
    let drags = [
        (
            "set-detail",
            vec![
                Layer {
                    payload: json!({"sharpening": 70, "luminance": 20}),
                    ..detail.clone()
                },
                presence.clone(),
            ],
            2,
        ),
        (
            "set-presence",
            vec![
                detail,
                Layer {
                    payload: json!({"clarity": 50, "dehaze": 20}),
                    ..presence
                },
            ],
            2,
        ),
    ];
    for (action, drafted, chained) in drags {
        let (job, draft) = draft_job(action, entry.clone(), drafted, 1);
        let drag = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
        assert_eq!(planned(&drag).spatial.len(), chained, "{action}");
        assert!(
            warmed.contains(&sequence(planned(&drag))),
            "{action}'s sequence is warmed"
        );
    }
}

/// Behind Detail, Dehaze's light reads Detail's exact output, which only the picture at rest's
/// sweep of the whole stage at full resolution computes: every plan reading it holds that light's
/// link, Detail's operation in its prefix, and beside it the stand-in with Detail left out, over
/// the source, which a slot computes while it holds no swept light. At a percentage zoom, with
/// nothing rendered and nothing stored, a Presence drag plans at once, reading the light at rest,
/// its plan running Detail's operation, then Presence's, from the photograph — the boundary every
/// gesture over the view starts from — over the window the region reads, every texel the whole
/// stage's. A Detail drag reads the light of the stack it started from, the slot's still; a Basic
/// drag between them, which changes the light's input by its tone, computes it every tick from the
/// source with Detail left out, the recorded default.
#[test]
fn behind_detail_a_region_plan_reads_the_light_at_rest_or_its_stand_in() {
    let rect = crate::modules::Region {
        x0: 200,
        y0: 120,
        width: 160,
        height: 100,
    };
    let region = crate::GpuView::Region {
        rect,
        magnification: 1.0,
    };
    let context = RenderContext::new();
    let detail = Layer::new(
        crate::DETAIL_EFFECT,
        json!({"sharpening": 40, "luminance": 20}),
    );
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40, "clarity": 20}));
    let entry = vec![detail.clone(), presence.clone()];
    let presence_drag = vec![
        detail.clone(),
        Layer {
            payload: json!({"dehaze": 60, "clarity": 20}),
            ..presence.clone()
        },
    ];
    let detail_drag = vec![
        Layer {
            payload: json!({"sharpening": 70, "luminance": 20}),
            ..detail.clone()
        },
        presence.clone(),
    ];
    let plan_of = |action: &str, drafted: &[Layer]| {
        let (job, draft) = draft_job_in(
            context.clone(),
            source(),
            action,
            entry.clone(),
            drafted.to_vec(),
            1,
        );
        (plan_preview(&job.evaluation, &draft, region).unwrap(), job)
    };
    let at_rest = crate::render::gpu::plan_rest(
        &committed(context.clone(), entry.clone()),
        GpuView::Fit(bounds()),
    )
    .unwrap();
    let swept = at_rest.lights.expect("Detail's exact output is swept");
    let at_rest = planned(&at_rest.view).lights.clone();
    let [light] = &at_rest[..] else {
        panic!("one light: {at_rest:?}");
    };
    assert_eq!(swept.lights[0].light, *light);
    assert!(light.layer == 1 && !light.over_source() && light.spatial.len() == 1);
    let stand_in = light.stand_in.as_deref().expect("its stand-in");
    assert!(stand_in.over_source() && stand_in.left_out == [0]);

    let (preview, mut job) = plan_of("set-presence", &presence_drag);
    let plan = planned(&preview);
    assert_eq!(plan.lights, at_rest, "the light at rest");
    assert_eq!(
        plan.spatial.len(),
        2,
        "Detail's operation, then Presence's, from the photograph"
    );
    let request = preview.boundary.clone().expect("a boundary");
    let window = request.window.expect("the region's window");
    assert!(
        window.x0 < rect.x0 && window.x1() > rect.x1(),
        "{window:?} holds {rect:?} and Presence's margin"
    );
    // The window planned on the owner, which the photo surface cuts from the source it holds, is
    // the one the CPU's reference render of the region's boundary at the source holds, its origin
    // moved by the plan's lead and rounded down to its multiple.
    job.viewport = Some(rect);
    let exact = job.evaluation.exact(&crate::Cancel::never()).unwrap();
    let cut = exact
        .region_boundary(rect, (0, 0), request.format)
        .expect("the region's boundary");
    let anchor = plan.anchor();
    assert_eq!(
        anchor,
        crate::GpuAnchor {
            multiple: (64, 64),
            lead: (300, 300)
        },
        "the drafted layer's three units, Texture's at nought, as a drag's shape holds them"
    );
    assert_eq!(
        request.window,
        Some(crate::anchored(
            crate::modules::Region {
                x0: cut.origin.0,
                y0: cut.origin.1,
                width: cut.width,
                height: cut.height,
            },
            anchor
        )),
        "the window planned on the owner"
    );
    assert_eq!(
        (window.x0 % anchor.multiple.0, window.y0 % anchor.multiple.1),
        (0, 0)
    );
    let frame = exact
        .source_boundary(window, request.format)
        .expect("the window of the source");
    let whole_stage = Stage {
        width: WIDTH,
        height: HEIGHT,
    };
    let whole = exact
        .boundary(
            job.evaluation.compiled().unwrap(),
            whole_stage,
            crate::modules::Region::whole(whole_stage),
            (0, 0),
            request.format,
        )
        .unwrap();
    // The boundary keeps what the plan's GPU window reads of the photograph, inside the halos of
    // every edge the cut leaves inside the stage: the whole stage's texels exactly.
    for y in 0..frame.height {
        for x in 0..frame.width {
            assert_eq!(
                frame.texel(x, y).unwrap().map(f32::to_bits),
                whole
                    .texel(x + frame.origin.0, y + frame.origin.1)
                    .unwrap()
                    .map(f32::to_bits),
                "texel ({x}, {y}) of the window"
            );
        }
    }

    let (preview, _) = plan_of("set-detail", &detail_drag);
    let plan = planned(&preview);
    assert_eq!(plan.spatial.len(), 2, "Detail's operation, then Presence's");
    assert_eq!(
        plan.lights, at_rest,
        "the light of the stack it started from"
    );
    assert!(preview.boundary.unwrap().window.is_some());
    let basic_drag = vec![
        detail.clone(),
        basic(json!({"exposure": 1.0})),
        presence.clone(),
    ];
    let (preview, _) = plan_of("set-basic", &basic_drag);
    let [light] = &planned(&preview).lights[..] else {
        panic!("one light");
    };
    assert!(
        light.layer == 2
            && light.over_source()
            && light.left_out == [0]
            && light.content.len() == 1,
        "the drafted exposure over the source, Detail left out: {light:?}"
    );
}

/// A drag after Presence, at a percentage zoom, runs Presence on the GPU from the stack's first
/// content layer over the window its region reads, behind Detail too: it plans at once, nothing
/// rendered or stored, reading the light the picture at rest draws with, which no window prepares.
#[test]
fn a_drag_after_presence_behind_detail_reads_the_light_at_rest() {
    let rect = crate::modules::Region {
        x0: 200,
        y0: 120,
        width: 160,
        height: 100,
    };
    let region = crate::GpuView::Region {
        rect,
        magnification: 1.0,
    };
    let detail = Layer::new(crate::DETAIL_EFFECT, json!({"sharpening": 40}));
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40}));
    let vignette = Layer::new(crate::VIGNETTE_EFFECT, json!({"amount": -30}));
    let entry = vec![detail.clone(), presence.clone()];
    let drafted = vec![detail, presence, vignette];
    let context = RenderContext::new();
    let (job, draft) = draft_job_in(
        context.clone(),
        source(),
        "set-vignette",
        entry.clone(),
        drafted,
        1,
    );
    let preview = plan_preview(&job.evaluation, &draft, region).unwrap();
    let plan = planned(&preview);
    assert_eq!(
        plan.spatial.len(),
        2,
        "the vignette's plan starts at Detail and runs Presence"
    );
    let at_rest =
        crate::render::gpu::plan_rest(&committed(context.clone(), entry), GpuView::Fit(bounds()))
            .unwrap();
    assert_eq!(
        plan.lights,
        planned(&at_rest.view).lights,
        "the light at rest"
    );
    assert!(!plan.lights[0].over_source());
    assert!(preview.boundary.unwrap().window.is_some());
    assert_eq!(context.estimates().len(), 0, "nothing reduced");
}

/// At Fit behind a straightened crop, whose proxy holds a window of its stage, a drag's light is
/// the whole exact stage's, as the CPU's windowed proxy is handed: a Presence drag reads it, and a
/// Basic drag under Dehaze computes it every tick from the source through the drafted exposure.
/// Nothing is rendered or stored first, and planning reduces nothing.
#[test]
fn behind_a_windowed_fit_proxy_a_plan_reads_the_whole_stages_light() {
    // A proxy stage wider than Dehaze's 512 px tiles, and a crop on its right, so the window the
    // crop reads starts past the stage's first tile.
    let (width, height) = (2400, 1600);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend([
                (x * 251 / width) as u8,
                (y * 241 / height) as u8,
                ((x * 7 + y * 3) % 256) as u8,
                255,
            ]);
        }
    }
    let source = || {
        PreviewSource::Jpeg(SourceImage {
            width,
            height,
            rgba: rgba.clone().into(),
            fingerprint: "sha256:gpu-preview-window-fixture".into(),
            orientation: 1,
            capture: Default::default(),
        })
    };
    let crop = || Layer::crop(fitted_crop(width, height, 4.0, [0.6, 0.3, 0.35, 0.4]));
    let bounds = ProxyBounds {
        width: 400,
        height: 300,
    };
    let context = RenderContext::new();
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40}));
    let entry = vec![presence.clone(), crop()];
    let plan_of = |action: &str, drafted: Vec<Layer>| {
        let (job, draft) =
            draft_job_in(context.clone(), source(), action, entry.clone(), drafted, 1);
        plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds)).unwrap()
    };
    let whole = Stage { width, height };
    let preview = plan_of(
        "set-presence",
        vec![
            Layer {
                payload: json!({"dehaze": 60}),
                ..presence.clone()
            },
            crop(),
        ],
    );
    assert!(
        preview
            .boundary
            .as_ref()
            .and_then(|request| request.key.plan())
            .is_some_and(|plan| plan.window.is_some()),
        "a windowed proxy"
    );
    let [light] = &planned(&preview).lights[..] else {
        panic!("one light");
    };
    assert!(light.stage == whole && light.over_source() && light.content.is_empty());
    let preview = plan_of(
        "set-basic",
        vec![basic(json!({"exposure": 1.0})), presence.clone(), crop()],
    );
    let [light] = &planned(&preview).lights[..] else {
        panic!("one light");
    };
    assert!(light.stage == whole && light.over_source() && light.content.len() == 1);
    assert_eq!(context.estimates().len(), 0, "planning reduces nothing");
}

/// The committed `layers` over [`source`], evaluated in `context`, as a displayed stack's job is.
fn committed(context: RenderContext, layers: Vec<Layer>) -> Evaluation {
    committed_over(context, source(), layers)
}

/// [`committed`] over `source`.
fn committed_over(context: RenderContext, source: PreviewSource, layers: Vec<Layer>) -> Evaluation {
    let (job, _) = draft_job_in(context, source, "set-basic", layers.clone(), layers, 0);
    let evaluation = job.evaluation;
    Evaluation::new(
        evaluation.registry().clone(),
        evaluation.context().clone(),
        evaluation.source().clone(),
        evaluation.entry().clone(),
        evaluation.recipe().clone(),
        None,
    )
}

/// At Fit, where the view draws the output stage smaller than it is, a committed stack's picture
/// at rest is planned process-first: the whole stack's plan at the exact stage from the source,
/// over tiles that cover the output stage once, row by row, each with the window of the source it
/// reads, anchored to the plan; reduced to the proxy frame's size by the proxy build's own area
/// average. A view that draws the stage at its own size plans none, and a Presence stack's
/// windows start on its anchor's multiples.
#[test]
fn a_picture_at_rest_is_planned_in_anchored_tiles_of_the_output_stage() {
    for (what, layers, anchor) in [
        (
            "Basic",
            vec![basic(json!({"exposure": 0.5}))],
            crate::GpuAnchor::NONE,
        ),
        (
            "Texture",
            vec![Layer::new(crate::PRESENCE_EFFECT, json!({"texture": 30.0}))],
            crate::GpuAnchor {
                multiple: (16, 16),
                lead: (60, 60),
            },
        ),
    ] {
        let evaluation = committed(RenderContext::new(), layers);
        let rest = crate::render::gpu::plan_rest(&evaluation, GpuView::Fit(bounds())).unwrap();
        let tiles = match rest.tiles {
            Some(Ok(tiles)) => tiles,
            other => panic!("{what}: {other:?}"),
        };
        assert_eq!(tiles.plan.anchor(), anchor, "{what}");
        assert_eq!(
            (tiles.output.width, tiles.output.height),
            (WIDTH, HEIGHT),
            "{what}"
        );
        let proxy = rest
            .view
            .boundary
            .as_ref()
            .and_then(|request| request.key.plan())
            .expect("a proxy");
        assert_eq!(tiles.view, (proxy.width, proxy.height), "{what}");
        assert_eq!(tiles.across.first.len(), proxy.width as usize);
        assert_eq!(tiles.down.first.len(), proxy.height as usize);
        let area: u64 = tiles.tiles.iter().map(|tile| tile.rect.pixels()).sum();
        assert_eq!(area, u64::from(WIDTH * HEIGHT), "{what}: every pixel once");
        for (index, tile) in tiles.tiles.iter().enumerate() {
            let (rect, window) = (tile.rect, tile.window);
            assert!(
                window.x0 <= rect.x0
                    && window.y0 <= rect.y0
                    && window.x1() >= rect.x1()
                    && window.y1() >= rect.y1()
                    && window.x1() <= WIDTH
                    && window.y1() <= HEIGHT,
                "{what}: tile {index}'s window {window:?} holds {rect:?}"
            );
            assert_eq!(
                (window.x0 % anchor.multiple.0, window.y0 % anchor.multiple.1),
                (0, 0),
                "{what}: anchored"
            );
            if index > 0 {
                let before = tiles.tiles[index - 1].rect;
                assert!(
                    (rect.y0, rect.x0) > (before.y0, before.x0),
                    "{what}: row by row"
                );
            }
        }
    }
    // A view that draws the stage at its own size: the view's plan is the picture at rest.
    let evaluation = committed(RenderContext::new(), vec![basic(json!({"exposure": 0.5}))]);
    let rest = crate::render::gpu::plan_rest(
        &evaluation,
        GpuView::Fit(ProxyBounds {
            width: 2000,
            height: 2000,
        }),
    )
    .unwrap();
    assert!(rest.tiles.is_none());
}

/// A picture at rest of a Dehaze stack is planned at once, nothing rendered or stored: its tiles'
/// plan reads the light its light link computes from the source. Behind Detail the light reads
/// Detail's exact output, so the picture at rest sweeps the whole content stage at full resolution
/// in tiles whose origins and sides are whole 16-pixel blocks, each block in one tile, every tile's
/// window holding what Detail's halo reads; a sweep's side must be whole blocks.
#[test]
fn a_picture_at_rest_reads_its_lights_and_sweeps_those_behind_detail() {
    let dehaze = Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40.0}));
    let evaluation = committed(RenderContext::new(), vec![dehaze.clone()]);
    let rest = crate::render::gpu::plan_rest(&evaluation, GpuView::Fit(bounds())).unwrap();
    let tiles = match &rest.tiles {
        Some(Ok(tiles)) => tiles,
        other => panic!("{other:?}"),
    };
    assert!(tiles.plan.reads_lights() && tiles.plan.lights[0].over_source());
    assert!(
        rest.lights.is_none(),
        "a light over the source needs no sweep"
    );

    let detail = Layer::new(crate::DETAIL_EFFECT, json!({"sharpening": 40}));
    let evaluation = committed(RenderContext::new(), vec![detail, dehaze]);
    let rest = crate::render::gpu::plan_rest(&evaluation, GpuView::Fit(bounds())).unwrap();
    let lights = planned(&rest.view).lights.clone();
    let swept = rest.lights.expect("a sweep");
    let [one] = &swept.lights[..] else {
        panic!("one light swept");
    };
    let stage = Stage {
        width: WIDTH,
        height: HEIGHT,
    };
    assert_eq!((one.k, &one.light, swept.stage), (0, &lights[0], stage));
    let check = |tiles: &[crate::RestTile]| {
        let area: u64 = tiles.iter().map(|tile| tile.rect.pixels()).sum();
        assert_eq!(area, u64::from(WIDTH * HEIGHT), "every pixel once");
        for tile in tiles {
            let (rect, window) = (tile.rect, tile.window);
            assert_eq!((rect.x0 % 16, rect.y0 % 16), (0, 0), "{rect:?}");
            assert!(
                window.x0 <= rect.x0
                    && window.y0 <= rect.y0
                    && window.x1() >= rect.x1()
                    && window.y1() >= rect.y1()
                    && window.x1() <= WIDTH
                    && window.y1() <= HEIGHT,
                "{window:?} holds {rect:?}"
            );
        }
    };
    check(&one.tiles);
    let small = crate::render::gpu::plan_rest_lights(&evaluation, &lights, Some(64))
        .unwrap()
        .expect("a sweep");
    assert_eq!(small.lights[0].tiles.len(), 10 * 7);
    check(&small.lights[0].tiles);
    assert!(crate::render::gpu::plan_rest_lights(&evaluation, &lights, Some(40)).is_err());
}

// The warm list over as many masked spatial layers as a recipe may hold.
mod warm;
