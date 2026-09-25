//! The preview queue and its worker, end to end.

use super::worker::mask_overlay_for;
use super::*;
use crate::{
    ActivityBoard, AssetId, BASIC_EFFECT, BoxRect, Cancel, CropStage, EFFECT_FORMAT, EntryId,
    HistoryEntry, Layer, LayerId, Mask, ModuleRegistry, Orientation, PIXEL_EFFECT, ProxyBounds,
    RECIPE_FORMAT, Recipe, RenderContext, RenderOptions, Snapshot, SnapshotId, SourceImage,
    Transform, analysis::AnalysisIdentity, render,
};
use crate::{Component, ComponentMode};
use serde_json::json;
use std::sync::Arc;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// Deadlines are generous on purpose: these tests assert order and content, never speed, and
/// they run unoptimized in the workspace check.
const DEADLINE: Duration = Duration::from_secs(120);

fn entry(color: u8) -> PreviewJob {
    job(color, false)
}

fn job(color: u8, analyse: bool) -> PreviewJob {
    let asset = AssetId::new();
    let original = Snapshot::original(asset.clone());
    let snapshot = original.append(Layer::pixel(0, 0, [color, 0, 0])).unwrap();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset,
        sequence: u64::from(color),
        action_id: "set-pixel".into(),
        label: "Pixel 0, 0".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 0,
        snapshot,
        undo_parent: None,
        restore_target: None,
    };
    let recipe = entry.snapshot.recipe.clone();
    let identity = AnalysisIdentity::of(
        &entry.asset_id.clone(),
        "test",
        &entry,
        &recipe,
        None,
        Some((1, 1)),
    )
    .unwrap();
    PreviewJob {
        source: PreviewSource::Jpeg(SourceImage {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255].into(),
            fingerprint: "test".into(),
            orientation: 1,
        }),
        registry: Arc::new(ModuleRegistry::builtin()),
        context: crate::render::testing::context().clone(),
        recipe,
        layer_count: None,
        draft_revision: None,
        identity,
        analyse,
        proxy: None,
        mask_overlay: None,
        entry,
    }
}

/// [`entry`] with one held colour layer after its pixel layer, so its render waits at `gate`
/// while the gate is shut and otherwise renders the same picture.
fn held_entry(gate: &Arc<crate::modules::RenderGate>, color: u8) -> PreviewJob {
    let mut job = entry(color);
    job.recipe.layers.push(Layer {
        id: LayerId::new(),
        effect_id: crate::modules::HELD_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({}),
        artifacts: Vec::new(),
        mask: None,
    });
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(crate::modules::HeldModule::shared(gate.clone()))
        .expect("a valid holding module");
    job.registry = Arc::new(registry);
    job
}

/// The pending slot is still newest-wins: three rapid requests run at most two jobs, the second
/// is replaced by the third, and the third is what the display ends on. The first job is held
/// at its gate inside the one chunk of its one-pixel render, so it is still running when the
/// others are requested, and past the last point that render reads its token: it finishes
/// although superseded, and its frame is delivered, because a frame newer than what is on
/// screen is never thrown away — that is what starves a drag. Each job that started delivers
/// one exact outcome, in increasing order, and the replaced one, which the request that
/// replaced it names, delivers nothing.
#[test]
fn newest_preview_wins_with_one_active_and_one_pending() {
    let gate = crate::modules::RenderGate::open_gate();
    let mut queue = PreviewQueue::default();
    gate.shut();
    let first = queue.request(held_entry(&gate, 1));
    assert_eq!(queue.pending_generation(), None, "the first job started");
    // Inside the render, past its first check: a job superseded before it begins rendering
    // stops at once and would let the next one start.
    gate_until(&gate, 1, "the first render never reached its gate");
    let replaced = queue.request(held_entry(&gate, 2));
    assert_eq!(queue.pending_generation(), Some(replaced));
    let Queued {
        generation: wanted,
        replaced: displaced,
    } = queue.request_replacing(held_entry(&gate, 3));
    assert_eq!(
        displaced,
        Some(replaced),
        "the request names what it replaced"
    );
    assert_eq!(
        queue.pending_generation(),
        Some(wanted),
        "the third request replaced the second"
    );
    gate.open();
    let deadline = Instant::now() + DEADLINE;
    let mut delivered: Vec<(u64, bool)> = Vec::new();
    loop {
        if let Some(result) = queue.poll() {
            assert!(
                delivered
                    .last()
                    .is_none_or(|(last, _)| *last < result.generation),
                "deliveries must strictly increase: {delivered:?} then {}",
                result.generation
            );
            assert_eq!(result.generation, queue.last_delivered());
            assert_eq!(
                result.phase(),
                PreviewPhase::Exact,
                "no job had a proxy phase"
            );
            delivered.push((result.generation, result.cancelled()));
            if result.generation == wanted {
                assert_eq!(
                    result.into_raster().unwrap().pixel(0, 0),
                    Some([3, 0, 0, 255])
                );
                break;
            }
        }
        assert!(Instant::now() < deadline, "the newest preview never came");
        std::thread::yield_now();
    }
    assert_eq!(
        delivered,
        vec![(first, false), (wanted, false)],
        "the first job's frame, then the third's; the second was replaced in the pending slot \
         and never ran"
    );
}

/// A job that finished before a newer request superseded it still has the newest frame anybody
/// has seen, so it is delivered rather than dropped.
#[test]
fn a_superseded_job_that_already_finished_is_still_delivered() {
    let sends = Arc::new(AtomicU64::new(0));
    let counter = sends.clone();
    let mut queue = PreviewQueue::default();
    queue.set_waker(Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    }));
    let first = queue.request(entry(1));
    // The waker says the frame is in the channel, so the request below supersedes a job that
    // has already answered and cancels nothing.
    wait_until(&sends, 1, "the first job never answered");
    let second = queue.request(entry(2));
    let delivered = drain_until(&mut queue, second, PreviewPhase::Exact);
    assert_eq!(
        delivered,
        vec![
            (first, PreviewPhase::Exact, false),
            (second, PreviewPhase::Exact, false)
        ],
        "a superseded but completed frame is delivered before the newer one, and nothing was \
         cancelled"
    );
}

/// `cancel` is the only thing that invalidates an in-flight result: a frame planned before it
/// never reaches the display, even when it is the only frame there is.
#[test]
fn a_result_older_than_the_cancel_floor_is_dropped() {
    let sends = Arc::new(AtomicU64::new(0));
    let counter = sends.clone();
    let mut queue = PreviewQueue::default();
    queue.set_waker(Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    }));
    queue.request(entry(1));
    // The frame is finished and waiting in the channel: only the floor can drop it now.
    wait_until(&sends, 1, "the job never answered");
    queue.cancel();
    let deadline = Instant::now() + DEADLINE;
    while queue.is_busy() {
        assert!(queue.poll().is_none(), "a frame from before the cancel");
        assert!(Instant::now() < deadline, "the cancelled job never drained");
        std::thread::yield_now();
    }
    assert_eq!(queue.last_delivered(), 0, "nothing was ever delivered");

    // An exact phase that `cancel` stopped mid-render answers cancelled, and that outcome is
    // at the floor too: the caller that raised it already knows the generation has ended.
    queue.request(stacked(1200, 900, eligible_layers(1200, 900), None));
    queue.cancel();
    let deadline = Instant::now() + DEADLINE;
    while queue.is_busy() {
        assert!(queue.poll().is_none(), "an outcome from before the cancel");
        assert!(Instant::now() < deadline, "the cancelled job never drained");
        std::thread::yield_now();
    }
    assert_eq!(queue.last_delivered(), 0, "nothing was ever delivered");
}

