//! A draft's GPU preview with its preview job: the plan and its boundary planned on the catalog
//! owner, one program sequence and one boundary through a drag that leaves neutral, and the
//! boundary rendered once by the preview worker after the job's Fit frame.
use super::*;
use crate::{
    AssetId, BASIC_EFFECT, Draft, DraftStamp, EntryId, Evaluation, GpuAnswer, GpuPlan, GpuPreview,
    HistoryEntry, Layer, ModuleRegistry, ProxyBounds, RECIPE_FORMAT, Recipe, RenderContext,
    Snapshot, SnapshotId, SourceImage,
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
        Arc::new(ModuleRegistry::builtin()),
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
/// word, which is what a drag changes.
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
        keys.extend(spatial.applies.iter().map(|apply| format!("{apply:?}")));
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

/// The boundary's key names the layers before it: a drag of the Tone curve after a Basic layer
/// keeps its key while only the curve moves, and another Basic value is another key.
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
        assert_eq!(planned(&preview).boundary.layer, 1, "the curve's input");
        assert!(planned(&preview).boundary.continues_run);
        preview.boundary.expect("a boundary").key
    };
    assert_eq!(key(0.5, 0.6), key(0.5, 0.4));
    assert_ne!(key(0.5, 0.6), key(0.7, 0.6));
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
}

/// A job that asks for the boundary delivers it after its Fit frame, as one more result of its
/// generation: the input of the drafted layer over the window of the proxy stage the crop reads,
/// which for a first layer is the proxy source itself, decoded. A job that does not ask delivers
/// its frame alone.
#[test]
fn a_job_that_asks_renders_its_boundary_after_its_fit_frame() {
    let (mut job, draft) = draft_job(
        "set-basic",
        vec![crop()],
        vec![basic(json!({"exposure": 0.3})), crop()],
        2,
    );
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    let request = preview.boundary.clone().expect("a boundary");
    let plan = request.key.plan().expect("a proxy");
    let window = plan.window.expect("the crop's window");
    job.proxy = Some(bounds());
    job.intent = PreviewIntent::Interactive;
    job.boundary = Some(request.clone());
    let proxied = job.evaluation.source().proxy(plan).unwrap();
    let mut plain = job.clone();
    plain.boundary = None;
    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    let proxy = wait_for("the Fit frame", || queue.poll());
    assert_eq!(
        (proxy.generation, proxy.phase()),
        (generation, PreviewPhase::Proxy)
    );
    let boundary = wait_for("the boundary", || queue.poll());
    assert_eq!(
        (boundary.generation, boundary.phase()),
        (generation, PreviewPhase::Boundary)
    );
    assert_eq!(boundary.draft_revision, Some(2));
    let outcome = boundary.boundary().unwrap();
    assert_eq!(outcome.key, request.key);
    let frame = outcome.result.as_ref().unwrap();
    assert_eq!(request.format, crate::BoundaryFormat::Half, "a JPEG's");
    assert_eq!(frame.format, crate::BoundaryFormat::Half);
    assert_eq!(frame.origin, (window.x, window.y));
    assert_eq!((frame.width, frame.height), (window.width, window.height));
    assert_eq!(
        frame.stage,
        Stage {
            width: plan.width,
            height: plan.height
        }
    );
    let PreviewSource::Jpeg(image) = &proxied else {
        panic!("a JPEG proxy")
    };
    assert_eq!((image.width, image.height), (window.width, window.height));
    let table = decode_table();
    let held = |value: f32| half::f16::from_f32(value).to_f32();
    for (index, pixel) in image.rgba.chunks_exact(4).enumerate() {
        let (x, y) = (index as u32 % image.width, index as u32 / image.width);
        assert_eq!(
            frame.texel(x, y).unwrap(),
            [0, 1, 2].map(|channel| held(table[usize::from(pixel[channel])])),
            "texel ({x}, {y})"
        );
    }
    // Without the request, the job ends with its frame.
    let generation = queue.request(plain);
    let proxy = wait_for("the plain Fit frame", || queue.poll());
    assert_eq!(
        (proxy.generation, proxy.phase()),
        (generation, PreviewPhase::Proxy)
    );
    luxforge_testbase::wait_until("the job to end", || !queue.is_busy());
    assert!(queue.poll().is_none(), "no boundary was asked for");
}

