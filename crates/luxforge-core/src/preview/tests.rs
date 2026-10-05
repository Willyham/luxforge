//! The preview queue and its worker, end to end.

use super::coverage::{MaskOverlayRequest, mask_overlay_for};
use super::*;
use crate::{
    ActivityBoard, AssetId, BASIC_EFFECT, BoxRect, Cancel, CropStage, EFFECT_FORMAT, EntryId,
    Evaluation, HistoryEntry, Layer, LayerId, Mask, ModuleRegistry, Orientation, PIXEL_EFFECT,
    ProxyBounds, RECIPE_FORMAT, Recipe, RenderContext, RenderOptions, Snapshot, SnapshotId,
    SourceImage, Transform, render,
};
use crate::{Component, ComponentMode};
use luxforge_testbase::{wait_for, wait_until};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    let source = PreviewSource::Jpeg(SourceImage {
        width: 1,
        height: 1,
        rgba: vec![0, 0, 0, 255].into(),
        fingerprint: "test".into(),
        orientation: 1,
        capture: Default::default(),
    });
    let evaluation = Evaluation::new(
        Arc::new(ModuleRegistry::developer()),
        RenderContext::new(),
        source,
        entry,
        recipe,
        None,
    );
    let mut job = PreviewJob::new(evaluation).unwrap();
    job.analyse = analyse;
    job
}

/// What a test job's evaluation is built from.
struct Parts {
    registry: Arc<ModuleRegistry>,
    context: RenderContext,
    source: PreviewSource,
    recipe: Recipe,
}

/// `job` evaluating what `change` makes of its parts. An evaluation is immutable and compiled
/// once, so a test that wants another stack, source, registry or context builds another one. The
/// job keeps its entry, its draft and how it is presented; its identity is the new evaluation's.
fn rebuilt(mut job: PreviewJob, change: impl FnOnce(&mut Parts)) -> PreviewJob {
    let held = &job.evaluation;
    let mut parts = Parts {
        registry: held.registry().clone(),
        context: held.context().clone(),
        source: held.source().clone(),
        recipe: held.recipe().clone(),
    };
    change(&mut parts);
    let evaluation = Evaluation::new(
        parts.registry,
        parts.context,
        parts.source,
        held.entry().clone(),
        parts.recipe,
        held.draft().cloned(),
    );
    job.identity = evaluation.identity().unwrap();
    job.evaluation = evaluation;
    job
}

/// The preview worker reduces the frame it just rendered, so a displayed target needs no second
/// render. The report must equal the reduction of that very raster, byte for byte.
#[test]
fn an_analysing_preview_returns_the_exact_reduction_of_the_frame_it_rendered() {
    let mut queue = PreviewQueue::default();
    let wanted = queue.request(job(9, true));
    let result = wait_for("the analysing preview", || queue.poll());
    assert_eq!(result.generation, wanted);
    let raster = result.raster().expect("a frame");
    assert_eq!(
        result
            .exact()
            .and_then(|exact| exact.report.clone())
            .expect("the job asked for a report"),
        crate::analysis::reduce(&raster.rgba, raster.width, raster.height, &Cancel::never())
            .unwrap()
    );
    assert!(result.identity.has_output_stage());
    // A job that does not ask carries no report: `None` is "not asked", never empty counts.
    let wanted = queue.request(job(9, false));
    let result = wait_for("the plain preview", || queue.poll());
    assert_eq!(result.generation, wanted);
    assert!(
        result
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_none()
    );
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
        capture: Default::default(),
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
    let evaluation = Evaluation::new(
        Arc::new(ModuleRegistry::developer()),
        RenderContext::new(),
        source,
        entry,
        recipe,
        None,
    );
    let mut job = PreviewJob::new(evaluation).unwrap();
    job.proxy = proxy;
    job
}

/// Every result the active job has left to send, in order. The queue releases the active slot
/// when the exact phase lands, so an idle queue with no pending job is the end of the job.
fn drain_all(queue: &mut PreviewQueue) -> Vec<PreviewResult> {
    let mut results = Vec::new();
    wait_until("the preview worker finishing", || {
        results.extend(queue.poll());
        !queue.is_busy()
    });
    results
}

/// `job` as a moving frame's: [`PreviewIntent::Interactive`], the one intent with a proxy phase.
fn moving(mut job: PreviewJob) -> PreviewJob {
    job.intent = PreviewIntent::Interactive;
    job
}

/// `job`'s proxy frame, as a draft's tick renders it, then its exact frame at rest: two jobs on
/// `queue`, each with its one phase.
fn proxy_then_exact(queue: &mut PreviewQueue, job: PreviewJob) -> [PreviewResult; 2] {
    queue.request(moving(job.clone()));
    let mut proxy = drain_all(queue);
    queue.request(job);
    let mut exact = drain_all(queue);
    assert_eq!(
        (proxy.len(), exact.len()),
        (1, 1),
        "a proxy is a moving job's one phase, and a job at rest renders none"
    );
    [proxy.remove(0), exact.remove(0)]
}

/// Poll until this generation's phase is delivered, collecting what came before it: each
/// delivery's generation, phase and whether it was cancelled.
fn drain_until(
    queue: &mut PreviewQueue,
    generation: u64,
    phase: PreviewPhase,
) -> Vec<(u64, PreviewPhase, bool)> {
    let mut delivered = Vec::new();
    wait_until(&format!("generation {generation} {phase:?}"), || {
        queue.poll().is_some_and(|result| {
            delivered.push((result.generation, result.phase(), result.cancelled()));
            (result.generation, result.phase()) == (generation, phase)
        })
    });
    delivered
}

fn viewport_job(intent: PreviewIntent) -> PreviewJob {
    let mut job = stacked(128, 96, Vec::new(), None);
    job.viewport = Some(crate::Region {
        x0: 17,
        y0: 13,
        width: 31,
        height: 23,
    });
    job.intent = intent;
    job.analyse = true;
    job
}

#[test]
fn interactive_viewport_delivers_one_bounded_region_without_a_report() {
    let mut queue = PreviewQueue::default();
    let generation = queue.request(viewport_job(PreviewIntent::Interactive));
    let results = drain_all(&mut queue);
    assert_eq!(
        results.len(),
        1,
        "moving input ends after its first visible phase"
    );
    let result = &results[0];
    assert_eq!(
        (result.generation, result.phase()),
        (generation, PreviewPhase::Region)
    );
    let region = result.region().expect("visible region");
    assert_eq!(
        (
            region.frame.full_stage.width,
            region.frame.full_stage.height
        ),
        (128, 96)
    );
    assert!(region.frame.full_rect.x0 <= 17 && region.frame.full_rect.x1() >= 48);
    assert_eq!(
        (region.frame.stage.width, region.frame.stage.height),
        (64, 48)
    );
    assert!(region.frame.raster.width <= 64 && region.frame.raster.height <= 48);
    assert!(region.frame.approximation.reduced_detail);
    assert!(result.viewport_declined.is_none());
    assert!(
        result.exact().is_none(),
        "no whole-image histogram during motion"
    );
}

#[test]
fn settled_viewport_delivers_exact_region_then_whole_report() {
    let mut queue = PreviewQueue::default();
    let generation = queue.request(viewport_job(PreviewIntent::Settle));
    let results = drain_all(&mut queue);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].phase(), PreviewPhase::Region);
    assert_eq!(results[1].phase(), PreviewPhase::Exact);
    assert_eq!(results[0].generation, generation);
    assert_eq!(results[1].generation, generation);
    let region = results[0].region().unwrap();
    assert_eq!(
        region.frame.stage, region.frame.full_stage,
        "settled region is exact detail"
    );
    let exact = results[1].exact().unwrap();
    assert!(exact.result.is_ok());
    assert!(
        exact.report.is_some(),
        "only the whole image may supply the histogram"
    );
}