/// The preview worker reduces the frame it just rendered, so a displayed target needs no second
/// render. The report must equal the reduction of that very raster, byte for byte.
#[test]
fn an_analysing_preview_returns_the_exact_reduction_of_the_frame_it_rendered() {
    let mut queue = PreviewQueue::default();
    let wanted = queue.request(job(9, true));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = queue.poll() {
            assert_eq!(result.generation, wanted);
            let raster = result.raster().expect("a frame");
            assert_eq!(
                result
                    .exact()
                    .and_then(|exact| exact.report.clone())
                    .expect("the job asked for a report"),
                crate::analysis::reduce_raster(raster).unwrap()
            );
            assert!(result.identity.has_output_stage());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the analysing preview never came"
        );
        std::thread::yield_now();
    }
    // A job that does not ask carries no report: `None` is "not asked", never empty counts.
    let wanted = queue.request(job(9, false));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = queue.poll() {
            assert_eq!(result.generation, wanted);
            assert!(
                result
                    .exact()
                    .and_then(|exact| exact.report.clone())
                    .is_none()
            );
            break;
        }
        assert!(Instant::now() < deadline, "the plain preview never came");
        std::thread::yield_now();
    }
}

// ---------------------------------------------------------------------------------------
// The proxy phase
// ---------------------------------------------------------------------------------------

/// A programmatically filled source, so a photo-shaped case costs an allocation and a fill and
/// reads no file. The fingerprint is fixed, so two sources of the same size share a proxy cache
/// identity exactly as two jobs over one prepared source do.
fn synthetic(width: u32, height: u32) -> PreviewSource {
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        pixel.copy_from_slice(&[index as u8, (index >> 5) as u8, (index >> 11) as u8, 255]);
    }
    PreviewSource::Jpeg(SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:preview-proxy-fixture".into(),
        orientation: 1,
    })
}

/// The stack the proxy design calls eligible: the one orientation layer, one colour-stage Basic
/// layer and a 7 degree straightening crop fitted onto the turned stage.
fn eligible_layers(width: u32, height: u32) -> Vec<Layer> {
    let stage = CropStage {
        width: height,
        height: width,
        angle: 7.0,
    };
    // The whole rotated box fitted about the centre: what a crop-fit commits. It touches the
    // rotated stage exactly, and re-rounding it at the proxy size is what the crop module's
    // covered rectangle exists for, so this stack proves the proxy phase on a real fitted crop.
    let (box_width, box_height) = stage.bounding_box();
    let fitted = stage.fit_about_center(BoxRect {
        x: 0.0,
        y: 0.0,
        width: box_width,
        height: box_height,
    });
    vec![
        Layer::orientation(Orientation::of(Transform::RotateRight)),
        Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure": 0.5, "contrast": 20.0}),
            mask: None,
            artifacts: Vec::new(),
        },
        Layer::crop(fitted.normalized(&stage)),
    ]
}

fn bounds(width: u32, height: u32) -> ProxyBounds {
    ProxyBounds { width, height }
}

/// A job over a synthetic source with an explicit stack, built without the catalog: the queue
/// is what is under test, not how a job is planned.
fn stacked(width: u32, height: u32, layers: Vec<Layer>, proxy: Option<ProxyBounds>) -> PreviewJob {
    stacked_with_masks(width, height, layers, Vec::new(), proxy)
}

/// [`stacked`] over a stack that carries a mask table, which is where a masked layer's `mask`
/// reference is resolved.
fn stacked_with_masks(
    width: u32,
    height: u32,
    layers: Vec<Layer>,
    masks: Vec<Mask>,
    proxy: Option<ProxyBounds>,
) -> PreviewJob {
    let asset = AssetId::new();
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks,
        ..Recipe::default()
    };
    let snapshot = Snapshot {
        id: SnapshotId::new(),
        asset_id: asset.clone(),
        recipe: recipe.clone(),
    };
    let source = synthetic(width, height);
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence: 1,
        action_id: "test".into(),
        label: "Test".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 0,
        snapshot,
        undo_parent: None,
        restore_target: None,
    };
    let identity = AnalysisIdentity::of(
        &asset,
        source.fingerprint(),
        &entry,
        &recipe,
        None,
        Some((width, height)),
    )
    .unwrap();
    PreviewJob {
        source,
        registry: Arc::new(ModuleRegistry::builtin()),
        context: crate::render::testing::context().clone(),
        recipe,
        layer_count: None,
        draft_revision: None,
        identity,
        analyse: false,
        proxy,
        mask_overlay: None,
        entry,
    }
}

fn wait_until(counter: &Arc<AtomicU64>, wanted: u64, what: &str) {
    let deadline = Instant::now() + DEADLINE;
    while counter.load(Ordering::Relaxed) < wanted {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::yield_now();
    }
}

/// Every result the active job has left to send, in order. The queue releases the active slot
/// when the exact phase lands, so an idle queue with no pending job is the end of the job.
fn drain_all(queue: &mut PreviewQueue) -> Vec<PreviewResult> {
    let deadline = Instant::now() + DEADLINE;
    let mut results = Vec::new();
    loop {
        if let Some(result) = queue.poll() {
            results.push(result);
        }
        if !queue.is_busy() {
            return results;
        }
        assert!(
            Instant::now() < deadline,
            "the preview worker never finished"
        );
        std::thread::yield_now();
    }
}

/// Poll until this generation's phase is delivered, collecting what came before it: each
/// delivery's generation, phase and whether it was cancelled.
fn drain_until(
    queue: &mut PreviewQueue,
    generation: u64,
    phase: PreviewPhase,
) -> Vec<(u64, PreviewPhase, bool)> {
    let deadline = Instant::now() + DEADLINE;
    let mut delivered = Vec::new();
    loop {
        if let Some(result) = queue.poll() {
            delivered.push((result.generation, result.phase(), result.cancelled()));
            if (result.generation, result.phase()) == (generation, phase) {
                return delivered;
            }
        }
        assert!(
            Instant::now() < deadline,
            "generation {generation} {phase:?} never arrived: {delivered:?}"
        );
        std::thread::yield_now();
    }
}

/// A job with display bounds smaller than its stage produces two frames under one generation:
/// the proxy first, then the exact one. Each is byte for byte the render this test computes
/// independently — the proxy against the exact downscale of the source, the exact one against
/// the prepared source itself.
#[test]
fn a_job_with_bounds_yields_the_proxy_phase_then_the_exact_phase() {
    let display = bounds(40, 40);
    let job = stacked(64, 48, eligible_layers(64, 48), Some(display));
    let registry = job.registry.clone();
    let source = job.source.clone();
    let recipe = job.recipe.clone();
    let snapshot = job.entry.snapshot.id.clone();
    let plan = source
        .proxy_plan(&registry, &recipe, display)
        .expect("a plan")
        .expect("a proxy is worthwhile");

    let mut queue = PreviewQueue::default();
    let requested = Instant::now();
    let generation = queue.request(job);
    let results = drain_all(&mut queue);
    let lifetime_ms = requested.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
    let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();

    // Each phase reports its own worker time: finite, and inside the job's own lifetime. The
    // two clocks run one after the other on the worker, so together they fit inside it too —
    // neither phase counts the other, and neither counts anything before the request.
    for (phase, ms) in [("proxy", proxy.render_ms), ("exact", exact.render_ms)] {
        assert!(
            ms.is_finite() && ms >= 0.0 && ms <= lifetime_ms,
            "the {phase} phase reports {ms} ms of a {lifetime_ms} ms job"
        );
    }
    assert!(
        proxy.render_ms + exact.render_ms <= lifetime_ms,
        "the phases overlap: {} + {} ms of a {lifetime_ms} ms job",
        proxy.render_ms,
        exact.render_ms
    );

    assert_eq!(proxy.generation, generation);
    assert_eq!(exact.generation, generation);
    assert_eq!(proxy.phase(), PreviewPhase::Proxy);
    assert_eq!(exact.phase(), PreviewPhase::Exact);
    assert_eq!(
        proxy.proxy().map(|proxy| proxy.dimensions),
        Some((plan.width, plan.height))
    );
    assert_eq!(exact.proxy().map(|proxy| proxy.dimensions), None);
    assert_eq!(
        exact.exact().and_then(|exact| exact.proxy_declined.clone()),
        None,
        "the proxy phase ran"
    );
    assert!(
        proxy.proxy().is_some_and(|proxy| proxy.built),
        "nothing was cached before this job"
    );
    // A proxy raster is never reduced, whatever the job asked for.
    assert!(
        proxy
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_none()
    );

    let reference = source
        .proxy(plan)
        .expect("the exact downscale")
        .render(&registry, snapshot.clone(), &recipe)
        .expect("the recipe renders at proxy size");
    let frame = proxy.into_raster().expect("a proxy frame");
    assert_eq!(
        (frame.width, frame.height),
        (reference.width, reference.height)
    );
    assert_eq!(
        frame.rgba.as_ref(),
        reference.rgba.as_ref(),
        "the proxy frame is the exact recipe over the exact downscale"
    );
    assert!(
        frame.width <= display.width && frame.height <= display.height,
        "the proxy frame fits the display bounds"
    );

    let proxy_size = (frame.width, frame.height);
    let reference = source
        .render(&registry, snapshot, &recipe)
        .expect("the exact render");
    let frame = exact.into_raster().expect("an exact frame");
    assert_eq!(
        (frame.width, frame.height),
        (reference.width, reference.height)
    );
    assert_eq!(frame.rgba.as_ref(), reference.rgba.as_ref());
    assert!(
        frame.width > proxy_size.0 && frame.height > proxy_size.1,
        "the exact phase renders the prepared source, not the proxy: {:?} against {proxy_size:?}",
        (frame.width, frame.height)
    );
}