/// A developed RAW's boundary, on the linear path, holds `f32` texels: every value of the proxy the
/// Fit frame was rendered from, exactly, as the plan of the linear path reads it.
#[test]
fn a_raw_boundary_holds_its_values_as_f32() {
    let raw = PreviewSource::Raw {
        image: crate::render::tests::varied(WIDTH, HEIGHT),
        settings: crate::LinearSettings::default(),
    };
    let (mut job, draft) = draft_job_over(
        raw,
        "set-basic",
        Vec::new(),
        vec![basic(json!({"exposure": 0.3}))],
        2,
    );
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    let request = preview.boundary.clone().expect("a boundary");
    assert_eq!(request.format, crate::BoundaryFormat::Float);
    let plan = request.key.plan().expect("a proxy");
    let proxied = job.evaluation.source().proxy(plan).unwrap();
    job.proxy = Some(bounds());
    job.intent = PreviewIntent::Interactive;
    job.boundary = Some(request.clone());
    let mut queue = PreviewQueue::default();
    queue.request(job);
    let _ = wait_for("the Fit frame", || queue.poll());
    let boundary = wait_for("the boundary", || queue.poll());
    let frame = boundary
        .boundary()
        .unwrap()
        .result
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(frame.format, crate::BoundaryFormat::Float);
    let PreviewSource::Raw { image, .. } = &proxied else {
        panic!("a RAW proxy")
    };
    assert_eq!((frame.width, frame.height), (image.width(), image.height()));
    assert_eq!(
        frame.texels.len(),
        (frame.width * frame.height * 16) as usize
    );
    for y in 0..frame.height {
        for x in 0..frame.width {
            assert_eq!(
                frame.texel(x, y).unwrap().map(f32::to_bits),
                image.pixel(x, y).unwrap().map(f32::to_bits),
                "texel ({x}, {y})"
            );
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
    let plans = crate::render::gpu::plan_warm(&job.evaluation, bounds()).unwrap();
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
        let plans = crate::render::gpu::plan_warm(&job.evaluation, bounds()).unwrap();
        plans
            .into_iter()
            .find(|plan| plan.content.is_empty() && !plan.spatial.is_empty())
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

/// A job whose Fit frame is drawn at another stage than its boundary names still answers the
/// request, with the reason, so the desktop never waits for a boundary that will not come.
#[test]
fn a_boundary_of_another_stage_is_answered_with_its_reason() {
    let (mut job, draft) = draft_job(
        "set-basic",
        vec![crop()],
        vec![basic(json!({"exposure": 0.3})), crop()],
        3,
    );
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    job.proxy = Some(ProxyBounds {
        width: 220,
        height: 150,
    });
    job.intent = PreviewIntent::Interactive;
    job.boundary = preview.boundary;
    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    let proxy = wait_for("the Fit frame", || queue.poll());
    assert_eq!(proxy.phase(), PreviewPhase::Proxy);
    let boundary = wait_for("the boundary's answer", || queue.poll());
    assert_eq!(
        (boundary.generation, boundary.phase()),
        (generation, PreviewPhase::Boundary)
    );
    let error = boundary.boundary().unwrap().result.as_ref().unwrap_err();
    assert!(error.detail.contains("another stage"), "{error}");
}

/// At a percentage zoom the boundary is the exact stage's window the visible region reads, at full
/// scale: keyed by that region, rendered after the job's region frame, and its texels the layer's
/// input there exactly. Behind a straightened crop the window is the crop's read of the source.
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
        job.intent = PreviewIntent::Interactive;
        job.boundary = Some(request.clone());
        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let region = wait_for("the region frame", || queue.poll());
        assert_eq!(
            (region.generation, region.phase()),
            (generation, PreviewPhase::Region),
            "{what}"
        );
        let boundary = wait_for("the boundary", || queue.poll());
        assert_eq!(
            (boundary.generation, boundary.phase()),
            (generation, PreviewPhase::Boundary),
            "{what}"
        );
        let outcome = boundary.boundary().unwrap();
        assert_eq!(outcome.key, request.key);
        let frame = outcome.result.as_ref().unwrap();
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

/// At a percentage zoom a Dehaze plan whose atmospheric light the GPU would take from the visible
/// region alone, where the exact visible region reads the whole stage's, is the CPU's, naming why
/// and asking for no boundary; at Fit, where the GPU holds the whole stage, the same draft plans.
/// A spatial plan with no global estimate plans over the region, its window wider than the region
/// by the margin its filters read.
#[test]
fn a_region_plan_never_takes_a_spatial_estimate_from_the_region_alone() {
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
    let dehaze = Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": 40.0}));
    let (job, draft) = draft_job("set-presence", Vec::new(), vec![dehaze], 1);
    let preview = plan_preview(&job.evaluation, &draft, region).unwrap();
    match &preview.answer {
        GpuAnswer::Fallback(reason) => {
            assert_eq!(reason.code(), "region-estimate");
            assert_eq!(reason.layer(), Some(0));
        }
        GpuAnswer::Plan(_) => panic!("a region plan took Dehaze's light from the region"),
    }
    assert!(preview.boundary.is_none());
    let fit = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    assert!(
        planned(&fit).approximate(),
        "at Fit the GPU takes it over the whole stage"
    );
    assert_eq!(fit.boundary.unwrap().window, None);
    let clarity = Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 30.0}));
    let (job, draft) = draft_job("set-presence", Vec::new(), vec![clarity], 1);
    let preview = plan_preview(&job.evaluation, &draft, region).unwrap();
    assert!(!planned(&preview).approximate());
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

/// A Presence drag reads Dehaze's light from the estimate store the committed stack's Fit frame
/// filled, under the name of the proxy that frame was rendered from, which planning never builds:
/// once that frame is rendered, the drag's plan holds the CPU's light and is not approximate, on a
/// JPEG and on a RAW. Before it, and for a drag of a layer under Presence, which changes the input
/// the light is estimated from, the light is taken on the GPU and the plan says so.
#[test]
fn a_presence_drag_reads_the_light_its_fit_frame_stored() {
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
        let clarity = || plan_of("set-presence", vec![presence(60)]);
        let first = clarity();
        assert!(
            first.boundary.as_ref().unwrap().key.plan().is_some(),
            "{name}: a proxy"
        );
        assert!(
            planned(&first).approximate(),
            "{name}: nothing is stored yet"
        );
        // The committed stack's Fit frame, through the preview worker in the same context.
        let (mut job, _) = draft_job_in(
            context.clone(),
            source.clone(),
            "set-presence",
            entry.clone(),
            entry.clone(),
            0,
        );
        job.proxy = Some(bounds());
        job.intent = PreviewIntent::Interactive;
        let mut queue = PreviewQueue::default();
        queue.request(job);
        let frame = wait_for("the Fit frame", || queue.poll());
        assert_eq!(frame.phase(), PreviewPhase::Proxy, "{name}");
        let stored = clarity();
        assert!(
            !planned(&stored).approximate(),
            "{name}: the stored light is read"
        );
        let under = plan_of(
            "set-basic",
            vec![basic(json!({"exposure": 0.3})), presence(30)],
        );
        assert_eq!(planned(&under).boundary.layer, 0, "{name}");
        assert!(
            planned(&under).approximate(),
            "{name}: a drag under Presence changes the light's input"
        );
    }
}