/// The live overlay's coverage over a visible rectangle is the core's region reduction of that
/// rectangle, and over the whole stage the whole-stage reduction, each at its own cells.
#[test]
fn mask_overlay_coverage_fills_a_visible_region_or_the_whole_stage() {
    let mask = gradient_mask(0.6);
    let job = stacked_with_masks(128, 96, masked_basic(&mask), vec![mask.clone()], None);
    let visible = crate::Region {
        x0: 23,
        y0: 11,
        width: 41,
        height: 29,
    };
    let transform = render(
        job.evaluation.registry(),
        job.evaluation.source().input(),
        job.evaluation.recipe(),
        RenderOptions::default(),
        &RenderContext::new(),
    )
    .unwrap()
    .transform()
    .unwrap();
    let compiled = crate::mask::CompiledMask::new(
        &mask,
        crate::modules::Stage {
            width: 128,
            height: 96,
        },
        &job.evaluation.recipe().strokes,
    )
    .unwrap();
    let expected_grid = |region, cells_w, cells_h| {
        crate::analysis::coverage_grid_region(
            &compiled,
            &transform,
            region,
            cells_w,
            cells_h,
            crate::analysis::MaskPixels::Unavailable("geometric mask"),
            &Cancel::never(),
        )
        .unwrap()
        .unwrap()
    };
    let expected_region = expected_grid(visible, 15, 11);
    let expected_whole = expected_grid(
        crate::Region {
            x0: 0,
            y0: 0,
            width: 128,
            height: 96,
        },
        61,
        43,
    );
    let target = MaskCoverageTarget::Existing {
        mask: mask.id.clone(),
        component: None,
    };
    let coverage = |cells, region| {
        job.evaluation
            .mask_overlay_coverage(&target, cells, region, None, &Cancel::never())
            .unwrap()
            .outcome
            .expect("no cached key was offered")
            .grid
            .expect("a geometric mask has a grid")
    };
    let region = &coverage((15, 11), Some(visible));
    let whole = &coverage((61, 43), None);
    assert_eq!((region.cells_w, region.cells_h), (15, 11));
    assert_eq!((whole.cells_w, whole.cells_h), (61, 43));
    assert_eq!(region.coverage.len(), 15 * 11);
    assert_eq!(whole.coverage.len(), 61 * 43);
    assert_eq!(region.coverage, expected_region);
    assert_eq!(whole.coverage, expected_whole);
    assert_ne!(
        region.coverage, whole.coverage,
        "the whole-stage grid samples the whole output stage"
    );
}