/// A job compiles its stack once at each stage it renders at: once at the exact stage, whose
/// compilation plans the proxy, renders the exact frame and gives the coverage grid its
/// geometry, and once at the proxy stage, whose compilation renders the proxy frame and says
/// whether it is approximate. A job without a proxy phase compiles once.
#[test]
fn a_preview_job_compiles_its_stack_once_per_stage_it_renders_at() {
    let mask = gradient_mask(0.5);
    let request = MaskOverlayRequest {
        mask: mask.id.clone(),
        component: None,
        cells_w: 8,
        cells_h: 6,
    };
    let mut queue = PreviewQueue::default();
    for (proxy, phases, compiles) in [(Some(bounds(40, 40)), 2, 2), (None, 1, 1)] {
        let mut job = stacked_with_masks(64, 48, masked_basic(&mask), vec![mask.clone()], proxy)
            .with_mask_overlay(request.clone())
            .expect("the stack holds the mask");
        let context = RenderContext::new();
        job.context = context.clone();
        queue.request(job);
        let results = drain_all(&mut queue);
        assert_eq!(results.len(), phases, "{proxy:?}");
        let exact = results.last().expect("an exact phase");
        assert!(
            exact.raster().is_ok()
                && exact
                    .exact()
                    .and_then(|exact| exact.mask_overlay.clone())
                    .is_some(),
            "{proxy:?}"
        );
        assert_eq!(context.compiles(), compiles, "{proxy:?}");
    }
}

/// A mask whose narrowest feature spans `length x stage.height` pixels. The gradient runs down
/// the frame, so its ramp is `length` mask-space units — the one number the thin-feature rule
/// reads.
fn gradient_mask(length: f64) -> Mask {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("linear");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.5, "y0": 0.5 - length / 2.0, "x1": 0.5, "y1": 0.5 + length / 2.0}),
    ));
    mask
}

/// One Basic layer bound to `mask`, which is the masked colour stack every assertion below
/// renders. No geometry, so the stage a proxy is fitted into is the source itself.
fn masked_basic(mask: &Mask) -> Vec<Layer> {
    vec![Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"exposure": 0.8, "contrast": 25.0}),
        mask: Some(mask.id.clone()),
        artifacts: Vec::new(),
    }]
}

/// The same stack without its mask reference, for the comparisons that have to show the mask
/// is doing something.
fn without_masks(recipe: &Recipe) -> Recipe {
    Recipe {
        format: recipe.format,
        masks: Vec::new(),
        layers: recipe
            .layers
            .iter()
            .map(|layer| Layer {
                mask: None,
                ..layer.clone()
            })
            .collect(),
        ..Recipe::default()
    }
}

/// The delivered proxy contract over a **masked** stack: the recipe is proxy eligible, the job
/// yields both phases, and the proxy frame is byte for byte the exact recipe rendered against
/// the exact downscale of the source.
///
/// This is the assertion
/// [`a_job_with_bounds_yields_the_proxy_phase_then_the_exact_phase`] makes, over a stack whose
/// colour layer is modulated by a mask. It holds because a mask's geometry is stored
/// normalized: the mask compiled against the proxy stage is the same field at a smaller scale,
/// so nothing about the equation changed, and the only sampling question — whether the proxy's
/// pixel grid resolves the mask's narrowest feature — is answered yes here, at nine proxy
/// pixels of ramp.
#[test]
fn a_masked_recipe_is_proxy_eligible_and_its_proxy_frame_is_the_exact_recipe_at_proxy_size() {
    let display = bounds(40, 40);
    let mask = gradient_mask(0.3);
    let job = stacked_with_masks(
        64,
        48,
        masked_basic(&mask),
        vec![mask.clone()],
        Some(display),
    );
    let registry = job.registry.clone();
    let source = job.source.clone();
    let recipe = job.recipe.clone();
    let snapshot = job.entry.snapshot.id.clone();
    registry
        .proxy_eligible(&recipe)
        .expect("a masked colour stack is proxy eligible");
    let plan = source
        .proxy_plan(&registry, &recipe, display)
        .expect("a plan")
        .expect("a proxy is worthwhile");
    assert!(
        0.3 * f64::from(plan.height) >= 2.0,
        "this mask's ramp must be at least two proxy pixels for the equality to be claimed"
    );

    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    let results = drain_all(&mut queue);
    assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
    let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();
    assert_eq!(
        (proxy.generation, proxy.phase()),
        (generation, PreviewPhase::Proxy)
    );
    assert_eq!(
        (exact.generation, exact.phase()),
        (generation, PreviewPhase::Exact)
    );
    assert_eq!(
        exact.exact().and_then(|exact| exact.proxy_declined.clone()),
        None,
        "the proxy phase ran"
    );
    assert_eq!(
        proxy.proxy().map(|proxy| proxy.dimensions),
        Some((plan.width, plan.height))
    );
    assert!(
        !proxy.proxy_approximate(),
        "a mask the proxy grid resolves is not an approximation: {:?}",
        proxy
            .proxy()
            .map(|proxy| proxy.approximation)
            .unwrap_or_default()
    );
    assert_eq!(
        proxy
            .proxy()
            .map(|proxy| proxy.approximation)
            .unwrap_or_default()
            .reason(),
        None
    );

    let reference = source
        .proxy(plan)
        .expect("the exact downscale")
        .render(&registry, snapshot.clone(), &recipe)
        .expect("the masked recipe renders at proxy size");
    let frame = proxy.into_raster().expect("a proxy frame");
    assert_eq!(
        (frame.width, frame.height),
        (reference.width, reference.height)
    );
    assert_eq!(
        frame.rgba.as_ref(),
        reference.rgba.as_ref(),
        "the masked proxy frame is the exact recipe over the exact downscale"
    );
    // The mask did something: the same units applied everywhere are a different picture, so the
    // equality above is not the equality of two unmasked renders.
    let global = source
        .proxy(plan)
        .expect("the exact downscale")
        .render(&registry, snapshot.clone(), &without_masks(&recipe))
        .expect("the unmasked recipe renders at proxy size");
    assert_ne!(
        frame.rgba.as_ref(),
        global.rgba.as_ref(),
        "the mask must modulate the frame, or this proves nothing about masks"
    );

    let reference = source
        .render(&registry, snapshot, &recipe)
        .expect("the exact render");
    let frame = exact.into_raster().expect("an exact frame");
    assert_eq!(frame.rgba.as_ref(), reference.rgba.as_ref());
}

#[test]
fn queue_timing_is_opt_in_and_survives_the_worker_result() {
    let mut ordinary = PreviewQueue::default();
    ordinary.request(stacked(
        64,
        48,
        eligible_layers(64, 48),
        Some(bounds(16, 16)),
    ));
    let ordinary_results = drain_all(&mut ordinary);
    assert!(
        ordinary_results
            .iter()
            .all(|result| result.queue_wait_ms.is_none())
    );

    let mut measured = PreviewQueue::default();
    let (queued, requested_at) = measured.request_timed(stacked(
        64,
        48,
        eligible_layers(64, 48),
        Some(bounds(16, 16)),
    ));
    assert_eq!(queued.generation, 1);
    assert!(requested_at <= Instant::now());
    let measured_results = drain_all(&mut measured);
    assert!(
        measured_results
            .iter()
            .all(|result| result.queue_wait_ms.is_some_and(|ms| ms >= 0.0))
    );
}

