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
            recipe: recipe(entry),
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
        recipe(drafted),
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

/// Every program the plan runs, in order: its program sequence.
fn sequence(plan: &GpuPlan) -> Vec<&'static str> {
    plan.operations()
        .flat_map(|operation| operation.units.iter().map(|unit| unit.program.entry))
        .collect()
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
            plan_preview(&job.evaluation, &draft, bounds()).unwrap()
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
        let preview = plan_preview(&job.evaluation, &draft, bounds()).unwrap();
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
    let preview = plan_preview(&job.evaluation, &draft, bounds()).unwrap();
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
    let preview = plan_preview(&job.evaluation, &draft, bounds()).unwrap();
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
    let preview = plan_preview(&job.evaluation, &draft, bounds()).unwrap();
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
    let warmed: Vec<Vec<&'static str>> = crate::render::gpu::plan_warm(&job.evaluation, bounds())
        .unwrap()
        .iter()
        .map(sequence)
        .collect();
    for (index, one) in warmed.iter().enumerate() {
        assert!(!warmed[..index].contains(one), "each sequence once");
    }
    let (job, draft) = draft_job(
        "set-basic",
        vec![crop.clone()],
        vec![basic(json!({"exposure": 0.6})), crop],
        1,
    );
    let drag = plan_preview(&job.evaluation, &draft, bounds()).unwrap();
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
    let preview = plan_preview(&job.evaluation, &draft, bounds()).unwrap();
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
            plan_preview(&job.evaluation, &draft, bounds()).unwrap()
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