/// A job with display bounds smaller than its stage: a moving one produces the proxy frame alone,
/// and one at rest the exact frame alone. Each is byte for byte the render this test computes
/// independently — the proxy against the exact downscale of the source, the exact one against
/// the prepared source itself.
#[test]
fn a_moving_job_yields_the_proxy_phase_and_a_job_at_rest_the_exact_phase() {
    let display = bounds(40, 40);
    let job = stacked(64, 48, eligible_layers(64, 48), Some(display));
    let registry = job.evaluation.registry().clone();
    let source = job.evaluation.source().clone();
    let recipe = job.evaluation.recipe().clone();
    let snapshot = job.evaluation.entry().snapshot.id.clone();
    let plan = source
        .proxy_plan(&registry, &recipe, display)
        .expect("a plan")
        .expect("a proxy is worthwhile");

    let mut queue = PreviewQueue::default();
    let mut lived = |job: PreviewJob| {
        let requested = Instant::now();
        let generation = queue.request(job);
        let mut results = drain_all(&mut queue);
        let lifetime_ms = requested.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(results.len(), 1, "each job renders one phase");
        (generation, results.remove(0), lifetime_ms)
    };
    let (moved, proxy, proxy_lifetime_ms) = lived(moving(job.clone()));
    let (rested, exact, exact_lifetime_ms) = lived(job);

    // Each phase reports its own worker time: finite, and inside its job's own lifetime, so it
    // counts nothing before the request.
    for (phase, ms, lifetime_ms) in [
        ("proxy", proxy.render_ms, proxy_lifetime_ms),
        ("exact", exact.render_ms, exact_lifetime_ms),
    ] {
        assert!(
            ms.is_finite() && ms >= 0.0 && ms <= lifetime_ms,
            "the {phase} phase reports {ms} ms of a {lifetime_ms} ms job"
        );
    }

    assert_eq!(proxy.generation, moved);
    assert_eq!(exact.generation, rested);
    assert_eq!(proxy.phase(), PreviewPhase::Proxy);
    assert_eq!(exact.phase(), PreviewPhase::Exact);
    // The fitted crop reads all but the proxy stage's corners, so the source holds the window of
    // the proxy stage those taps reach: never more than the whole proxy stage.
    let (width, height) = proxy.proxy().expect("a proxy phase").dimensions;
    assert!(
        width <= plan.width && height <= plan.height,
        "a {width}x{height} proxy source of a {}x{} proxy stage",
        plan.width,
        plan.height
    );
    assert_eq!(exact.proxy().map(|proxy| proxy.dimensions), None);
    assert_eq!(
        exact.exact().and_then(|exact| exact.proxy_declined.clone()),
        None,
        "a job at rest asks for no proxy, so it declines none"
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

/// A stack is compiled once at each stage it renders at: once at the exact stage, by its
/// evaluation when the job is built, whose compilation plans a moving job's proxy and renders the
/// exact frame of a job at rest over the same evaluation, and once at the proxy stage, by the plan
/// that walks the window its output reads, whose compilation renders the proxy frame and says
/// whether it is approximate. The count sees every compile the entry point makes, the proxy plan's
/// included, over a whole-stage proxy and a tight crop's windowed one. A stack drawn only at rest
/// is compiled once.
#[test]
fn a_preview_job_compiles_its_stack_once_per_stage_it_renders_at() {
    let mask = gradient_mask(0.5);
    let mut cropped = masked_basic(&mask);
    cropped.push(Layer::crop(crate::CropPayload {
        angle: 3.0,
        x: 0.55,
        y: 0.45,
        width: 0.2,
        height: 0.2,
    }));
    let cases = [
        (
            "whole-stage proxy",
            64,
            48,
            masked_basic(&mask),
            Some(bounds(40, 40)),
            2,
            2,
        ),
        (
            "windowed proxy",
            400,
            300,
            cropped,
            Some(bounds(40, 30)),
            2,
            2,
        ),
        ("no proxy", 64, 48, masked_basic(&mask), None, 1, 1),
    ];
    let mut queue = PreviewQueue::default();
    for (name, width, height, layers, proxy, phases, compiles) in cases {
        let context = RenderContext::new();
        let job = rebuilt(
            stacked_with_masks(width, height, layers, vec![mask.clone()], proxy),
            |parts| parts.context = context.clone(),
        );
        let source = job.evaluation.source().clone();
        let recipe = job.evaluation.recipe().clone();
        let registry = job.evaluation.registry().clone();
        let results = match proxy {
            Some(_) => Vec::from(proxy_then_exact(&mut queue, job)),
            None => {
                queue.request(job);
                drain_all(&mut queue)
            }
        };
        assert_eq!(results.len(), phases, "{name}");
        let exact = results.last().expect("an exact phase");
        assert!(exact.raster().is_ok(), "{name}");
        let first = results.first().expect("a first phase");
        if let Some(display) = proxy {
            let plan = source
                .proxy_plan(&registry, &recipe, display)
                .unwrap()
                .expect("a proxy is worthwhile");
            let windowed =
                first.proxy().expect("a proxy phase").dimensions != (plan.width, plan.height);
            assert_eq!(windowed, name == "windowed proxy", "{name}");
        }
        assert_eq!(context.compiles(), compiles, "{name}");
    }
}

/// A mask's coverage is answered over each stack: a grid over a geometric mask and over a value-based
/// one, through a straightening crop, and the host's refusal where a value-based mask sits behind a
/// spatial layer. It reads no pixel of any rendered frame, so display bounds on the job that holds
/// the evaluation change nothing about it.
#[test]
fn the_coverage_grid_answers_each_stack_whatever_the_display_bounds() {
    let presence = |mask: Option<&Mask>| Layer {
        id: LayerId::new(),
        effect_id: crate::PRESENCE_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"texture": 40.0}),
        mask: mask.map(|mask| mask.id.clone()),
        artifacts: Vec::new(),
    };
    let gradient = gradient_mask(0.3);
    let band = band_mask();
    let mixed = mixed_mask();
    // The eligible stack — an orientation, a Basic layer and a fitted straightening crop — with
    // its Basic layer bound to the mask, so the grid composes a real geometry tail.
    let mut cropped = eligible_layers(64, 48);
    cropped[1].mask = Some(gradient.id.clone());
    let stacks = [
        ("gradient", gradient.clone(), masked_basic(&gradient)),
        ("band", band.clone(), masked_basic(&band)),
        ("gradient and band", mixed.clone(), masked_basic(&mixed)),
        ("gradient under a crop", gradient.clone(), cropped),
        (
            "band behind a spatial layer",
            band.clone(),
            vec![presence(None), presence(Some(&band))],
        ),
    ];
    for (name, mask, layers) in stacks {
        let target = MaskCoverageTarget::Existing {
            mask: mask.id.clone(),
            component: None,
        };
        let coverage = |proxy: Option<ProxyBounds>| {
            stacked_with_masks(64, 48, layers.clone(), vec![mask.clone()], proxy)
                .evaluation
                .mask_overlay_coverage(&target, (13, 9), None, None, &Cancel::never())
                .expect("the stack holds the mask")
                .outcome
                .expect("no cached key was offered")
        };
        let expected = coverage(None);
        let refused = name == "band behind a spatial layer";
        assert_eq!(
            (expected.grid.is_some(), expected.absent.is_some()),
            (!refused, refused),
            "{name}: a grid, or the host's reason there is none"
        );
        for display in [bounds(40, 40), bounds(4000, 4000)] {
            assert_eq!(coverage(Some(display)), expected, "{name}");
        }
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

/// The delivered proxy contract over a **masked** stack: the recipe is proxy eligible, a moving
/// job yields its proxy, and the proxy frame is byte for byte the exact recipe rendered against
/// the exact downscale of the source.
///
/// This is the assertion
/// [`a_moving_job_yields_the_proxy_phase_and_a_job_at_rest_the_exact_phase`] makes, over a stack
/// whose
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
    let registry = job.evaluation.registry().clone();
    let source = job.evaluation.source().clone();
    let recipe = job.evaluation.recipe().clone();
    let snapshot = job.evaluation.entry().snapshot.id.clone();
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
    let [proxy, exact] = proxy_then_exact(&mut queue, job);
    assert_eq!(
        (proxy.phase(), exact.phase()),
        (PreviewPhase::Proxy, PreviewPhase::Exact)
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
        queue.request(moving(job));
        let mut results = drain_all(&mut queue);
        assert_eq!(results.len(), 1);
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
    let registry = job.evaluation.registry().clone();
    let recipe = job.evaluation.recipe().clone();
    let mut queue = PreviewQueue::default();
    queue.request(moving(job));
    let results = drain_all(&mut queue);
    assert_eq!(results.len(), 1);
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

/// The three ways a moving job that offered bounds has no proxy phase, and the two ways a job asks
/// for none: no bounds, or a job at rest. Each yields exactly one exact frame, and each that asked
/// says why it has none.
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

    // No bounds at all, or a job at rest: the exact path, and nothing to decline.
    for job in [
        moving(stacked(64, 48, eligible_layers(64, 48), None)),
        stacked(64, 48, eligible_layers(64, 48), Some(bounds(8, 8))),
    ] {
        let result = only(&mut queue, job);
        assert_eq!(
            result
                .exact()
                .and_then(|exact| exact.proxy_declined.clone()),
            None
        );
    }

    // An ineligible stack: the layer that made it so is named.
    let pixel = vec![Layer::pixel(0, 0, [9, 9, 9])];
    let result = only(
        &mut queue,
        moving(stacked(64, 48, pixel, Some(bounds(8, 8)))),
    );
    let reason = result
        .exact()
        .and_then(|exact| exact.proxy_declined.clone())
        .expect("a reason");
    assert!(reason.contains(PIXEL_EFFECT), "{reason}");
    assert!(reason.contains("layer 0"), "{reason}");

    // Bounds the stage already fits: there is no proxy smaller than the source to build.
    let large = stacked(64, 48, eligible_layers(64, 48), Some(bounds(4000, 4000)));
    let result = only(&mut queue, moving(large));
    let reason = result
        .exact()
        .and_then(|exact| exact.proxy_declined.clone())
        .expect("a reason");
    assert!(reason.contains("scale is 1"), "{reason}");

    // A truncated job is judged on the prefix it renders: an ineligible layer inside the prefix
    // declines its proxy exactly as it declines a whole stack's.
    let mut pixel = vec![Layer::pixel(0, 0, [9, 9, 9])];
    pixel.extend(eligible_layers(64, 48));
    let mut truncated = stacked(64, 48, pixel, Some(bounds(8, 8)));
    truncated.layer_count = Some(1);
    let result = only(&mut queue, moving(truncated));
    let reason = result
        .exact()
        .and_then(|exact| exact.proxy_declined.clone())
        .expect("a reason");
    assert!(reason.contains(PIXEL_EFFECT), "{reason}");
}

/// A truncated job that offers bounds — a crop draft's input stage at Fit — has a proxy phase of
/// its own layer prefix: planned from the prefix's output stage and judged on the prefix's layers,
/// so a layer after the prefix that would decline the whole stack's proxy declines nothing here.
/// Asked for interactively, which is how the desktop asks for the stage at Fit, it renders the
/// proxy alone: the prefix over the exact downscale of the source, byte for byte; at rest, the
/// prefix at full resolution alone. It is never analysed: the owner refuses a job that is both
/// truncated and analysing.
#[test]
fn a_truncated_job_with_bounds_has_its_prefixs_own_proxy_phase() {
    let display = bounds(40, 40);
    // The crop's input stage is the orientation and the Basic layer ahead of the crop; a pixel
    // layer after the crop makes the whole stack ineligible for a proxy.
    let mut layers = eligible_layers(64, 48);
    layers.push(Layer::pixel(0, 0, [9, 9, 9]));
    let count = 2;
    let mut job = stacked(64, 48, layers, Some(display));
    job.layer_count = Some(count);
    let registry = job.evaluation.registry().clone();
    let source = job.evaluation.source().clone();
    let whole = job.evaluation.recipe().clone();
    assert!(
        registry.proxy_eligible(&whole).is_err(),
        "the whole stack has no proxy"
    );
    let prefix = Recipe {
        layers: whole.layers[..count].to_vec(),
        ..whole.clone()
    };
    let snapshot = job.evaluation.entry().snapshot.id.clone();
    let plan = source
        .proxy_plan(&registry, &prefix, display)
        .expect("a plan")
        .expect("a proxy is worthwhile");

    let mut queue = PreviewQueue::default();
    let [proxy, exact] = proxy_then_exact(&mut queue, job.clone());
    assert_eq!(proxy.phase(), PreviewPhase::Proxy);
    assert!(proxy.proxy().is_some_and(|proxy| proxy.built));
    assert!(
        exact
            .exact()
            .and_then(|exact| exact.report.clone())
            .is_none(),
        "a truncated job carries no report"
    );
    // The prefix holds no crop, so its proxy is the whole proxy stage.
    assert_eq!(
        proxy.proxy().map(|proxy| proxy.dimensions),
        Some((plan.width, plan.height))
    );
    let reference = source
        .proxy(plan)
        .expect("the exact downscale")
        .render(&registry, snapshot.clone(), &prefix)
        .expect("the prefix renders at proxy size");
    let frame = proxy.into_raster().expect("a proxy frame");
    assert_eq!(
        (frame.width, frame.height),
        (reference.width, reference.height)
    );
    assert_eq!(
        frame.rgba.as_ref(),
        reference.rgba.as_ref(),
        "the proxy frame is the prefix over the exact downscale"
    );
    assert!(frame.width <= display.width && frame.height <= display.height);
    let reference = source
        .render(&registry, snapshot, &prefix)
        .expect("the exact prefix");
    let full = exact.into_raster().expect("an exact frame");
    assert_eq!((full.width, full.height), (48, 64), "the turned stage");
    assert_eq!(full.rgba.as_ref(), reference.rgba.as_ref());

    // The next moving job at the same bounds reads the proxy source already in hand.
    queue.request(moving(job));
    let results = drain_all(&mut queue);
    assert_eq!(results.len(), 1, "the proxy frame alone");
    assert_eq!(results[0].phase(), PreviewPhase::Proxy);
    assert!(!results[0].proxy().is_some_and(|proxy| proxy.built));
    assert_eq!(
        results[0].raster().expect("a proxy frame").rgba.as_ref(),
        frame.rgba.as_ref()
    );
}

/// The proxy source is built once and held by the queue: the next job at the same identity and
/// the same bounds renders against the source already in hand.
#[test]
fn two_jobs_at_the_same_bounds_build_the_proxy_once() {
    let display = bounds(32, 32);
    let mut queue = PreviewQueue::default();

    queue.request(moving(stacked(
        64,
        48,
        eligible_layers(64, 48),
        Some(display),
    )));
    let first = drain_all(&mut queue);
    assert!(
        first[0].proxy().is_some_and(|proxy| proxy.built),
        "the first job builds the proxy"
    );

    queue.request(moving(stacked(
        64,
        48,
        eligible_layers(64, 48),
        Some(display),
    )));
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
    queue.request(moving(stacked(
        64,
        48,
        eligible_layers(64, 48),
        Some(bounds(24, 24)),
    )));
    let resized = drain_all(&mut queue);
    assert!(
        resized[0].proxy().is_some_and(|proxy| proxy.built),
        "a resized window rebuilds the proxy"
    );
}

/// A tight crop's proxy holds the window of the proxy stage the crop reads, and is keyed by it: a
/// job that changes another layer under the same crop renders against the source already in hand,
/// and a crop that moves builds the window it now reads. Every frame is the exact recipe over the
/// exact downscale of the whole source, byte for byte.
#[test]
fn a_tight_crops_windowed_proxy_is_cached_by_its_window() {
    let display = bounds(40, 30);
    let layers = |exposure: f64, x: f64| {
        vec![
            Layer {
                id: LayerId::new(),
                effect_id: BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"exposure": exposure, "contrast": 15.0}),
                mask: None,
                artifacts: Vec::new(),
            },
            Layer::crop(crate::CropPayload {
                angle: 3.0,
                x,
                y: 0.45,
                width: 0.2,
                height: 0.2,
            }),
        ]
    };
    let mut queue = PreviewQueue::default();
    let mut frame = |job: PreviewJob| {
        let registry = job.evaluation.registry().clone();
        let source = job.evaluation.source().clone();
        let recipe = job.evaluation.recipe().clone();
        queue.request(moving(job));
        let results = drain_all(&mut queue);
        let proxy = results[0].proxy().expect("a proxy phase");
        let (built, dimensions) = (proxy.built, proxy.dimensions);
        let plan = source
            .proxy_plan(&registry, &recipe, display)
            .unwrap()
            .expect("a proxy is worthwhile");
        let reference = source
            .proxy(plan)
            .unwrap()
            .render_proxy_cancellable(&registry, SnapshotId::new(), &recipe, &Cancel::never())
            .unwrap();
        let raster = results[0].raster().expect("a proxy frame");
        assert_eq!(
            raster.rgba.as_ref(),
            reference.rgba.as_ref(),
            "the windowed proxy frame is the exact recipe over the exact downscale"
        );
        assert!(
            u64::from(dimensions.0) * u64::from(dimensions.1)
                < u64::from(plan.width) * u64::from(plan.height) / 4,
            "a {dimensions:?} proxy source of a {}x{} proxy stage",
            plan.width,
            plan.height
        );
        (built, dimensions)
    };

    let (built, first) = frame(stacked(400, 300, layers(0.3, 0.55), Some(display)));
    assert!(built, "the first job builds the window");
    let (built, second) = frame(stacked(400, 300, layers(-0.6, 0.55), Some(display)));
    assert!(
        !built,
        "an exposure change under the same crop hits the window"
    );
    assert_eq!(first, second);
    let (built, _) = frame(stacked(400, 300, layers(-0.6, 0.25), Some(display)));
    assert!(built, "a moved crop reads another window");
}