/// The thin-feature rule, on both sides of its threshold, over the same stack and the same
/// bounds: only the mask's ramp changes.
///
/// Above two proxy pixels the frame is the exact recipe at proxy size and reports no
/// approximation. Below it the **mask field** is evaluated with a 2 x 2 supersample per pixel —
/// the effect is not — so the frame is no longer the point-sampled render, and it says so with
/// the word the spatial layer already uses.
#[test]
fn a_mask_thinner_than_two_proxy_pixels_is_supersampled_and_reported_approximate() {
    let display = bounds(40, 40);
    // A 40x30 proxy of a 64x48 source: 0.3 x 30 = 9 px of ramp resolves, 0.05 x 30 = 1.5 px
    // does not — and 0.05 x 48 = 2.4 px still resolves at full resolution, so the rule is about
    // the grid the frame is sampled on and not about the mask alone.
    let resolvable = gradient_mask(0.3);
    let thin = gradient_mask(0.05);

    let frame_of = |mask: &Mask| -> PreviewResult {
        let job = stacked_with_masks(
            64,
            48,
            masked_basic(mask),
            vec![mask.clone()],
            Some(display),
        );
        let mut queue = PreviewQueue::default();
        queue.request(job);
        let mut results = drain_all(&mut queue);
        assert_eq!(results.len(), 2);
        results.remove(0)
    };

    let coarse = frame_of(&resolvable);
    assert!(!coarse.proxy_approximate());
    assert!(
        !coarse
            .proxy()
            .map(|proxy| proxy.approximation)
            .unwrap_or_default()
            .mask
    );

    let fine = frame_of(&thin);
    assert!(
        fine.proxy_approximate(),
        "a ramp of 1.5 proxy pixels is below the threshold"
    );
    assert!(
        fine.proxy()
            .map(|proxy| proxy.approximation)
            .unwrap_or_default()
            .mask
    );
    assert!(
        !fine
            .proxy()
            .map(|proxy| proxy.approximation)
            .unwrap_or_default()
            .spatial,
        "there is no spatial layer in this stack"
    );
    let reason = fine
        .proxy()
        .map(|proxy| proxy.approximation)
        .unwrap_or_default()
        .reason()
        .expect("a reason");
    assert!(
        reason.contains("narrower than two proxy pixels"),
        "{reason}"
    );
    assert!(reason.contains("supersample"), "{reason}");

    // The supersample changes the picture it is applied to, which is why it is reported: the
    // point-sampled render of the same stack at the same size is a different frame.
    let source = synthetic(64, 48);
    let registry = ModuleRegistry::builtin();
    let recipe = Recipe {
        format: RECIPE_FORMAT,
        layers: masked_basic(&thin),
        masks: vec![thin.clone()],
        ..Recipe::default()
    };
    let plan = source
        .proxy_plan(&registry, &recipe, display)
        .unwrap()
        .unwrap();
    let point_sampled = source
        .proxy(plan)
        .unwrap()
        .render(&registry, SnapshotId::new(), &recipe)
        .unwrap();
    assert_ne!(
        fine.into_raster().expect("a proxy frame").rgba.as_ref(),
        point_sampled.rgba.as_ref(),
        "the thin mask was supersampled, so its frame differs from the point-sampled one"
    );
}

/// A mask with no components draws no feature at all, so `min_feature_px` answers
/// `f32::INFINITY` and the comparison against two pixels reads it correctly: the supersample
/// path is not tripped and the frame reports no approximation.
#[test]
fn a_mask_with_no_components_reports_no_approximation() {
    let display = bounds(40, 40);
    let empty = Mask::new("Mask 1");
    let job = stacked_with_masks(
        64,
        48,
        masked_basic(&empty),
        vec![empty.clone()],
        Some(display),
    );
    let registry = job.registry.clone();
    let recipe = job.recipe.clone();
    let mut queue = PreviewQueue::default();
    queue.request(job);
    let results = drain_all(&mut queue);
    assert_eq!(results.len(), 2);
    let proxy = &results[0];
    assert_eq!(proxy.phase(), PreviewPhase::Proxy);
    assert!(!proxy.proxy_approximate());
    assert!(
        !proxy
            .proxy()
            .map(|proxy| proxy.approximation)
            .unwrap_or_default()
            .mask
    );
    assert_eq!(registry.proxy_approximation(&recipe, 40, 30).reason(), None);
}

/// Both reasons at once: a spatial layer and a thin mask in one stack. The frame is approximate
/// for two separate reasons and names both, so a person can tell which is which.
#[test]
fn a_spatial_layer_and_a_thin_mask_are_reported_separately() {
    let registry = ModuleRegistry::builtin();
    let thin = gradient_mask(0.05);
    let spatial_and_mask = Recipe {
        format: RECIPE_FORMAT,
        layers: masked_basic(&thin)
            .into_iter()
            .chain([Layer {
                id: LayerId::new(),
                effect_id: crate::PRESENCE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"texture": 40.0}),
                mask: None,
                artifacts: Vec::new(),
            }])
            .collect(),
        masks: vec![thin.clone()],
        ..Recipe::default()
    };
    let both = registry.proxy_approximation(&spatial_and_mask, 40, 30);
    assert!(both.spatial && both.mask);
    let reason = both.reason().expect("a reason");
    assert!(reason.contains("neighbourhoods"), "{reason}");
    assert!(
        reason.contains("narrower than two proxy pixels"),
        "{reason}"
    );

    // The spatial layer alone still says only what it is.
    let only = registry.proxy_approximation(&without_masks(&spatial_and_mask), 40, 30);
    assert!(only.spatial && !only.mask);
    let reason = only.reason().expect("a reason");
    assert!(
        !reason.contains("narrower than two proxy pixels"),
        "{reason}"
    );
}

/// A newer request stops the exact phase of the job it replaced within a chunk, and that phase
/// answers with no frame at all, delivered under its own generation before anything of the
/// newer job. The proxy phase of the older job is polled first, so the cancel lands inside the
/// exact render rather than before it.
#[test]
fn a_newer_request_cancels_the_exact_phase_of_the_job_it_replaced() {
    let display = bounds(200, 200);
    let mut queue = PreviewQueue::default();
    let older = queue.request(stacked(
        1200,
        900,
        eligible_layers(1200, 900),
        Some(display),
    ));
    let first = drain_until(&mut queue, older, PreviewPhase::Proxy);
    assert_eq!(first, vec![(older, PreviewPhase::Proxy, false)]);

    let newer = queue.request(stacked(
        1200,
        900,
        eligible_layers(1200, 900),
        Some(display),
    ));
    let delivered = drain_until(&mut queue, newer, PreviewPhase::Exact);
    let order: Vec<(u64, PreviewPhase)> = delivered
        .iter()
        .map(|(generation, phase, _)| (*generation, *phase))
        .collect();
    assert_eq!(
        order,
        vec![
            (older, PreviewPhase::Exact),
            (newer, PreviewPhase::Proxy),
            (newer, PreviewPhase::Exact)
        ],
        "the older job's one exact outcome, then the newer job's two phases"
    );
    // The exact phase of the older job either finished before the cancel reached it — which is
    // vanishingly unlikely on a frame this size but is not forbidden — or it was cancelled. The
    // newer job's phases are frames either way.
    assert!(
        delivered[1..].iter().all(|(_, _, cancelled)| !cancelled),
        "{delivered:?}"
    );
}