/// A drag of a restoration or spatial layer the stack holds is warmed by one plan, the layer in
/// its GPU shape, which every drag of it draws: a Presence layer's Clarity drag and the first
/// Texture and Dehaze drags of either sign, and a Detail layer's Sharpening drag and first
/// noise-reduction drags, all find their pipelines warmed. Dehaze's light is stored by the
/// committed stack's Fit frame only after the warm list is planned with its job, and the drags
/// that read it still find theirs.
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
        let plans = crate::render::gpu::plan_warm(&job.evaluation, bounds()).unwrap();
        // The plans that start at the layer itself; the colour candidates before it hold it too.
        let own: Vec<&GpuPlan> = plans
            .iter()
            .filter(|plan| !plan.spatial.is_empty() && plan.content.is_empty())
            .collect();
        assert_eq!(
            own.len(),
            1,
            "{effect}: one plan, the layer in its GPU shape"
        );
        // The committed stack's Fit frame, through the preview worker in the same context, fills
        // the estimate store after the warm list was planned.
        let (mut job, _) = job_of(entry.clone(), 0);
        job.proxy = Some(bounds());
        job.intent = PreviewIntent::Interactive;
        let mut queue = PreviewQueue::default();
        queue.request(job);
        let frame = wait_for("the Fit frame", || queue.poll());
        assert_eq!(frame.phase(), PreviewPhase::Proxy, "{effect}");
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
            assert!(
                !planned(&drag).approximate(),
                "{effect}: a stored estimate is read"
            );
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