// ---------------------------------------------------------------------------------------
// The activity each job publishes
// ---------------------------------------------------------------------------------------

/// A job whose one colour layer waits on `gate` in every phase that renders it, so a test can
/// hold the job in the phase it is about. The layer leaves its pixels as it found them.
fn held(gate: &Arc<luxforge_testbase::Gate>, proxy: Option<ProxyBounds>) -> PreviewJob {
    held_behind(gate, proxy, Vec::new())
}

/// [`held`], with `before` placed ahead of the waiting layer.
fn held_behind(
    gate: &Arc<luxforge_testbase::Gate>,
    proxy: Option<ProxyBounds>,
    mut before: Vec<Layer>,
) -> PreviewJob {
    let layer = Layer {
        id: LayerId::new(),
        effect_id: crate::modules::HELD_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({}),
        artifacts: Vec::new(),
        mask: None,
    };
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(crate::modules::HeldModule::shared(gate.clone()))
        .expect("a valid holding module");
    before.push(layer);
    rebuilt(stacked(64, 48, before, proxy), |parts| {
        parts.registry = Arc::new(registry);
    })
}

/// Read the board until `wanted` holds. The job under test is held at a gate, so what it waits
/// for is the worker reaching that gate, never a race with how fast the machine renders.
fn board_until(
    board: &ActivityBoard,
    wanted: impl Fn(&crate::ActivitySnapshot) -> bool,
    what: &str,
) -> crate::ActivitySnapshot {
    wait_for(what, || {
        Some(board.snapshot()).filter(|snapshot| wanted(snapshot))
    })
}

/// A moving job is listed in `proxy` while its one phase runs and a job at rest in `exact`, and
/// each entry has already ended when the queue releases its job. The board keeps every finished
/// entry here, because its recent threshold is zero.
#[test]
fn a_jobs_activity_names_its_phase_and_ends_when_the_queue_releases_it() {
    let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
    let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
    let mut queue = PreviewQueue::default();
    queue.set_activity(board.clone());
    let job = held(&gate, Some(bounds(16, 16)));
    let asset = job.evaluation.entry().asset_id.clone();

    for (job, phase, delivered) in [
        (moving(job.clone()), "proxy", PreviewPhase::Proxy),
        (job, "exact", PreviewPhase::Exact),
    ] {
        gate.shut();
        queue.request(job);
        let running = board_until(
            &board,
            |snapshot| {
                snapshot
                    .active
                    .first()
                    .is_some_and(|active| active.entry.phase.as_deref() == Some(phase))
            },
            "the phase never reached its gate",
        );
        assert_eq!(running.active.len(), 1);
        let entry = &running.active[0].entry;
        assert_eq!(
            (&*entry.kind, &*entry.label),
            ("preview.render", "Rendering preview")
        );
        assert_eq!(entry.asset_id.as_ref(), Some(&asset));
        assert_eq!((&entry.detail, &entry.job_id), (&None, &None));

        gate.open();
        let results = drain_all(&mut queue);
        assert_eq!(
            results
                .iter()
                .map(|result| result.phase())
                .collect::<Vec<_>>(),
            [delivered]
        );
        // The queue released the job the moment its result arrived, and the entry had already
        // ended by then: nothing here waits for the worker again.
        let ended = board.snapshot();
        assert!(ended.active.is_empty(), "{ended:?}");
        let recent = &ended.recent[0];
        assert_eq!(recent.entry.kind, "preview.render");
        assert_eq!(recent.entry.phase.as_deref(), Some(phase));
        assert_eq!(recent.outcome, crate::activity::Outcome::Completed);
    }
    assert_eq!(board.snapshot().recent.len(), 2);
}