/// The three ways a job that offered bounds has no proxy phase, and the one way a job never
/// offered them. Each yields exactly one exact frame, and each says why.
#[test]
fn a_job_without_a_proxy_phase_yields_one_exact_result_and_says_why() {
    let mut queue = PreviewQueue::default();

    let only = |queue: &mut PreviewQueue, job: PreviewJob| -> PreviewResult {
        queue.request(job);
        let mut results = drain_all(queue);
        assert_eq!(results.len(), 1, "one exact frame and nothing else");
        let result = results.pop().unwrap();
        assert_eq!(result.phase(), PreviewPhase::Exact);
        assert!(result.raster().is_ok(), "the exact path runs unchanged");
        result
    };

    // No bounds at all: the exact path, and nothing to decline.
    let result = only(&mut queue, stacked(64, 48, eligible_layers(64, 48), None));
    assert_eq!(
        result
            .exact()
            .and_then(|exact| exact.proxy_declined.clone()),
        None
    );

    // An ineligible stack: the layer that made it so is named.
    let pixel = vec![Layer::pixel(0, 0, [9, 9, 9])];
    let result = only(&mut queue, stacked(64, 48, pixel, Some(bounds(8, 8))));
    let reason = result
        .exact()
        .and_then(|exact| exact.proxy_declined.clone())
        .expect("a reason");
    assert!(reason.contains(PIXEL_EFFECT), "{reason}");
    assert!(reason.contains("layer 0"), "{reason}");

    // Bounds the stage already fits: there is no proxy smaller than the source to build.
    let large = stacked(64, 48, eligible_layers(64, 48), Some(bounds(4000, 4000)));
    let result = only(&mut queue, large);
    let reason = result
        .exact()
        .and_then(|exact| exact.proxy_declined.clone())
        .expect("a reason");
    assert!(reason.contains("scale is 1"), "{reason}");

    // A truncated job renders a layer prefix, which has no proxy phase. It is never analysed
    // either: the owner refuses to plan a job that is both truncated and analysing, because the
    // prefix is not the stack the job's identity describes.
    let mut truncated = stacked(64, 48, eligible_layers(64, 48), Some(bounds(8, 8)));
    truncated.layer_count = Some(1);
    let result = only(&mut queue, truncated);
    let reason = result
        .exact()
        .and_then(|exact| exact.proxy_declined.clone())
        .expect("a reason");
    assert!(reason.contains("truncated"), "{reason}");
    assert!(
        result
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_none(),
        "a truncated job carries no report"
    );
}

/// The proxy source is built once and held by the queue: the next job at the same identity and
/// the same bounds renders against the source already in hand.
#[test]
fn two_jobs_at_the_same_bounds_build_the_proxy_once() {
    let display = bounds(32, 32);
    let mut queue = PreviewQueue::default();

    queue.request(stacked(64, 48, eligible_layers(64, 48), Some(display)));
    let first = drain_all(&mut queue);
    assert!(
        first[0].proxy().is_some_and(|proxy| proxy.built),
        "the first job builds the proxy"
    );

    queue.request(stacked(64, 48, eligible_layers(64, 48), Some(display)));
    let second = drain_all(&mut queue);
    assert_eq!(second[0].phase(), PreviewPhase::Proxy);
    assert!(
        !second[0].proxy().is_some_and(|proxy| proxy.built),
        "the second job reuses the cached one"
    );
    assert_eq!(
        second[0].proxy().map(|proxy| proxy.dimensions),
        first[0].proxy().map(|proxy| proxy.dimensions)
    );
    // Same source, same plan, same picture: the cached proxy is the built one.
    let cached = second[0].raster().expect("a proxy frame");
    let built = first[0].raster().expect("a proxy frame");
    assert_eq!(cached.rgba.as_ref(), built.rgba.as_ref());

    // Different bounds are a different plan and a miss, which is what a window resize is.
    queue.request(stacked(
        64,
        48,
        eligible_layers(64, 48),
        Some(bounds(24, 24)),
    ));
    let resized = drain_all(&mut queue);
    assert!(
        resized[0].proxy().is_some_and(|proxy| proxy.built),
        "a resized window rebuilds the proxy"
    );
}

/// The waker is what replaces the preview poll timer: one call per result sent, on the worker
/// thread, and never on the owner thread.
#[test]
fn the_waker_is_called_once_per_result() {
    let calls = Arc::new(AtomicU64::new(0));
    let counter = calls.clone();
    let owner = std::thread::current().id();
    let mut queue = PreviewQueue::default();
    queue.set_waker(Arc::new(move || {
        assert_ne!(
            std::thread::current().id(),
            owner,
            "the waker runs on the preview worker, never on the thread that asked"
        );
        counter.fetch_add(1, Ordering::Relaxed);
    }));

    queue.request(stacked(
        64,
        48,
        eligible_layers(64, 48),
        Some(bounds(32, 32)),
    ));
    assert_eq!(drain_all(&mut queue).len(), 2);
    wait_until(&calls, 2, "the two-phase job woke twice");

    queue.request(stacked(64, 48, eligible_layers(64, 48), None));
    assert_eq!(drain_all(&mut queue).len(), 1);
    wait_until(&calls, 3, "the exact-only job woke once");
    // A generous wait proves no extra call follows the last result.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::Relaxed), 3);
}

// ---------------------------------------------------------------------------------------
// The activity each job publishes
// ---------------------------------------------------------------------------------------

/// A job whose one colour layer waits on `gate` in every phase that renders it, so a test can
/// hold the job in the phase it is about. The layer leaves its pixels as it found them.
fn held(gate: &Arc<crate::modules::RenderGate>, proxy: Option<ProxyBounds>) -> PreviewJob {
    let layer = Layer {
        id: LayerId::new(),
        effect_id: crate::modules::HELD_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({}),
        artifacts: Vec::new(),
        mask: None,
    };
    let mut job = stacked(64, 48, vec![layer], proxy);
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(crate::modules::HeldModule::shared(gate.clone()))
        .expect("a valid holding module");
    job.registry = Arc::new(registry);
    job
}

/// Read the board until `wanted` holds. The job under test is held at a gate, so what it waits
/// for is the worker reaching that gate, never a race with how fast the machine renders.
fn board_until(
    board: &ActivityBoard,
    wanted: impl Fn(&crate::ActivitySnapshot) -> bool,
    what: &str,
) -> crate::ActivitySnapshot {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let snapshot = board.snapshot();
        if wanted(&snapshot) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "{what}: {snapshot:?}");
        std::thread::yield_now();
    }
}

/// A job with a proxy phase is listed in `proxy` while that phase runs and ends in `exact`, and
/// its entry has already ended when the queue releases the job. The board keeps every finished
/// entry here, because its recent threshold is zero.
#[test]
fn a_jobs_activity_moves_from_proxy_to_exact_and_ends_when_the_queue_releases_it() {
    let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
    let gate = crate::modules::RenderGate::open_gate();
    let mut queue = PreviewQueue::default();
    queue.set_activity(board.clone());
    let job = held(&gate, Some(bounds(16, 16)));
    let asset = job.entry.asset_id.clone();

    gate.shut();
    queue.request(job);
    let running = board_until(
        &board,
        |snapshot| {
            snapshot
                .active
                .first()
                .is_some_and(|active| active.entry.phase == Some("proxy"))
        },
        "the proxy phase never reached its gate",
    );
    assert_eq!(running.active.len(), 1);
    let entry = &running.active[0].entry;
    assert_eq!(
        (entry.kind, entry.label),
        ("preview.render", "Rendering preview")
    );
    assert_eq!(entry.asset_id.as_ref(), Some(&asset));
    assert_eq!((&entry.detail, &entry.job_id), (&None, &None));
    assert!(running.recent.is_empty());

    gate.open();
    let results = drain_all(&mut queue);
    assert_eq!(
        results
            .iter()
            .map(|result| result.phase())
            .collect::<Vec<_>>(),
        [PreviewPhase::Proxy, PreviewPhase::Exact]
    );
    // The queue released the job the moment its exact result arrived, and the entry had
    // already ended by then: nothing here waits for the worker again.
    let ended = board.snapshot();
    assert!(ended.active.is_empty(), "{ended:?}");
    assert_eq!(ended.recent.len(), 1);
    let recent = &ended.recent[0];
    assert_eq!(recent.entry.kind, "preview.render");
    assert_eq!(
        recent.entry.phase,
        Some("exact"),
        "the job moved on to its exact phase"
    );
    assert_eq!(recent.outcome, crate::activity::Outcome::Completed);
}