/// At a percentage zoom a Dehaze drag reads the light the exact visible region's render stored over
/// the whole stage, as the CPU frame that settles it does: before that render the store holds none
/// and the drag keeps the CPU path (`region-estimate`); after it the region plan holds the CPU's
/// light, is not approximate, and names its boundary's window.
#[test]
fn a_region_drag_reads_the_light_its_exact_region_stored() {
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
    let dehaze = |amount: i32| Layer::new(crate::PRESENCE_EFFECT, json!({"dehaze": amount}));
    let entry = vec![dehaze(40)];
    let plan_of = || {
        let (job, draft) = draft_job_in(
            context.clone(),
            source(),
            "set-presence",
            entry.clone(),
            vec![dehaze(60)],
            1,
        );
        plan_preview(&job.evaluation, &draft, region).unwrap()
    };
    let first = plan_of();
    assert!(
        matches!(&first.answer, GpuAnswer::Fallback(reason) if reason.code() == "region-estimate"),
        "nothing is stored yet"
    );
    // The committed stack's exact visible region, as the quiet policy settles it, in the same
    // context.
    let (mut job, _) = draft_job_in(
        context.clone(),
        source(),
        "set-presence",
        entry.clone(),
        entry.clone(),
        0,
    );
    job.viewport = Some(rect);
    job.intent = PreviewIntent::Settle;
    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    wait_for("the exact region", || {
        let result = queue.poll()?;
        (result.generation == generation && result.phase() == PreviewPhase::Region).then_some(())
    });
    let stored = plan_of();
    assert!(!planned(&stored).approximate(), "the stored light is read");
    assert!(stored.boundary.unwrap().window.is_some());
}

/// With Detail and Presence both in the stack, the warm list holds a Detail drag's sequence, which
/// chains Presence's operation after Detail's, and a Presence drag's.
#[test]
fn the_warmed_plans_hold_a_drag_of_each_of_two_spatial_layers() {
    let detail = Layer::new(crate::DETAIL_EFFECT, json!({"sharpening": 40}));
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 30, "dehaze": 20}));
    let entry = vec![detail.clone(), presence.clone()];
    let (job, _) = draft_job("set-detail", entry.clone(), entry.clone(), 0);
    let warmed: Vec<Vec<&'static str>> = crate::render::gpu::plan_warm(&job.evaluation, bounds())
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
            1,
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