/// A job's exact phase publishes how far its spatial tiles have got on the queue while it runs,
/// and its activity carries the same fraction; once the phase has ended the queue reads nothing.
/// Detail's tiles all run before the colour layer behind it waits at its gate, so the reading
/// there is the phase's whole extent, finished.
#[test]
fn the_exact_phase_publishes_its_progress_until_it_ends() {
    let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
    let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
    let mut queue = PreviewQueue::default();
    queue.set_activity(board.clone());
    assert_eq!(queue.progress(), None, "nothing runs");

    gate.shut();
    let detail = Layer::new(
        crate::DETAIL_EFFECT,
        json!({"sharpening": 60, "luminance": 30}),
    );
    let generation = queue.request(held_behind(&gate, None, vec![detail]));
    let progress = wait_for("the exact phase never finished its tiles", || {
        queue
            .progress()
            .filter(|progress| progress.counts.done == progress.counts.planned)
    });
    assert_eq!(progress.generation, generation);
    assert!(progress.counts.planned > 0, "{progress:?}");
    board_until(
        &board,
        |snapshot| {
            snapshot.active.first().is_some_and(|active| {
                active.entry.progress.as_ref().and_then(|p| p.fraction) == Some(1.0)
            })
        },
        "the activity never reported the finished tiles",
    );

    gate.open();
    let results = drain_all(&mut queue);
    assert_eq!(
        results.last().map(PreviewResult::phase),
        Some(PreviewPhase::Exact)
    );
    assert_eq!(queue.progress(), None, "the phase has ended");

    // A stack without a spatial operation publishes its phase with nothing planned: there is no
    // truthful extent to show.
    gate.shut();
    queue.request(held(&gate, None));
    let progress = wait_for("the exact phase never started", || queue.progress());
    assert_eq!(progress.counts, crate::ProgressCounts::default());
    gate.open();
    drain_all(&mut queue);
    assert_eq!(queue.progress(), None);
}

/// The exact phase's meter wakes the consumer only once the phase has run
/// [`super::worker::PROGRESS_QUIET`], and then at most once per interval however many batches
/// finish; before the phase starts it wakes nothing.
#[test]
fn the_exact_meter_wakes_the_consumer_only_after_the_quiet_interval() {
    use std::sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    };
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = wakes.clone();
    let waker: crate::latest::Wake = Arc::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
    });
    let meter_for = |phase: Option<Instant>| {
        let started = Arc::new(OnceLock::new());
        if let Some(phase) = phase {
            let _ = started.set(phase);
        }
        let meter = super::worker::exact_meter(None, Some(waker.clone()), started);
        meter.plan(8);
        meter
    };

    let unstarted = meter_for(None);
    unstarted.advance(1);
    let fresh = meter_for(Some(Instant::now()));
    fresh.advance(1);
    assert_eq!(
        wakes.load(Ordering::Relaxed),
        0,
        "before the quiet interval"
    );

    let long = meter_for(Instant::now().checked_sub(Duration::from_secs(1)));
    for _ in 0..4 {
        long.advance(1);
    }
    assert_eq!(
        wakes.load(Ordering::Relaxed),
        1,
        "batches inside one interval wake the consumer once"
    );
    assert_eq!(long.counts().done, 4);
}

/// A newer request stops the exact phase of the job it replaces, and that job's activity ends
/// cancelled while the newer one completes. The older job is held at its gate inside the first
/// 16-row chunk of its colour pass, so the stop reaches the check before its second chunk.
#[test]
fn a_superseded_jobs_activity_ends_cancelled() {
    let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
    let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
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
                .is_some_and(|active| active.entry.phase.as_deref() == Some("exact"))
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

/// A moving job under the persistent worker: a newer request arrives while the older job's proxy
/// render is held at its gate. The proxy phase is not interrupted — its frame is still newer than
/// anything on screen — and is delivered as a frame; the newer job then renders its own.
#[test]
fn a_superseded_moving_jobs_proxy_is_still_delivered() {
    let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
    let mut queue = PreviewQueue::default();
    gate.shut();
    let older = queue.request(moving(held(&gate, Some(bounds(16, 16)))));
    gate.wait_reached(1, "the proxy render");
    let newer = queue.request(moving(held(&gate, Some(bounds(16, 16)))));
    gate.open();
    let delivered = drain_until(&mut queue, newer, PreviewPhase::Proxy);
    assert_eq!(
        delivered,
        vec![
            (older, PreviewPhase::Proxy, false),
            (newer, PreviewPhase::Proxy, false),
        ],
        "the superseded job's proxy frame, then the newer job's"
    );
}

/// The worker takes the pending job itself when the active one ends: nothing here polls, and
/// the pending job still runs to the end while the first job's outcome waits undelivered. A job
/// replaced in the pending slot is named by the request that replaced it, never starts, and so
/// publishes no activity and delivers nothing.
#[test]
fn the_next_job_starts_without_a_poll() {
    let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
    let gate = std::sync::Arc::new(luxforge_testbase::Gate::new());
    let mut queue = PreviewQueue::default();
    queue.set_activity(board.clone());
    gate.shut();
    let first = queue.request(held(&gate, None));
    // The worker has taken the first job before the next is asked for, so that one waits in the
    // pending slot for the replacing request to displace.
    board_until(
        &board,
        |snapshot| {
            snapshot
                .active
                .first()
                .is_some_and(|active| active.entry.phase.as_deref() == Some("exact"))
        },
        "the first job never started",
    );
    let replaced = queue.request(held(&gate, None));
    assert_eq!(queue.pending_generation(), Some(replaced));
    let Queued {
        generation: second,
        replaced: displaced,
    } = queue.request_replacing(held(&gate, None));
    assert_eq!(
        displaced,
        Some(replaced),
        "the request names what it replaced"
    );
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
        "newest first: the pending job completed, the first was superseded, the replaced one \
         never started"
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
        ],
        "the replaced job delivers nothing"
    );
}

#[test]
fn history_selection_and_view_are_read_only_validated_session_state() {
    let mut session = PreviewSession::default();
    let (asset, other) = (crate::AssetId::new(), crate::AssetId::new());
    let entry = EntryId::new();
    session
        .select(&asset, HistorySelection::Entry(entry.clone()))
        .unwrap();
    assert!(!session.can_edit(&asset));
    // A selection is the named asset's alone.
    assert!(session.can_edit(&other));
    assert_eq!(session.selection(&other), HistorySelection::Current);
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
    assert!(session.can_edit(&asset));
    assert_eq!(session.selection(&asset), HistorySelection::Current);
    assert!(session.selections.is_empty());
}