/// A newer request stops the exact phase of the job it replaces, and that job's activity ends
/// cancelled while the newer one completes. The older job is held at its gate inside the first
/// 16-row chunk of its colour pass, so the stop reaches the check before its second chunk.
#[test]
fn a_superseded_jobs_activity_ends_cancelled() {
    let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
    let gate = crate::modules::RenderGate::open_gate();
    let mut queue = PreviewQueue::default();
    queue.set_activity(board.clone());

    gate.shut();
    let first = queue.request(held(&gate, None));
    let running = board_until(
        &board,
        |snapshot| {
            snapshot
                .active
                .first()
                .is_some_and(|active| active.entry.phase == Some("exact"))
        },
        "the exact phase never reached its gate",
    );
    let older = running.active[0].entry.id;
    let newer = queue.request(held(&gate, None));
    gate.open();
    let delivered = drain_until(&mut queue, newer, PreviewPhase::Exact);
    assert_eq!(
        delivered,
        vec![
            (first, PreviewPhase::Exact, true),
            (newer, PreviewPhase::Exact, false)
        ],
        "the older exact phase stopped, and its cancelled outcome came first"
    );

    let ended = board.snapshot();
    assert!(ended.active.is_empty(), "{ended:?}");
    let outcomes: Vec<(u64, crate::activity::Outcome)> = ended
        .recent
        .iter()
        .map(|recent| (recent.entry.id, recent.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            (older + 1, crate::activity::Outcome::Completed),
            (older, crate::activity::Outcome::Cancelled),
        ],
        "newest first: the job that replaced it completed"
    );
}

/// Wait until the job held at `gate` has reached it at least `rows` times.
fn gate_until(gate: &crate::modules::RenderGate, rows: u64, what: &str) {
    let deadline = Instant::now() + DEADLINE;
    while gate.reached() < rows {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::yield_now();
    }
}

/// The two-phase rule under the persistent worker: a newer request arrives while the older
/// job's proxy render is held at its gate. The proxy phase is not interrupted — its frame is
/// still newer than anything on screen — and is delivered as a frame; the exact phase behind it
/// was superseded before it began, so it answers cancelled; and the newer job then runs both of
/// its phases.
#[test]
fn a_superseded_jobs_exact_phase_is_cancelled_but_its_proxy_is_not() {
    let gate = crate::modules::RenderGate::open_gate();
    let mut queue = PreviewQueue::default();
    gate.shut();
    let older = queue.request(held(&gate, Some(bounds(16, 16))));
    gate_until(&gate, 1, "the proxy render never reached its gate");
    let newer = queue.request(held(&gate, Some(bounds(16, 16))));
    gate.open();
    let delivered = drain_until(&mut queue, newer, PreviewPhase::Exact);
    assert_eq!(
        delivered,
        vec![
            (older, PreviewPhase::Proxy, false),
            (older, PreviewPhase::Exact, true),
            (newer, PreviewPhase::Proxy, false),
            (newer, PreviewPhase::Exact, false),
        ],
        "the superseded job's proxy frame, its cancelled exact phase, then the newer job"
    );
}

/// The worker takes the pending job itself when the active one ends: nothing here polls, and
/// the pending job still runs to the end while the first job's outcome waits undelivered.
#[test]
fn the_next_job_starts_without_a_poll() {
    let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
    let gate = crate::modules::RenderGate::open_gate();
    let mut queue = PreviewQueue::default();
    queue.set_activity(board.clone());
    gate.shut();
    let first = queue.request(held(&gate, None));
    let second = queue.request(held(&gate, None));
    assert_eq!(queue.pending_generation(), Some(second));
    gate.open();
    let ended = board_until(
        &board,
        |snapshot| snapshot.active.is_empty() && snapshot.recent.len() == 2,
        "the pending job never ran without a poll",
    );
    assert_eq!(
        ended
            .recent
            .iter()
            .map(|recent| recent.outcome)
            .collect::<Vec<_>>(),
        [
            crate::activity::Outcome::Completed,
            crate::activity::Outcome::Cancelled
        ],
        "newest first: the pending job completed, the first was superseded"
    );
    assert_eq!(queue.pending_generation(), None);
    assert_eq!(queue.last_delivered(), 0, "nothing was polled");
    assert!(queue.ready());
    let delivered = drain_until(&mut queue, second, PreviewPhase::Exact);
    assert_eq!(
        delivered,
        vec![
            (first, PreviewPhase::Exact, true),
            (second, PreviewPhase::Exact, false)
        ]
    );
}

#[test]
fn history_selection_and_view_are_read_only_validated_session_state() {
    let mut session = PreviewSession::default();
    let entry = EntryId::new();
    session.select(HistorySelection::Entry(entry.clone()));
    assert!(!session.can_edit());
    session
        .view
        .set_zoom(Zoom::Percent { value: 100.0 })
        .unwrap();
    assert!(session.view.source_detail_required());
    assert!(
        session
            .view
            .set_zoom(Zoom::Percent { value: f32::NAN })
            .is_err()
    );
    session.return_current();
    assert!(session.can_edit());
    assert_eq!(session.selection, HistorySelection::Current);
}

/// A RAW job over planes developed at one white balance, rendering them at `white_balance`, at
/// display bounds that give it a proxy phase, asking for a report.
fn raw_job(white_balance: Option<crate::WhiteBalanceApproximation>) -> PreviewJob {
    use crate::{LinearImage, LinearSettings};
    let (width, height) = (240, 160);
    let planes: Vec<f32> = (0..3 * width * height)
        .map(|index| 0.02 + ((index * 37) % 1009) as f32 / 1100.0)
        .collect();
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:raw-wb").unwrap();
    let mut job = job(1, true);
    // The stock test job carries a pixel-stage layer, which is not proxy-eligible.
    job.recipe.layers.clear();
    job.source = PreviewSource::Raw {
        image,
        settings: LinearSettings {
            exposure_ev: 0.25,
            white_balance,
        },
    };
    job.proxy = Some(ProxyBounds {
        width: 60,
        height: 60,
    });
    job
}

fn approximation() -> crate::WhiteBalanceApproximation {
    crate::WhiteBalanceApproximation::from_matrix([
        [1.35, 0.08, -0.04],
        [0.03, 0.98, 0.02],
        [-0.06, 0.04, 0.71],
    ])
    .unwrap()
}

/// A job whose source approximates its white balance says so on both of its phases and is
/// never reduced into a report, although it asked for one. Otherwise it is an ordinary job:
/// the proxy phase is the approximate recipe rendered against the exact downscale of the
/// developed planes, byte for byte, and the exact phase the approximate recipe at full size.
#[test]
fn an_approximate_white_balance_is_labelled_on_both_phases_and_never_analysed() {
    let job = raw_job(Some(approximation()));
    assert!(job.analyse, "the job asked for a report");
    assert!(job.source.approximate_white_balance());
    let (registry, source, recipe) = (job.registry.clone(), job.source.clone(), job.recipe.clone());
    let snapshot = job.entry.snapshot.id.clone();
    let plan = source
        .proxy_plan(&registry, &recipe, job.proxy.unwrap())
        .unwrap()
        .expect("a proxy is worthwhile");

    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    let results = drain_all(&mut queue);
    assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
    let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();
    assert_eq!(
        (proxy.generation, exact.generation),
        (generation, generation)
    );
    assert_eq!(
        (proxy.phase(), exact.phase()),
        (PreviewPhase::Proxy, PreviewPhase::Exact)
    );
    assert!(proxy.approximate_white_balance && exact.approximate_white_balance);
    assert!(
        proxy
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_none()
    );
    assert!(
        exact
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_none(),
        "an approximate frame is never reduced, whatever the job asked"
    );

    let reference = source
        .proxy(plan)
        .expect("the exact downscale")
        .render(&registry, snapshot.clone(), &recipe)
        .expect("the approximate recipe at proxy size");
    assert_eq!(
        proxy.into_raster().expect("a proxy frame").rgba.as_ref(),
        reference.rgba.as_ref(),
        "the proxy frame is the approximate recipe over the exact downscale"
    );
    let reference = source
        .render(&registry, snapshot, &recipe)
        .expect("the approximate recipe at full size");
    assert_eq!(
        exact.into_raster().expect("an exact frame").rgba.as_ref(),
        reference.rgba.as_ref()
    );
}

