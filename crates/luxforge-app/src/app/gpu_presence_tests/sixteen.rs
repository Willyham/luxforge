//! As many masked spatial layers as a recipe may hold, of mixed shapes: masked Detail and Presence
//! layers of several unit sets, through masks of every kind, Basic layers between them. Each layer
//! is a link of the plan's chain, so no limit of one operation binds; every link compiles; a drag
//! of each layer finds every link it draws in the warm list the desktop hands the surface; and each
//! tick's frame is the whole evaluation's, bit for bit.
use super::{photograph, stage};
use crate::app::gpu_plan::surface_plan;
use crate::app::gpu_qualification::{headless, held_to_whole, lit_fixed};
use luxforge_core::{
    BASIC_EFFECT, Component, ComponentMode, DETAIL_EFFECT, GPU_PLAN_LINKS, GPU_PROGRAMS,
    GPU_WARM_LINKS, GpuAnswer, GpuPlanRequest, GpuProgramKind, Layer, MASKS_PER_RECIPE,
    MAX_MASKED_SPATIAL_LAYERS, Mask, ModuleRegistry, PRESENCE_EFFECT, Recipe, gpu_plan,
    mask::Stroke, path::StrokeTable,
};
use luxforge_gpu::{
    BoundaryFormat, GPU_PREVIEW_BUDGET, GpuBoundary, GpuPlan, GpuStep, PIPELINE_CACHE,
    qualification::boundary, qualification::unwarmed_links,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The mask kinds the recipe's masks take in turn.
const KINDS: [&str; 5] = [
    "radial",
    "linear",
    "luminance-range",
    "colour-range",
    "brush",
];
/// How many of the masks hold masked Detail; the rest hold masked Presence.
const DETAILS: usize = 6;

/// The `index`-th mask, of the kind [`KINDS`] gives it, placed across the frame; a brush's stroke
/// goes into `strokes`.
fn mask_of(index: usize, strokes: &mut StrokeTable) -> Mask {
    let kind = KINDS[index % KINDS.len()];
    let t = index as f64 / MAX_MASKED_SPATIAL_LAYERS as f64;
    let payload = match kind {
        "radial" => json!({"x": 0.15 + 0.7 * t, "y": 0.45, "radius_x": 0.16, "radius_y": 0.13,
                           "angle": 12.0, "feather": 50.0}),
        "linear" => json!({"x0": 0.1, "y0": 0.15 + 0.6 * t, "x1": 0.55, "y1": 0.85}),
        "luminance-range" => json!({"low": 5.0 + 40.0 * t, "low_feather": 15.0,
                                    "high": 60.0 + 30.0 * t, "high_feather": 15.0}),
        "colour-range" => json!({"samples": [[0.2 + 0.4 * t, 0.25, 0.1]], "refine": 40.0}),
        _ => {
            let stroke = Stroke::capture(
                &[[0.15, 0.2 + 0.5 * t], [0.5, 0.35], [0.85, 0.75]],
                0.12,
                50.0,
                100.0,
                false,
            )
            .expect("a stroke");
            json!({"strokes": [strokes.insert(stroke).to_string()]})
        }
    };
    let mut mask = Mask::new(format!("Mask {}", index + 1));
    let name = mask.next_component_name(kind);
    mask.components
        .push(Component::new(name, ComponentMode::Add, kind, payload));
    mask
}

/// Each masked Detail layer's values, in turn: sharpening, sharpening with luminance noise
/// reduction, and noise reduction alone.
fn detail(index: usize) -> Value {
    [
        json!({"sharpening": 45}),
        json!({"sharpening": 30, "luminance": 25}),
        json!({"luminance": 35, "colour": 30}),
    ][index % 3]
        .clone()
}

/// Each masked Presence layer's values, in turn: Texture and Clarity, Clarity alone, and all three
/// fields.
fn presence(index: usize) -> Value {
    [
        json!({"texture": 40, "clarity": 30}),
        json!({"clarity": 50}),
        json!({"texture": 25, "clarity": -20, "dehaze": 20}),
    ][index % 3]
        .clone()
}

/// A recipe of [`MAX_MASKED_SPATIAL_LAYERS`] masks, each holding a masked exposure and one masked
/// spatial layer — Detail on the first [`DETAILS`], Presence on the rest — in the order placement
/// gives them: the masked Detail layers, a global Basic layer and the masked Basic layers, which
/// the last Detail link holds after its own step, then the masked Presence layers.
fn sixteen() -> Recipe {
    let mut strokes = StrokeTable::new("sixteen masked spatial layers");
    let masks: Vec<Mask> = (0..MAX_MASKED_SPATIAL_LAYERS)
        .map(|index| mask_of(index, &mut strokes))
        .collect();
    let masked = |effect: &str, payload: Value, mask: &Mask| Layer {
        mask: Some(mask.id.clone()),
        ..Layer::new(effect, payload)
    };
    let mut layers: Vec<Layer> = masks[..DETAILS]
        .iter()
        .enumerate()
        .map(|(index, mask)| masked(DETAIL_EFFECT, detail(index), mask))
        .collect();
    layers.push(Layer::new(BASIC_EFFECT, json!({"exposure": 0.2})));
    layers.extend(
        masks
            .iter()
            .map(|mask| masked(BASIC_EFFECT, json!({"exposure": 0.3}), mask)),
    );
    layers.extend(
        masks[DETAILS..]
            .iter()
            .enumerate()
            .map(|(index, mask)| masked(PRESENCE_EFFECT, presence(index), mask)),
    );
    Recipe {
        layers,
        masks,
        strokes,
        ..Recipe::default()
    }
}

/// The surface's own compile cache holds the largest plan's links and a whole warm list at once:
/// the core bounds the warm list to the cache less the largest plan, whose links are a content
/// link and one for each spatial operation, a global layer of each spatial effect and every masked
/// spatial layer, and the light link its lights are computed with.
#[test]
fn gpu_preview_the_largest_plan_and_a_warm_list_fit_the_compile_cache() {
    let spatial_effects = GPU_PROGRAMS
        .iter()
        .filter(|program| program.kind == GpuProgramKind::Spatial)
        .count();
    assert_eq!(
        GPU_PLAN_LINKS,
        1 + spatial_effects + MAX_MASKED_SPATIAL_LAYERS + 1
    );
    assert_eq!(GPU_WARM_LINKS + GPU_PLAN_LINKS, PIPELINE_CACHE);
    const { assert!(MAX_MASKED_SPATIAL_LAYERS <= MASKS_PER_RECIPE) };
}

/// [`sixteen`]'s plan at a Fit stage, the drags of layers of every shape in turn, each tick one
/// value that crosses no zero: every frame is the whole evaluation's, bit for bit, and so with the
/// poison on. Each masked spatial layer is a link of its own, sixteen links, so the plan meets no
/// limit of one operation: no `spatial-chain`, each operation's applies reading at most four
/// planes. Every link compiles, and the slot holding the stack at a 24 MP photograph's full-screen
/// Fit stage on a JPEG fits the GPU-preview budget, at about 1.01 GB.
#[test]
fn gpu_presence_sixteen_masked_layers_of_mixed_shapes_draw_what_a_whole_evaluation_does() {
    let test =
        "gpu_presence_sixteen_masked_layers_of_mixed_shapes_draw_what_a_whole_evaluation_does";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (480, 320);
    let pixels = photograph(width, height, 16);
    let held = boundary(width, height, 1, &pixels).expect("a boundary");
    let start = sixteen();
    let request =
        GpuPlanRequest::fit(0, stage(width, height), stage(width * 4, height * 4)).qualifying();
    let planned =
        |stack: &Recipe| match gpu_plan(&registry, stack, request).expect("the stack compiles") {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
    let first = planned(&start);
    assert_eq!(first.spatial.len(), MAX_MASKED_SPATIAL_LAYERS);
    let planes: Vec<usize> = first
        .spatial
        .iter()
        .map(|spatial| spatial.applies.iter().map(|apply| apply.planes.len()).sum())
        .collect();
    assert!(planes.iter().all(|planes| *planes <= 4), "{planes:?}");
    let surface = surface_plan(&first, held.clone()).expect("a runnable plan");
    let links = surface
        .steps
        .iter()
        .filter(|step| matches!(step, GpuStep::Spatial(_)))
        .count();
    assert_eq!(links, MAX_MASKED_SPATIAL_LAYERS, "a link each, none before");
    let compiled = qualifier
        .compile_time(&surface.steps)
        .expect("every link compiles");
    let passes = qualifier.pass_pipelines_created();
    // What the slot charges for the same stack at a 24 MP photograph's full-screen Fit stage on a
    // JPEG, within the GPU-preview budget.
    let (fit_width, fit_height) = (2292, 1528);
    let fit = GpuBoundary::new(
        Arc::new(vec![0u8; (fit_width * fit_height) as usize * 8]),
        fit_width,
        fit_height,
        1,
        BoundaryFormat::Half,
    )
    .expect("a boundary");
    let full = GpuPlanRequest::fit(0, stage(fit_width, fit_height), stage(6000, 4000));
    let at_fit = match gpu_plan(&registry, &start, full).expect("the stack compiles") {
        GpuAnswer::Plan(plan) => surface_plan(&plan, fit).expect("a runnable plan"),
        GpuAnswer::Fallback(reason) => panic!("{reason}"),
    };
    let charged = qualifier.charged_bytes(&at_fit).expect("a charge");
    eprintln!(
        "{test}: {links} links compiled in {} ms, {passes} pass pipelines; apply planes per \
         operation {planes:?}; the slot at a {fit_width}x{fit_height} Fit stage {charged} B",
        compiled.as_millis()
    );
    assert!(charged <= GPU_PREVIEW_BUDGET, "{charged} B");
    // Each tick after the first: the layer its drag moves, the field and the value.
    let index_of = |effect: &str, nth: usize| {
        start
            .layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.effect_id == effect && layer.mask.is_some())
            .nth(nth)
            .map(|(index, _)| index)
            .expect("the layer")
    };
    let drags: [(usize, &str, f64); 8] = [
        (index_of(DETAIL_EFFECT, 1), "sharpening", 40.0),
        (index_of(DETAIL_EFFECT, 1), "sharpening", 50.0),
        (index_of(PRESENCE_EFFECT, 0), "clarity", 45.0),
        (index_of(BASIC_EFFECT, 4), "exposure", 0.6),
        (index_of(PRESENCE_EFFECT, 9), "texture", 60.0),
        (index_of(PRESENCE_EFFECT, 5), "dehaze", 35.0),
        (index_of(DETAIL_EFFECT, 5), "colour", 15.0),
        (index_of(PRESENCE_EFFECT, 9), "texture", 30.0),
    ];
    let mut stack = start.clone();
    let mut ticks: Vec<(GpuPlan, Option<[u32; 4]>)> = vec![(surface, None)];
    for (layer, field, value) in drags {
        stack.layers[layer].payload[field] = json!(value);
        let plan = surface_plan(&planned(&stack), held.clone()).expect("a runnable plan");
        ticks.push((plan, None));
    }
    // Every tick held to its whole evaluation, both reading one light, whichever it is.
    lit_fixed(&qualifier, &ticks[0].0);
    let whole: Vec<_> = ticks
        .iter()
        .map(|(plan, _)| qualifier.evaluate(plan).expect("a readback"))
        .collect();
    let name = |number: usize| match number {
        0 => "the first tick".to_owned(),
        _ => {
            let (layer, field, value) = drags[number - 1];
            format!("tick {number}, layer {layer}'s {field} at {value}")
        }
    };
    let passes = held_to_whole(&qualifier, &ticks, &whole, &name);
    for (number, ran) in passes.iter().enumerate() {
        eprintln!("{test}: {}: {ran:?} passes", name(number));
    }
    // Each link's passes, tick by tick: every pass first, Detail's 4, 14 or 13 by its units and
    // Presence's 13, 5 or 20. A link the tick leaves alone runs none, the link it changes the passes
    // its words change — sharpening its 4, noise reduction all 13, Clarity none, Texture 5, Dehaze
    // all 20 when a later link holding Dehaze's planes wrote its scratch since, a colour step after a
    // Detail step none of Detail's — and every link after it, whose input moved, all of its own.
    const EVERY: [u64; 16] = [4, 14, 13, 4, 14, 13, 13, 5, 20, 13, 5, 20, 13, 5, 20, 13];
    let after = |link: usize, ran: u64| {
        let mut passes = EVERY;
        passes[..link].fill(0);
        passes[link] = ran;
        passes.to_vec()
    };
    let wanted = [
        EVERY.to_vec(),
        after(1, 4),
        after(1, 4),
        after(6, 0),
        after(5, 0),
        after(15, 5),
        after(11, 20),
        after(5, 13),
        after(15, 5),
    ];
    assert_eq!(passes, wanted);
}

/// The mask kinds the desktop's masks take in turn, each through its own command.
const DESKTOP_KINDS: [&str; 4] = ["radial", "linear", "luminance-range", "brush"];

/// Another client commits the `index`-th mask, of the kind [`DESKTOP_KINDS`] gives it, a masked
/// exposure through it and its masked spatial layer, Detail on the first [`DETAILS`] and Presence
/// on the rest; answers the mask's identity.
fn desktop_mask(
    editor: &mut crate::app::Editor,
    asset: &luxforge_core::AssetId,
    agent: luxforge_core::ClientId,
    index: usize,
) -> luxforge_core::MaskId {
    use crate::app::gpu_window_tests::answered;
    let t = index as f64 / MAX_MASKED_SPATIAL_LAYERS as f64;
    let (method, geometry) = match DESKTOP_KINDS[index % DESKTOP_KINDS.len()] {
        "radial" => (
            "mask.create-radial",
            json!({"x": 0.15 + 0.7 * t, "y": 0.45, "radius_x": 0.16, "radius_y": 0.13,
                   "angle": 12.0, "feather": 50.0}),
        ),
        "linear" => (
            "mask.create-linear",
            json!({"x0": 0.1, "y0": 0.15 + 0.6 * t, "x1": 0.55, "y1": 0.85}),
        ),
        "luminance-range" => (
            "mask.create-luminance-range",
            json!({"low": 5.0 + 40.0 * t, "low_feather": 15.0, "high": 60.0 + 30.0 * t,
                   "high_feather": 15.0}),
        ),
        _ => (
            "mask.add-stroke",
            json!({"points": [[0.15, 0.2 + 0.5 * t], [0.5, 0.35], [0.85, 0.75]], "size": 0.12,
                   "feather": 50.0, "flow": 100.0, "erase": false}),
        ),
    };
    let mask = answered(editor, asset, agent, method, geometry)["mask"].clone();
    let exposure = json!({"mask": mask, "exposure": 0.3});
    answered(editor, asset, agent, "edit.set-basic", exposure);
    let (method, mut values) = if index < DETAILS {
        ("edit.set-detail", detail(index))
    } else {
        ("edit.set-presence", presence(index - DETAILS))
    };
    values["mask"] = mask.clone();
    answered(editor, asset, agent, method, values);
    serde_json::from_value(mask).expect("a mask identity")
}

/// Sixteen masked spatial layers committed through the desktop, Detail and Presence of several unit
/// sets through masks of four kinds, each with a masked exposure. The committed stack's warm list,
/// as the desktop hands it to the surface, holds every link a drag of each of the sixteen layers
/// draws, keyed as the surface's compile cache keys them, so each drag begins on the GPU over a
/// warm cache, which holds the warm list and the largest plan together. Each drag's plan holds a
/// link for each layer. A drag of one draws on the GPU once compiled: every link of its plan
/// compiles, and its ticks are the whole evaluation's, bit for bit, the poison on too.
#[test]
fn gpu_presence_a_drag_of_each_of_sixteen_masked_layers_finds_its_links_warmed() {
    use crate::app::gpu_preview::SurfaceReport;
    use crate::app::gpu_window_tests::deliver_until;
    use crate::app::message::{Message, draft::DraftMessage};
    use crate::app::testing::{finish, real_photo, slide};
    let test = "gpu_presence_a_drag_of_each_of_sixteen_masked_layers_finds_its_links_warmed";
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-gpu-sixteen-{}.sqlite",
        std::process::id()
    ));
    let (mut editor, asset, agent) = real_photo(&catalog);
    let masks: Vec<luxforge_core::MaskId> = (0..MAX_MASKED_SPATIAL_LAYERS)
        .map(|index| desktop_mask(&mut editor, &asset, agent, index))
        .collect();
    // The committed stack read back at the desktop's own bounds, whose job carries the plans its
    // gestures are likely to draw.
    let refreshed = crate::app::tasks::refresh(
        &editor.owner,
        editor.client,
        asset.clone(),
        crate::app::tasks::Scope::Open,
        editor.proxy_bounds(),
    )
    .expect("the stack is read");
    let _ = editor.update(Message::Sync(
        crate::app::message::sync::SyncMessage::Refreshed(Ok(Box::new(refreshed))),
    ));
    deliver_until(&mut editor, "the committed stack's frame", |editor| {
        !editor.presentation.queue.is_busy()
    });
    let warm = editor.gpu.warm().expect("a warm list").clone();
    eprintln!(
        "{test}: the warm list holds {} plans",
        warm.sequences().len()
    );
    editor.gpu.surface = Some(SurfaceReport::default());
    let qualifier = headless(test);
    let mut dragged: Vec<GpuPlan> = Vec::new();
    for (index, mask) in masks.iter().enumerate() {
        let (action, parameter) = if index < DETAILS {
            ("set-detail", "sharpening")
        } else {
            ("set-presence", "clarity")
        };
        editor.session.workspace.mode = luxforge_core::MASK_MODE.into();
        editor.mask_panel.selected_mask = Some(mask.clone());
        let mut plans = Vec::new();
        for value in [20.0, 25.0, 30.0, 35.0] {
            let _ = slide(&mut editor, action, parameter, value);
            if !editor.gpu.holds_boundary() {
                deliver_until(&mut editor, "the boundary", |editor| {
                    editor.gpu.holds_boundary()
                });
                let _ = slide(&mut editor, action, parameter, value);
            }
            let (plan, _) = editor
                .gpu
                .surface_plan()
                .expect("the tick's plan, handed to the surface");
            plans.push(plan.clone());
        }
        let spatial = plans[0]
            .steps
            .iter()
            .filter(|step| matches!(step, GpuStep::Spatial(_)))
            .count();
        assert_eq!(spatial, MAX_MASKED_SPATIAL_LAYERS, "mask {index}");
        for plan in &plans {
            assert_eq!(
                unwarmed_links(&warm, plan),
                Vec::<usize>::new(),
                "mask {index}: every link of its drag is warmed"
            );
        }
        let _ = editor.update(Message::Draft(DraftMessage::Cancel));
        if index == DETAILS {
            dragged = plans;
        }
    }
    finish(editor, catalog);
    let Some(qualifier) = qualifier else {
        return;
    };
    // The first masked Presence layer's drag: every link compiles, then its ticks in turn.
    let compiled = qualifier
        .compile_time(&dragged[0].steps)
        .expect("every link compiles");
    eprintln!(
        "{test}: a drag's {MAX_MASKED_SPATIAL_LAYERS} links compiled in {} ms, {} pass pipelines",
        compiled.as_millis(),
        qualifier.pass_pipelines_created()
    );
    let ticks: Vec<(GpuPlan, Option<[u32; 4]>)> =
        dragged.into_iter().map(|plan| (plan, None)).collect();
    // Every tick held to its whole evaluation, both reading one light, whichever it is.
    lit_fixed(&qualifier, &ticks[0].0);
    let whole: Vec<_> = ticks
        .iter()
        .map(|(plan, _)| qualifier.evaluate(plan).expect("a readback"))
        .collect();
    let passes = held_to_whole(&qualifier, &ticks, &whole, &|number| {
        format!("the Presence drag's tick {number}")
    });
    for (number, ran) in passes.iter().enumerate() {
        eprintln!("{test}: tick {number}: {ran:?} passes");
    }
    // The drafted layer holds Dehaze at its identity, so it runs Texture's and Clarity's 13 passes
    // first and none for a Clarity drag; every link after it runs all of its own.
    let every = [4, 14, 13, 4, 14, 13, 13, 5, 20, 13, 5, 20, 13, 5, 20, 13];
    let mut moved = every;
    moved[..=DETAILS].fill(0);
    assert_eq!(passes, [every, moved, moved, moved].map(|ran| ran.to_vec()));
    // Every plan of the warm list compiles, and compiled again creates no pass pipeline: the pass
    // cache holds every pass module the warm list and the drag run.
    let compile_all = || {
        for (steps, _) in warm.sequences() {
            qualifier
                .compile_time(steps)
                .expect("a warmed plan compiles");
        }
        qualifier.pass_pipelines_created()
    };
    let once = compile_all();
    let again = compile_all();
    eprintln!("{test}: the drag and the warm list run {once} pass pipelines");
    assert_eq!(again, once, "compiled again, nothing new");
}