/// A session holds at most `MAX_SELECTIONS` historical selections: one more is refused with
/// `resource-limit` and changes nothing, reselecting an asset already held is not one more, and
/// returning one asset to current frees its place.
#[test]
fn historical_selections_are_bounded_per_session() {
    let mut session = PreviewSession::default();
    let assets: Vec<_> = (0..=crate::MAX_SELECTIONS)
        .map(|_| crate::AssetId::new())
        .collect();
    for asset in &assets[..crate::MAX_SELECTIONS] {
        session
            .select(asset, HistorySelection::Entry(EntryId::new()))
            .unwrap();
    }
    let generation = session.generation;
    let refused = session
        .select(
            &assets[crate::MAX_SELECTIONS],
            HistorySelection::Entry(EntryId::new()),
        )
        .unwrap_err();
    assert_eq!(refused.kind, crate::ErrorKind::ResourceLimit);
    assert_eq!(session.selections.len(), crate::MAX_SELECTIONS);
    assert_eq!(session.generation, generation, "a refusal changes nothing");
    session
        .select(&assets[0], HistorySelection::Entry(EntryId::new()))
        .unwrap();
    session
        .select(&assets[0], HistorySelection::Current)
        .unwrap();
    session
        .select(
            &assets[crate::MAX_SELECTIONS],
            HistorySelection::Entry(EntryId::new()),
        )
        .unwrap();
    assert_eq!(session.selections.len(), crate::MAX_SELECTIONS);
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
    let mut job = rebuilt(job(1, true), |parts| {
        // The stock test job carries a pixel-stage layer, which is not proxy-eligible.
        parts.recipe.layers.clear();
        parts.source = PreviewSource::Raw {
            image,
            settings: LinearSettings { white_balance },
        };
    });
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

/// A job whose source approximates its white balance says so on its proxy, as a draft's tick
/// renders it, and on its exact frame, and is never reduced into a report, although it asked for
/// one. Otherwise it is an ordinary job: the proxy phase is the approximate recipe rendered
/// against the exact downscale of the developed planes, byte for byte, and the exact phase the
/// approximate recipe at full size.
#[test]
fn an_approximate_white_balance_is_labelled_on_both_phases_and_never_analysed() {
    let job = raw_job(Some(approximation()));
    assert!(job.analyse, "the job asked for a report");
    assert!(job.evaluation.source().approximate_white_balance());
    let (registry, source, recipe) = (
        job.evaluation.registry().clone(),
        job.evaluation.source().clone(),
        job.evaluation.recipe().clone(),
    );
    let snapshot = job.evaluation.entry().snapshot.id.clone();
    let plan = source
        .proxy_plan(&registry, &recipe, job.proxy.unwrap())
        .unwrap()
        .expect("a proxy is worthwhile");

    let mut queue = PreviewQueue::default();
    let [proxy, exact] = proxy_then_exact(&mut queue, job);
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
/// drafted white balance renders against the proxy a moving frame of the committed settings built
/// over the same planes — a cache hit — through its own matrix, and says so.
#[test]
fn a_drafted_white_balance_hits_the_proxy_built_over_the_same_planes() {
    let exact = raw_job(None);
    let drafted = raw_job(Some(approximation()));
    // The same developed planes: a drafted job reads the planes the committed one did.
    let source = PreviewSource::Raw {
        image: match exact.evaluation.source() {
            PreviewSource::Raw { image, .. } => image.clone(),
            PreviewSource::Jpeg(_) => unreachable!(),
        },
        settings: match drafted.evaluation.source() {
            PreviewSource::Raw { settings, .. } => *settings,
            PreviewSource::Jpeg(_) => unreachable!(),
        },
    };
    let drafted = rebuilt(drafted, |parts| parts.source = source);
    let mut queue = PreviewQueue::default();
    queue.request(moving(exact));
    let first = drain_all(&mut queue);
    assert!(
        first[0].proxy().is_some_and(|proxy| proxy.built) && !first[0].approximate_white_balance
    );
    queue.request(moving(drafted));
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
    assert!(!job.evaluation.source().approximate_white_balance());
    let mut queue = PreviewQueue::default();
    let results = proxy_then_exact(&mut queue, job);
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

/// A cached RAW proxy is the developed planes, not an edit: a second job over the same planes
/// with another Basic exposure is a cache hit that renders at its own exposure.
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
        let mut job = rebuilt(job(1, analyse), |parts| {
            // The stock test job carries a pixel-stage layer, which is not proxy-eligible; one
            // Basic exposure is the stack under test.
            parts.recipe.layers = vec![crate::Layer {
                id: crate::LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: json!({"exposure": ev}),
                mask: None,
                artifacts: Vec::new(),
            }];
            parts.source = PreviewSource::Raw {
                image: image.clone(),
                settings: LinearSettings::default(),
            };
        });
        job.proxy = Some(bounds);
        moving(job)
    };
    let mut queue = PreviewQueue::default();
    // The proxy frame of `generation`, and whether that job built the proxy.
    let proxy_of = |queue: &mut PreviewQueue, generation: u64, what: &str| {
        wait_for(what, || {
            queue
                .poll()
                .filter(|result| {
                    result.generation == generation && result.phase() == PreviewPhase::Proxy
                })
                .map(|result| {
                    let built = result.proxy().is_some_and(|proxy| proxy.built);
                    (built, result.into_raster().expect("a proxy frame"))
                })
        })
    };
    let first = queue.request(job_at(0.0, false));
    let (built, dark) = proxy_of(&mut queue, first, "the first proxy");
    assert!(built, "the first job builds the proxy");
    drain_all(&mut queue);
    let second = queue.request(job_at(1.0, false));
    let (built, bright) = proxy_of(&mut queue, second, "the second proxy");
    assert!(!built, "the same planes and bounds are a cache hit");
    assert_eq!((dark.width, dark.height), (bright.width, bright.height));
    let brighter = dark
        .rgba
        .chunks_exact(4)
        .zip(bright.rgba.chunks_exact(4))
        .filter(|(a, b)| b[0] > a[0])
        .count();
    assert!(
        brighter > (dark.width * dark.height / 2) as usize,
        "one stop more exposure brightens the cached proxy, not the cached planes"
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
/// A pixel read through a spatial segment is answered from that segment's whole frame ([performance
/// rule 4](../../../../docs/engineering/performance-rules.md#rules)), so asking it once per display
/// cell over the whole stage would render the picture's spatial layers on every overlay. That is not an overlay to ship
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
            job.evaluation.registry(),
            job.evaluation.source(),
            job.evaluation.recipe(),
            RenderOptions::default(),
            job.evaluation.context(),
        )
        .expect("the stack compiles");
        let MaskOverlayOutcome {
            grid,
            absent: reason,
        } = mask_overlay_for(
            job.evaluation.registry(),
            &frame,
            job.evaluation.recipe(),
            &request,
            None,
            &Cancel::never(),
            job.evaluation.context(),
        );
        if absent {
            assert!(grid.is_none(), "a value-based mask behind a spatial layer");
            let reason = reason.expect("the host's own reason travels with the outcome");
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

/// What one coverage grid costs, on 24 MP and 60 MP, before and after a value-based component is in
/// the mask — the measurement proposal P16 of `docs/design/range-study.md` was decided against. The
/// figures are stated against the exact render of the same frame.
///
/// The "before" figure for a value-based mask is nothing at all, because such a mask was refused a
/// grid; the geometric rows are the delivered cost of a grid and must not have moved. So the added
/// cost is the band and mixed rows.
///
/// The two grid sizes are the two a person actually asks for, from
/// `state::histogram::overlay_cells`: at Fit one cell per physical pixel of the drawn photograph,
/// and at 100% the delivered 4096-cell cap a side.
///
/// Ignored by default because it is a measurement and not a pass/fail property. Run it with
/// `cargo test --release --locked --package luxforge-core --lib -- --ignored --nocapture
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
            let registry = job.evaluation.registry().clone();
            let source = job.evaluation.source().clone();
            let recipe = job.evaluation.recipe().clone();
            let snapshot = job.evaluation.entry().snapshot.id.clone();
            // The exact render, for the figures to be stated against.
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
                let context = RenderContext::new();
                let frame = crate::render(
                    &registry,
                    &source,
                    &recipe,
                    RenderOptions::default(),
                    &context,
                )
                .expect("the stack compiles");
                let started = Instant::now();
                let MaskOverlayOutcome { grid, absent } = mask_overlay_for(
                    &registry,
                    &frame,
                    &recipe,
                    &request,
                    None,
                    &Cancel::never(),
                    &context,
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
/// evaluation holds the whole spatial target: a `render.sample` through the same layer on the
/// owner thread, which is how a committed RAW crop was once refused with "spatial
/// processing needs … bytes, and … of the … byte spatial budget is in use" and left unshown.
/// A moving job's proxy and the exact frame at rest each deliver the cropped frame, byte for byte
/// the frame the same stack renders with the target free, and every batch releases what it
/// reserved.
#[test]
fn a_cropped_raw_preview_with_presence_renders_while_the_spatial_target_is_held() {
    use crate::{LinearImage, LinearSettings, PRESENCE_EFFECT};
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
    let mut job = rebuilt(job(1, false), |parts| {
        parts.recipe.layers = vec![
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
        parts.source = PreviewSource::Raw {
            image,
            settings: LinearSettings::default(),
        };
    });
    let display = ProxyBounds {
        width: 480,
        height: 320,
    };
    job.proxy = Some(display);
    let registry = job.evaluation.registry().clone();
    let recipe = job.evaluation.recipe().clone();
    let snapshot = job.evaluation.entry().snapshot.id.clone();
    let output = registry.compile(width, height, &recipe).unwrap().stage();
    assert!(
        output.width < width && output.height < height,
        "the crop trims the stage"
    );
    let plan = job
        .evaluation
        .source()
        .proxy_plan(&registry, &recipe, display)
        .unwrap()
        .expect("a proxy is worthwhile");
    let exact_reference = job
        .evaluation
        .source()
        .render(&registry, snapshot.clone(), &recipe)
        .expect("the stack renders with the target free");
    let proxy_reference = job
        .evaluation
        .source()
        .proxy(plan)
        .unwrap()
        .render(&registry, snapshot, &recipe)
        .expect("the proxy renders with the target free");

    // The job renders through its own context; holding that context's whole target stands in for
    // the other evaluation.
    let context = job.evaluation.context().clone();
    let budget = context.spatial();
    let held = budget.reserve(budget.target(), 1);
    let mut queue = PreviewQueue::default();
    let [proxy, exact] = proxy_then_exact(&mut queue, job);
    drop(held);
    assert_eq!(budget.in_use(), 0, "every batch released its reservation");
    assert_eq!(
        (proxy.phase(), exact.phase()),
        (PreviewPhase::Proxy, PreviewPhase::Exact)
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

/// A radial component off the stage centre, soft enough that a thumbnail's cells read a gradient.
fn radial_mask() -> Mask {
    let mut mask = Mask::new("Radial");
    let name = mask.next_component_name("radial");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "radial",
        json!({"x": 0.4, "y": 0.55, "radius_x": 0.3, "radius_y": 0.2, "angle": 15.0, "feather": 0.5}),
    ));
    mask
}

/// The overlay's own whole-stage grid for `mask` over `job`, filled directly from the core's
/// reduction: what a thumbnail must equal cell for cell.
fn whole_grid(job: &PreviewJob, mask: &Mask, cells: (u32, u32)) -> Vec<u8> {
    let transform = render(
        job.evaluation.registry(),
        job.evaluation.source().input(),
        job.evaluation.recipe(),
        RenderOptions::default(),
        &RenderContext::new(),
    )
    .unwrap()
    .transform()
    .unwrap();
    let compiled = crate::mask::CompiledMask::new(
        mask,
        crate::modules::Stage {
            width: transform.content.width,
            height: transform.content.height,
        },
        &job.evaluation.recipe().strokes,
    )
    .unwrap();
    crate::analysis::coverage_grid(
        &compiled,
        &transform,
        cells.0,
        cells.1,
        crate::analysis::MaskPixels::Unavailable("geometric mask"),
        &Cancel::never(),
    )
    .unwrap()
    .unwrap()
}

/// A mask thumbnail is the overlay's grid at the thumbnail's cells — a linear, and a radial, behind
/// a turned and straightened crop whose cells map back through the geometry tail — and it describes
/// the whole mask rather than one component of it.
#[test]
fn a_mask_thumbnail_is_the_overlay_grid_at_its_cells() {
    let linear = gradient_mask(0.6);
    let radial = radial_mask();
    let mut layers = eligible_layers(96, 64);
    layers.extend(masked_basic(&radial));
    layers.extend(masked_basic(&linear));
    let job = stacked_with_masks(96, 64, layers, vec![linear.clone(), radial.clone()], None);
    for mask in [&linear, &radial] {
        let coverage = job
            .evaluation
            .mask_coverage(&mask.id, (28, 19), None, &Cancel::never())
            .unwrap();
        let outcome = coverage.outcome.expect("nothing was cached");
        assert_eq!(outcome.absent, None);
        let grid = outcome.grid.expect("a geometric mask has a grid");
        assert_eq!((grid.mask.clone(), grid.component), (mask.id.clone(), None));
        assert_eq!((grid.cells_w, grid.cells_h), (28, 19));
        assert_eq!(grid.coverage, whole_grid(&job, mask, (28, 19)));
        assert!(
            grid.coverage.iter().any(|cell| *cell > 0) && grid.coverage.contains(&0),
            "the fixture covers part of the frame"
        );
    }
}

/// The key names everything the grid depends on and nothing else: the key already held fills no
/// cell, an exposure a position-only mask cannot see keeps it, and a moved mask is a new key.
#[test]
fn a_mask_coverage_key_moves_only_with_what_its_grid_depends_on() {
    let mask = gradient_mask(0.6);
    let job = stacked_with_masks(64, 48, masked_basic(&mask), vec![mask.clone()], None);
    let first = job
        .evaluation
        .mask_coverage(&mask.id, (28, 19), None, &Cancel::never())
        .unwrap();
    assert!(first.outcome.is_some());
    let again = job
        .evaluation
        .mask_coverage(&mask.id, (28, 19), Some(first.key), &Cancel::never())
        .unwrap();
    assert_eq!(again.key, first.key);
    assert_eq!(again.outcome, None, "an unchanged mask is not filled again");

    // Another exposure on the masked layer changes the picture, not the mask.
    let mut layers = masked_basic(&mask);
    layers[0].payload = json!({"exposure": -1.5});
    let edited = stacked_with_masks(64, 48, layers, vec![mask.clone()], None);
    let kept = edited
        .evaluation
        .mask_coverage(&mask.id, (28, 19), Some(first.key), &Cancel::never())
        .unwrap();
    assert_eq!((kept.key, kept.outcome), (first.key, None));

    // The same mask, lengthened, is a different grid.
    let mut moved = gradient_mask(0.9);
    moved.id = mask.id.clone();
    let job = stacked_with_masks(64, 48, masked_basic(&moved), vec![moved.clone()], None);
    let changed = job
        .evaluation
        .mask_coverage(&moved.id, (28, 19), Some(first.key), &Cancel::never())
        .unwrap();
    assert_ne!(changed.key, first.key);
    assert_eq!(
        changed.outcome.unwrap().grid.unwrap().coverage,
        whole_grid(&job, &moved, (28, 19))
    );
}

/// A mask that reads pixels is answered on its first bound layer's input, and has no thumbnail —
/// with the host's own reason — while no layer is bound to it: the overlay's rule, not a grid read
/// from anything else. A layer before the bound one is part of its key.
#[test]
fn a_value_based_thumbnail_needs_the_masked_operation_input() {
    let mut mask = Mask::new("Shadows");
    let name = mask.next_component_name("luminance-range");
    mask.components.push(Component::new(
        name,
        ComponentMode::Add,
        "luminance-range",
        json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0}),
    ));
    let unbound = stacked_with_masks(32, 24, Vec::new(), vec![mask.clone()], None);
    let outcome = unbound
        .evaluation
        .mask_coverage(&mask.id, (28, 19), None, &Cancel::never())
        .unwrap()
        .outcome
        .unwrap();
    assert_eq!(outcome.grid, None);
    assert!(outcome.absent.is_some(), "the refusal is named");

    let bound = stacked_with_masks(32, 24, masked_basic(&mask), vec![mask.clone()], None);
    let coverage = bound
        .evaluation
        .mask_coverage(&mask.id, (28, 19), None, &Cancel::never())
        .unwrap();
    let grid = coverage
        .outcome
        .clone()
        .unwrap()
        .grid
        .expect("the input is available");
    assert_eq!(grid.coverage.len(), 28 * 19);
    let mut before = vec![Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"exposure": 1.0}),
        mask: None,
        artifacts: Vec::new(),
    }];
    before.extend(masked_basic(&mask));
    let brighter = stacked_with_masks(32, 24, before, vec![mask.clone()], None);
    let moved = brighter
        .evaluation
        .mask_coverage(&mask.id, (28, 19), Some(coverage.key), &Cancel::never())
        .unwrap();
    assert_ne!(moved.key, coverage.key);
}