/// The proxy cache keys on the developed planes and takes the settings from the job, so a
/// drafted white balance renders against the proxy the committed frame built — a cache hit —
/// through its own matrix, and says so.
#[test]
fn a_drafted_white_balance_hits_the_proxy_the_exact_job_built() {
    let exact = raw_job(None);
    let mut drafted = raw_job(Some(approximation()));
    // The same developed planes: a drafted job reads the planes the committed one did.
    drafted.source = PreviewSource::Raw {
        image: match &exact.source {
            PreviewSource::Raw { image, .. } => image.clone(),
            PreviewSource::Jpeg(_) => unreachable!(),
        },
        settings: match &drafted.source {
            PreviewSource::Raw { settings, .. } => *settings,
            PreviewSource::Jpeg(_) => unreachable!(),
        },
    };
    let mut queue = PreviewQueue::default();
    queue.request(exact);
    let first = drain_all(&mut queue);
    assert!(
        first[0].proxy().is_some_and(|proxy| proxy.built) && !first[0].approximate_white_balance
    );
    queue.request(drafted);
    let second = drain_all(&mut queue);
    assert_eq!(second[0].phase(), PreviewPhase::Proxy);
    assert!(
        !second[0].proxy().is_some_and(|proxy| proxy.built),
        "the drafted job reuses the proxy"
    );
    assert!(second[0].approximate_white_balance);
    assert_ne!(
        second[0].raster().unwrap().rgba,
        first[0].raster().unwrap().rgba,
        "the cached pixels render through the drafted matrix, not the cached settings"
    );
}

/// The same job over planes that hold its white balance is exact: unlabelled, and reduced.
#[test]
fn an_exact_raw_job_is_unlabelled_and_analysed() {
    let job = raw_job(None);
    assert!(!job.source.approximate_white_balance());
    let mut queue = PreviewQueue::default();
    queue.request(job);
    let results = drain_all(&mut queue);
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|result| !result.approximate_white_balance)
    );
    assert!(
        results[0]
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_none(),
        "a proxy frame is never reduced"
    );
    assert!(
        results[1]
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_some(),
        "the exact frame is"
    );
}

/// A cached RAW proxy is pixels, not settings: a second job over the same developed planes
/// with another exposure is a cache hit that renders at its own exposure.
#[test]
fn a_cached_raw_proxy_renders_at_the_exposure_of_the_job_that_hits_it() {
    use crate::{LinearImage, LinearSettings};
    let width = 2000;
    let height = 1200;
    let planes: Vec<f32> = (0..3 * width * height)
        .map(|index| 0.1 + (index % 997) as f32 / 4000.0)
        .collect();
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:raw").unwrap();
    let bounds = ProxyBounds {
        width: 400,
        height: 300,
    };
    let job_at = |ev: f64, analyse: bool| {
        let mut job = job(1, analyse);
        // The stock test job carries a pixel-stage layer, which is not proxy-eligible; the
        // development settings alone are the stack under test.
        job.recipe.layers.clear();
        job.source = PreviewSource::Raw {
            image: image.clone(),
            settings: LinearSettings {
                exposure_ev: ev,
                white_balance: None,
            },
        };
        job.proxy = Some(bounds);
        job
    };
    let mut queue = PreviewQueue::default();
    let first = queue.request(job_at(0.0, false));
    let mut dark = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while dark.is_none() {
        if let Some(result) = queue.poll()
            && result.generation == first
            && result.phase() == PreviewPhase::Proxy
        {
            assert!(
                result.proxy().is_some_and(|proxy| proxy.built),
                "the first job builds the proxy"
            );
            dark = Some(result.into_raster().expect("a proxy frame"));
        }
        assert!(Instant::now() < deadline, "the first proxy never came");
        std::thread::yield_now();
    }
    while queue.is_busy() {
        let _ = queue.poll();
        std::thread::yield_now();
    }
    let second = queue.request(job_at(1.0, false));
    let mut bright = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while bright.is_none() {
        if let Some(result) = queue.poll()
            && result.generation == second
            && result.phase() == PreviewPhase::Proxy
        {
            assert!(
                !result.proxy().is_some_and(|proxy| proxy.built),
                "the same planes and bounds are a cache hit"
            );
            bright = Some(result.into_raster().expect("a proxy frame"));
        }
        assert!(Instant::now() < deadline, "the second proxy never came");
        std::thread::yield_now();
    }
    let (dark, bright) = (dark.unwrap(), bright.unwrap());
    assert_eq!((dark.width, dark.height), (bright.width, bright.height));
    let brighter = dark
        .rgba
        .chunks_exact(4)
        .zip(bright.rgba.chunks_exact(4))
        .filter(|(a, b)| b[0] > a[0])
        .count();
    assert!(
        brighter > (dark.width * dark.height / 2) as usize,
        "one stop more exposure brightens the cached proxy, not the cached settings"
    );
}

/// A luminance band on its own, which is what makes a mask read pixels.
fn band_mask() -> Mask {
    let mut mask = Mask::new("Mask 1");
    mask.components.push(Component::new(
        "Luminance range 1",
        ComponentMode::Add,
        "luminance-range",
        json!({"low": 15.0, "low_feather": 20.0, "high": 90.0, "high_feather": 20.0}),
    ));
    mask
}

/// A gradient intersected with a band: a value-based mask whose conservative rectangle is the
/// gradient's, which is what the per-cell pixel read is skipped outside.
fn mixed_mask() -> Mask {
    let mut mask = gradient_mask(0.3);
    mask.components.push(Component::new(
        "Luminance range 1",
        ComponentMode::Intersect,
        "luminance-range",
        json!({"low": 15.0, "low_feather": 20.0, "high": 90.0, "high_feather": 20.0}),
    ));
    mask
}

/// A value-based mask whose first bound layer sits behind a **spatial** layer has no grid, and the
/// refusal names the cost rather than paying it.
///
/// A point sample through a spatial segment is the declared exception to [performance rule
/// 4](../../docs/engineering/performance-rules.md#rules): it evaluates the stage-aligned tiles its
/// pixel needs plus the operation's halo, so asking it once per display cell over the whole stage
/// would evaluate every tile of the picture on every overlay. That is not an overlay to ship
/// slowly, so the grid is refused here on exactly the rule the unbound mask is refused on, and the
/// 100% view still reads such a selection. The **geometric** half of the same stack is unaffected,
/// because a position-only mask needs no pixel at all.
#[test]
fn a_value_based_mask_behind_a_spatial_layer_has_no_grid_and_says_what_it_would_cost() {
    let mask = band_mask();
    let geometric = gradient_mask(0.3);
    let presence = |mask: Option<&Mask>| Layer {
        id: LayerId::new(),
        effect_id: crate::PRESENCE_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"texture": 40.0}),
        mask: mask.map(|mask| mask.id.clone()),
        artifacts: Vec::new(),
    };
    for (held, absent) in [(&mask, true), (&geometric, false)] {
        let job = stacked_with_masks(
            64,
            48,
            vec![presence(None), presence(Some(held))],
            vec![held.clone()],
            None,
        );
        let request = MaskOverlayRequest {
            mask: held.id.clone(),
            component: None,
            cells_w: 8,
            cells_h: 6,
        };
        let frame = render(
            &job.registry,
            &job.source,
            &job.recipe,
            RenderOptions::default(),
            &job.context,
        )
        .expect("the stack compiles");
        let (grid, reason) = mask_overlay_for(
            &job.registry,
            &frame,
            &job.recipe,
            &request,
            &Cancel::never(),
            &job.context,
        );
        if absent {
            assert!(grid.is_none(), "a value-based mask behind a spatial layer");
            let reason = reason.expect("the host's own reason travels with the frame");
            assert!(
                reason.contains("depends on the pixel it reads")
                    && reason.contains("tile per grid cell")
                    && reason.contains("100% view"),
                "{reason}"
            );
        } else {
            assert!(
                grid.is_some(),
                "a position-only mask needs no pixel and keeps its grid: {reason:?}"
            );
            assert_eq!(reason, None);
        }
    }
}