/// A cancelled grid is an error, never an absent grid a caller could keep as the answer.
#[test]
fn a_cancelled_mask_coverage_is_an_error() {
    let mask = gradient_mask(0.6);
    let job = stacked_with_masks(32, 24, masked_basic(&mask), vec![mask.clone()], None);
    let cancel = Cancel::new();
    cancel.cancel();
    let error = job
        .evaluation
        .mask_coverage(&mask.id, (28, 19), None, &cancel)
        .unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Cancelled);
}

#[test]
fn the_view_frame_reduces_final_pixels_and_reduce_only_shares_full_raster() {
    let layer = Layer::new(
        crate::DETAIL_EFFECT,
        json!({"sharpening":50,"luminance":40,"colour":40}),
    );
    let mut wanted = stacked(
        129,
        97,
        vec![layer],
        Some(ProxyBounds {
            width: 37,
            height: 31,
        }),
    );
    wanted.analyse = true;
    wanted.intent = PreviewIntent::Settle;
    let mut queue = PreviewQueue::default();
    queue.request(wanted.clone());
    let settled = drain_all(&mut queue);
    assert_eq!(settled.len(), 1, "the committed stack runs no proxy phase");
    let exact = settled[0].exact().unwrap();
    let full = exact.result.as_ref().unwrap();
    let displayed = exact.display.as_ref().unwrap();
    let plan = crate::ProxyPlan::fit((129, 97), (129, 97), wanted.proxy.unwrap()).unwrap();
    let reference = crate::proxy::reduce_raster(full, plan, &Cancel::never()).unwrap();
    assert_eq!(displayed.rgba, reference.rgba);
    assert_eq!(
        exact.report.as_ref().unwrap(),
        &crate::analysis::reduce(&full.rgba, 129, 97, &Cancel::never()).unwrap()
    );
    let full = Arc::new(full.clone());
    wanted.reduce = Some(full.clone());
    wanted.proxy = Some(ProxyBounds {
        width: 29,
        height: 17,
    });
    wanted.intent = PreviewIntent::Reduce;
    queue.request(wanted.clone());
    let resized = drain_all(&mut queue);
    assert_eq!(resized.len(), 1);
    let resized = resized[0].exact().unwrap();
    assert!(resized.report.is_none(), "resizing does not rerun analysis");
    assert!(Arc::ptr_eq(
        &full.rgba,
        &resized.result.as_ref().unwrap().rgba
    ));
    let plan = crate::ProxyPlan::fit((129, 97), (129, 97), wanted.proxy.unwrap()).unwrap();
    assert_eq!(
        resized.display.as_ref().unwrap().rgba,
        crate::proxy::reduce_raster(&full, plan, &Cancel::never())
            .unwrap()
            .rgba
    );
    wanted.reduce = Some(Arc::new(crate::Raster {
        snapshot_id: SnapshotId::new(),
        ..(*full).clone()
    }));
    queue.request(wanted);
    assert!(
        drain_all(&mut queue)[0].exact().unwrap().result.is_err(),
        "an unrelated exact allocation is refused"
    );
}