/// What the coverage overlay costs the exact preview phase, on 24 MP and 60 MP, before and after
/// a value-based component is in the mask — the measurement proposal P16 of
/// `docs/design/range-study.md` was decided against.
///
/// The "before" figure for a value-based mask is nothing at all, because such a mask was refused a
/// grid; the geometric rows are the delivered cost of a grid and must not have moved. So the added
/// cost is the band and mixed rows, and it is stated against the exact render of the same frame,
/// which is the phase the grid is filled beside.
///
/// The two grid sizes are the two a person actually asks for, from
/// `state::histogram::overlay_cells`: at Fit one cell per physical pixel of the drawn photograph,
/// and at 100% the delivered 4096-cell cap a side.
///
/// Ignored by default because it is a measurement and not a pass/fail property. Run it with
/// `cargo test --release --locked --package lightwell-core --lib -- --ignored --nocapture
/// preview::tests::the_cost_of_a_coverage_grid`, and record the host's one-minute load average
/// beside every figure.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn the_cost_of_a_coverage_grid() {
    // A canvas 1728 px wide is the Develop workspace's photograph on the reference machine.
    let stages = [("24 MP", 6000_u32, 4000_u32), ("60 MP", 9504, 6336)];
    for (label, width, height) in stages {
        let fit = crate::analysis::MAX_OVERLAY_CELLS.min(1728);
        let fit_cells = (fit, (fit * height).div_ceil(width));
        let hundred = {
            let cap = f64::from(crate::analysis::MAX_OVERLAY_CELLS);
            let scale = (cap / f64::from(width)).min(cap / f64::from(height));
            (
                (f64::from(width) * scale).round() as u32,
                (f64::from(height) * scale).round() as u32,
            )
        };
        for (name, mask) in [
            ("gradient (delivered)", gradient_mask(0.3)),
            ("band", band_mask()),
            ("gradient ∩ band", mixed_mask()),
        ] {
            let job =
                stacked_with_masks(width, height, masked_basic(&mask), vec![mask.clone()], None);
            let registry = job.registry.clone();
            let source = job.source.clone();
            let recipe = job.recipe.clone();
            let snapshot = job.entry.snapshot.id.clone();
            // The phase the grid is filled beside, for the figures to be stated against.
            let started = Instant::now();
            let raster = source
                .render(&registry, snapshot, &recipe)
                .expect("the exact frame");
            let render = started.elapsed();
            std::hint::black_box(raster.rgba.len());
            for (view, (cells_w, cells_h)) in [("fit", fit_cells), ("100%", hundred)] {
                let request = MaskOverlayRequest {
                    mask: mask.id.clone(),
                    component: None,
                    cells_w,
                    cells_h,
                };
                let context = crate::render::testing::context();
                let frame = crate::render(
                    &registry,
                    &source,
                    &recipe,
                    RenderOptions::default(),
                    context,
                )
                .expect("the stack compiles");
                let started = Instant::now();
                let (grid, absent) = mask_overlay_for(
                    &registry,
                    &frame,
                    &recipe,
                    &request,
                    &Cancel::never(),
                    crate::render::testing::context(),
                );
                let elapsed = started.elapsed();
                let cells = u64::from(cells_w) * u64::from(cells_h);
                match grid {
                    Some(grid) => {
                        std::hint::black_box(grid.coverage.len());
                        println!(
                            "{label} {name} at {view}: {cells_w}x{cells_h} = {cells} cells in \
                         {:.1} ms ({:.1} ns/cell), beside a {:.1} ms exact render — {:.1}% of it",
                            elapsed.as_secs_f64() * 1000.0,
                            elapsed.as_secs_f64() * 1e9 / cells as f64,
                            render.as_secs_f64() * 1000.0,
                            100.0 * elapsed.as_secs_f64() / render.as_secs_f64(),
                        );
                    }
                    None => println!(
                        "{label} {name} at {view}: no grid — {}",
                        absent.unwrap_or_else(|| "nothing to describe".into())
                    ),
                }
            }
        }
    }
}

/// A straightened crop over a RAW source with a Presence layer, previewed while another
/// evaluation holds the whole spatial target: the pointer readout sampling through the same
/// layer on the owner thread, which is how a committed RAW crop was once refused with "spatial
/// processing needs … bytes, and … of the … byte spatial budget is in use" and left unshown.
/// Both phases deliver the cropped frame, each byte for byte the frame the same stack renders
/// with the target free, and every batch releases what it reserved.
#[test]
fn a_cropped_raw_preview_with_presence_renders_while_the_spatial_target_is_held() {
    use crate::{LinearImage, LinearSettings, PRESENCE_EFFECT};
    let _guard = crate::render::spatial::tests::spatial_guard();
    crate::render::testing::clear_estimates();
    // More than one 512 px tile each way, so the spatial pass runs in batches.
    let (width, height) = (1100_u32, 700_u32);
    let planes: Vec<f32> = (0..3 * width * height)
        .map(|index| 0.05 + (index % 1009) as f32 / 1400.0)
        .collect();
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:raw-crop").unwrap();
    let stage = CropStage {
        width,
        height,
        angle: 7.0,
    };
    let (box_width, box_height) = stage.bounding_box();
    let fitted = stage.fit_about_center(BoxRect {
        x: 0.0,
        y: 0.0,
        width: box_width,
        height: box_height,
    });
    let mut job = job(1, false);
    job.recipe.layers = vec![
        Layer {
            id: LayerId::new(),
            effect_id: PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"clarity": 60.0}),
            artifacts: Vec::new(),
            mask: None,
        },
        Layer::crop(fitted.normalized(&stage)),
    ];
    job.source = PreviewSource::Raw {
        image,
        settings: LinearSettings::default(),
    };
    let display = ProxyBounds {
        width: 480,
        height: 320,
    };
    job.proxy = Some(display);
    let registry = job.registry.clone();
    let recipe = job.recipe.clone();
    let snapshot = job.entry.snapshot.id.clone();
    let output = registry.compile(width, height, &recipe).unwrap().stage();
    assert!(
        output.width < width && output.height < height,
        "the crop trims the stage"
    );
    let plan = job
        .source
        .proxy_plan(&registry, &recipe, display)
        .unwrap()
        .expect("a proxy is worthwhile");
    let exact_reference = job
        .source
        .render(&registry, snapshot.clone(), &recipe)
        .expect("the stack renders with the target free");
    let proxy_reference = job
        .source
        .proxy(plan)
        .unwrap()
        .render(&registry, snapshot, &recipe)
        .expect("the proxy renders with the target free");

    let budget = crate::render::testing::context().spatial();
    let held = budget.reserve(budget.target(), 1);
    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    let results = drain_all(&mut queue);
    drop(held);
    assert_eq!(budget.in_use(), 0, "every batch released its reservation");
    assert_eq!(results.len(), 2, "one proxy frame and one exact frame");
    let [proxy, exact] = <[PreviewResult; 2]>::try_from(results).ok().unwrap();
    assert_eq!(
        (proxy.generation, proxy.phase()),
        (generation, PreviewPhase::Proxy)
    );
    assert_eq!(
        (exact.generation, exact.phase()),
        (generation, PreviewPhase::Exact)
    );
    assert_eq!(
        exact.exact().and_then(|exact| exact.proxy_declined.clone()),
        None,
        "the proxy phase ran"
    );
    let proxy = proxy
        .into_raster()
        .expect("the proxy phase renders beside a held target");
    assert_eq!(
        (proxy.width, proxy.height),
        (proxy_reference.width, proxy_reference.height)
    );
    assert!(
        proxy.rgba == proxy_reference.rgba,
        "the proxy frame differs"
    );
    let exact = exact
        .into_raster()
        .expect("the exact phase renders beside a held target");
    assert_eq!((exact.width, exact.height), (output.width, output.height));
    assert!(
        exact.rgba == exact_reference.rgba,
        "the exact frame differs"
    );
}