#[test]
fn the_view_frame_is_every_whole_stacks_and_excludes_partial_region_moving_and_approximate_frames()
{
    let detail = Layer::new(crate::DETAIL_EFFECT, json!({"luminance":30}));
    let mut whole = stacked(64, 48, vec![detail.clone()], Some(bounds(16, 12)));
    whole.intent = PreviewIntent::Settle;
    let raster = whole
        .evaluation
        .exact(&Cancel::never())
        .unwrap()
        .frame(whole.evaluation.entry().snapshot.id.clone())
        .unwrap();
    let result = Ok(raster);
    assert!(
        super::worker::view_frame(&whole, &result, &Cancel::never())
            .unwrap()
            .is_some()
    );
    // A stack with no restoration is reduced as well: the reference frame of every whole stack the
    // view draws smaller than it is comes from its exact frame.
    let basic = stacked(
        64,
        48,
        vec![Layer::new(BASIC_EFFECT, json!({"exposure":0.5}))],
        Some(bounds(16, 12)),
    );
    let basic_result = basic
        .evaluation
        .exact(&Cancel::never())
        .unwrap()
        .frame(basic.evaluation.entry().snapshot.id.clone());
    assert!(
        super::worker::view_frame(&basic, &basic_result, &Cancel::never())
            .unwrap()
            .is_some(),
        "a stack without Detail has its view frame too"
    );

    let mut partial = whole.clone();
    partial.layer_count = Some(1);
    let mut viewport = whole.clone();
    viewport.viewport = Some(crate::Region {
        x0: 0,
        y0: 0,
        width: 16,
        height: 12,
    });
    let mut moving = whole.clone();
    moving.intent = PreviewIntent::Interactive;
    let mut fits = whole.clone();
    fits.proxy = Some(bounds(64, 48));
    let mut unbounded = whole.clone();
    unbounded.proxy = None;
    let mut approximate = rebuilt(raw_job(Some(approximation())), |parts| {
        parts.recipe.layers.push(detail)
    });
    approximate.intent = PreviewIntent::Settle;
    for (name, job) in [
        ("truncated", partial),
        ("viewport", viewport),
        ("interactive", moving),
        ("scale one", fits),
        ("unbounded", unbounded),
        ("approximate white balance", approximate.clone()),
    ] {
        assert!(
            super::worker::view_frame(&job, &result, &Cancel::never())
                .unwrap()
                .is_none(),
            "{name}"
        );
    }
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert_eq!(
        super::worker::view_frame(&whole, &result, &cancelled)
            .unwrap_err()
            .kind,
        crate::ErrorKind::Cancelled
    );

    // Even a correctly tagged retained RAW raster cannot become a view frame when
    // its source settings approximate a drafted white balance.
    let raw = approximate
        .evaluation
        .exact(&Cancel::never())
        .unwrap()
        .frame(approximate.evaluation.entry().snapshot.id.clone())
        .unwrap();
    approximate.intent = PreviewIntent::Reduce;
    approximate.reduce = Some(Arc::new(raw));
    let mut queue = PreviewQueue::default();
    queue.request(approximate);
    let answers = drain_all(&mut queue);
    let exact = answers[0].exact().unwrap();
    assert_eq!(
        exact.result.as_ref().unwrap_err().kind,
        crate::ErrorKind::Validation
    );
    assert!(exact.display.is_none() && exact.report.is_none());
}

#[test]
fn a_superseded_settle_returns_no_view_frame_or_report() {
    let gate = Arc::new(luxforge_testbase::Gate::new());
    let mut older = rebuilt(held(&gate, Some(bounds(16, 12))), |parts| {
        parts
            .recipe
            .layers
            .insert(0, Layer::new(crate::DETAIL_EFFECT, json!({"luminance":30})));
    });
    older.intent = PreviewIntent::Settle;
    older.analyse = true;
    let mut queue = PreviewQueue::default();
    gate.shut();
    let first = queue.request(older.clone());
    gate.wait_reached(1, "the Detail settlement colour pass");
    let second = queue.request(older);
    gate.open();
    let answers = drain_all(&mut queue);
    assert_eq!(answers.len(), 2);
    assert_eq!(
        (answers[0].generation, answers[1].generation),
        (first, second)
    );
    let cancelled = answers[0].exact().unwrap();
    assert_eq!(
        cancelled.result.as_ref().unwrap_err().kind,
        crate::ErrorKind::Cancelled
    );
    assert!(cancelled.display.is_none() && cancelled.report.is_none());
    let current = answers[1].exact().unwrap();
    assert!(current.result.is_ok() && current.display.is_some() && current.report.is_some());
}
