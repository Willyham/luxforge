//! The warm list over a recipe of as many masked spatial layers as it may hold, of mixed shapes:
//! one plan for each distinct drafted shape, so the drag of every masked spatial layer finds each
//! link it draws warmed, within the link bound that keeps the list and the largest plan inside the
//! surface's compile cache together.
use super::*;
use crate::{
    Component, ComponentMode, DETAIL_EFFECT, GPU_PLAN_LINKS, GPU_WARM_LINKS,
    MAX_MASKED_SPATIAL_LAYERS, Mask, PRESENCE_EFFECT, mask::Stroke, path::StrokeTable,
    render::gpu::warm_links,
};

/// The mask kinds the recipe's masks take in turn.
const KINDS: [&str; 5] = [
    "radial",
    "linear",
    "luminance-range",
    "colour-range",
    "brush",
];

/// The `index`-th mask across the frame, of one component of the kind [`KINDS`] gives it; with
/// `shaped`, every mask after the fifth adds components of other kinds, so no two masks are of one
/// shape. A brush's stroke goes into `strokes`.
fn mask_of(index: usize, shaped: bool, strokes: &mut StrokeTable) -> Mask {
    let mut kinds = vec![KINDS[index % KINDS.len()]];
    if shaped {
        // The kinds after the first by one or two places, as the mask's place in the list asks.
        kinds.extend((1..=index / KINDS.len()).map(|step| KINDS[(index + step) % KINDS.len()]));
    }
    let mut mask = Mask::new(format!("Mask {}", index + 1));
    for kind in kinds {
        let name = mask.next_component_name(kind);
        let payload = component(kind, index, strokes);
        mask.components
            .push(Component::new(name, ComponentMode::Add, kind, payload));
    }
    mask
}

/// The geometry of one component of `kind` for the `index`-th mask.
fn component(kind: &str, index: usize, strokes: &mut StrokeTable) -> Value {
    let t = index as f64 / MAX_MASKED_SPATIAL_LAYERS as f64;
    match kind {
        "radial" => json!({"x": 0.2 + 0.6 * t, "y": 0.4, "radius_x": 0.15, "radius_y": 0.12,
                           "angle": 10.0, "feather": 50.0}),
        "linear" => json!({"x0": 0.1, "y0": 0.2 + 0.5 * t, "x1": 0.6, "y1": 0.8}),
        "luminance-range" => json!({"low": 10.0 + 40.0 * t, "low_feather": 10.0,
                                    "high": 70.0 + 20.0 * t, "high_feather": 10.0}),
        "colour-range" => json!({"samples": [[0.18 + 0.3 * t, 0.2, 0.12]], "refine": 50.0}),
        "brush" => {
            let stroke =
                Stroke::capture(&[[0.2, 0.2 + 0.4 * t], [0.8, 0.7]], 0.1, 50.0, 100.0, false)
                    .expect("a stroke");
            json!({"strokes": [strokes.insert(stroke).to_string()]})
        }
        other => panic!("no mask kind {other}"),
    }
}

/// A recipe of [`MAX_MASKED_SPATIAL_LAYERS`] masks ([`mask_of`]), each holding a masked Basic
/// exposure and one masked spatial layer: masked Detail on the first six, of three unit sets, then
/// masked Presence of three unit sets, Dehaze among them, in the order placement gives them —
/// Detail, a global Basic layer and the masked Basic layers, which the last Detail link holds after
/// its own step, then Presence.
fn sixteen(shaped: bool) -> Recipe {
    let mut strokes = StrokeTable::new("the 16-layer warm list");
    let masks: Vec<Mask> = (0..MAX_MASKED_SPATIAL_LAYERS)
        .map(|index| mask_of(index, shaped, &mut strokes))
        .collect();
    let masked = |effect: &str, payload: Value, mask: &Mask| Layer {
        mask: Some(mask.id.clone()),
        ..Layer::new(effect, payload)
    };
    let details = [
        json!({"sharpening": 40}),
        json!({"sharpening": 30, "luminance": 20}),
        json!({"luminance": 30, "colour": 20}),
    ];
    let presences = [
        json!({"texture": 40, "clarity": 30}),
        json!({"clarity": 50}),
        json!({"texture": 20, "clarity": 10, "dehaze": 15}),
    ];
    let split = 6;
    let mut layers: Vec<Layer> = masks[..split]
        .iter()
        .enumerate()
        .map(|(index, mask)| masked(DETAIL_EFFECT, details[index % 3].clone(), mask))
        .collect();
    layers.push(basic(json!({"exposure": 0.2})));
    layers.extend(
        masks
            .iter()
            .map(|mask| masked(BASIC_EFFECT, json!({"exposure": 0.3}), mask)),
    );
    layers.extend(
        masks[split..]
            .iter()
            .enumerate()
            .map(|(index, mask)| masked(PRESENCE_EFFECT, presences[index % 3].clone(), mask)),
    );
    Recipe {
        masks,
        strokes,
        ..recipe(layers)
    }
}

/// The warm list `stack`'s committed job carries, and every link it holds.
fn warmed(stack: &Recipe, context: &RenderContext) -> (Vec<GpuPlan>, Vec<Vec<String>>) {
    let (job, _) = draft_job_of(
        context.clone(),
        source(),
        "set-presence",
        stack.clone(),
        stack.clone(),
        0,
    );
    let warm =
        crate::render::gpu::plan_warm_list(&job.evaluation, crate::GpuView::Fit(bounds())).unwrap();
    // Every link of each plan's chain, then the light links the list warms.
    let mut links: Vec<Vec<String>> = Vec::new();
    for plan in &warm.plans {
        for link in warm_links(plan) {
            if !is_light(&link) && !links.contains(&link) {
                links.push(link);
            }
        }
    }
    for (_, light) in &warm.lights {
        let link = crate::render::gpu::light_link(light);
        if !links.contains(&link) {
            links.push(link);
        }
    }
    (warm.plans, links)
}

/// Whether `link` is a light link's, which [`warmed`] counts by the list's own lights.
fn is_light(link: &[String]) -> bool {
    link.first()
        .is_some_and(|first| first.starts_with("light link"))
}

/// The links a drag of `stack`'s layer `index` through `action` draws, `fields` moved; or, with no
/// layer, the first drag of `action`'s module, which the stack does not hold.
fn drag_links(
    stack: &Recipe,
    context: &RenderContext,
    action: &str,
    index: Option<usize>,
    fields: Value,
) -> Vec<Vec<String>> {
    let mut drafted = stack.clone();
    if let Some(index) = index {
        for (key, value) in fields.as_object().expect("fields") {
            drafted.layers[index].payload[key] = value.clone();
        }
    }
    let (job, mut draft) =
        draft_job_of(context.clone(), source(), action, stack.clone(), drafted, 1);
    if let Some(mask) = index.and_then(|index| stack.layers[index].mask.as_ref()) {
        draft.target.insert("mask".into(), mask.as_str().to_owned());
    }
    let preview = plan_preview(&job.evaluation, &draft, crate::GpuView::Fit(bounds())).unwrap();
    warm_links(planned(&preview))
}

/// How many links of `links` `warmed` does not hold.
fn unwarmed(links: &[Vec<String>], warmed: &[Vec<String>]) -> usize {
    links.iter().filter(|link| !warmed.contains(link)).count()
}

/// Sixteen masked Detail and Presence layers of mixed mask kinds and unit sets: the warm list holds
/// the stack's own plan and one drag for each distinct drafted shape, so the drag of every masked
/// spatial layer, Detail's and Presence's, draws only links the list warms and begins on the GPU
/// over a warm cache. Each such drag draws a link a layer, its own drafted and the stack's own
/// others, and the one light link the stand-ins of its Dehaze layers' lights share, Detail before
/// them left out. The list stays within its link bound. A Basic drag computes the light every tick
/// through its drafted layer, a light link of its own, so each Basic candidate takes two links: here
/// the drags of the first nine Basic layers are warmed and the last eight left out.
#[test]
fn the_warm_list_holds_a_drag_of_each_of_sixteen_masked_spatial_layers() {
    let stack = sixteen(false);
    let context = RenderContext::new();
    let (plans, links) = warmed(&stack, &context);
    let own = warm_links(&plans[0]);
    assert_eq!(
        own.len(),
        MAX_MASKED_SPATIAL_LAYERS + 1,
        "the stack's own plan and its light link"
    );
    assert!(links.len() <= GPU_WARM_LINKS, "{} links", links.len());
    let mut shapes: Vec<Vec<String>> = Vec::new();
    let mut colours = Vec::new();
    for (index, layer) in stack.layers.iter().enumerate() {
        let (action, fields) = match layer.effect_id.as_str() {
            DETAIL_EFFECT => ("set-detail", json!({"sharpening": 55})),
            PRESENCE_EFFECT => ("set-presence", json!({"clarity": 35})),
            BASIC_EFFECT => ("set-basic", json!({"exposure": 0.45})),
            _ => continue,
        };
        let drag = drag_links(&stack, &context, action, Some(index), fields);
        if action == "set-basic" {
            colours.push(unwarmed(&drag, &links));
            continue;
        }
        assert_eq!(
            unwarmed(&drag, &links),
            0,
            "layer {index} ({action}): every link it draws is warmed"
        );
        {
            assert_eq!(drag.len(), MAX_MASKED_SPATIAL_LAYERS + 1, "layer {index}");
            assert!(
                unwarmed(&drag, &own) <= 1,
                "layer {index}: the stack's own links but its drafted one"
            );
            for link in drag.into_iter().filter(|link| !own.contains(link)) {
                if !shapes.contains(&link) {
                    shapes.push(link);
                }
            }
        }
    }
    eprintln!(
        "the warm list of sixteen masked spatial layers: {} plans, {} links of {GPU_WARM_LINKS}; \
         the stack's own plan {} links, {} drafted shapes it does not hold; the Basic drags' \
         unwarmed links {colours:?}",
        plans.len(),
        links.len(),
        own.len(),
        shapes.len()
    );
    let warm = colours.iter().take_while(|missing| **missing == 0).count();
    assert!(
        warm == 9 && colours[warm..].iter().all(|missing| *missing == 2),
        "the first colour drags warmed, the last left out: {colours:?}"
    );
    // The stack's own links and each drafted shape it does not hold stay within twice the largest
    // plan, which leaves the colour candidates room under the bound.
    assert!(own.len() + shapes.len() < 2 * GPU_PLAN_LINKS);
}

/// When no two masks are of one shape, every masked spatial layer's drafted link is a sequence of
/// its own, and with them the colour candidates would add more links than the list's bound leaves:
/// the list fills to its bound, and the drag of every masked spatial layer still finds each link it
/// draws warmed, because those drags are chosen first. The colour candidates past the bound, the
/// last of them in stack order, are left out and begin compiling. A drag of a colour layer before
/// the Dehaze layers computes their light every tick through its drafted layer, a light link of its
/// own, so each such candidate takes two links: here the drags of the last ten masked Basic layers
/// are left out, each missing its drafted link and its light link, and the first drags of the Tone
/// curve and the colour mixer likewise, and the vignette's, after every Dehaze layer, its drafted
/// link alone.
#[test]
fn past_its_bound_the_warm_list_keeps_every_spatial_drag_and_leaves_out_colour_ones() {
    let stack = sixteen(true);
    let context = RenderContext::new();
    let (plans, links) = warmed(&stack, &context);
    assert_eq!(links.len(), GPU_WARM_LINKS, "the list is full");
    let mut colours = Vec::new();
    for (index, layer) in stack.layers.iter().enumerate() {
        let (action, fields) = match layer.effect_id.as_str() {
            DETAIL_EFFECT => ("set-detail", json!({"sharpening": 55})),
            PRESENCE_EFFECT => ("set-presence", json!({"clarity": 35})),
            BASIC_EFFECT => ("set-basic", json!({"exposure": 0.45})),
            _ => continue,
        };
        let missing = unwarmed(
            &drag_links(&stack, &context, action, Some(index), fields),
            &links,
        );
        if action == "set-basic" {
            colours.push(missing);
        } else {
            assert_eq!(missing, 0, "layer {index} ({action}) is warmed");
        }
    }
    let firsts: Vec<(&str, usize)> = ["set-curve", "set-mixer", "set-vignette"]
        .into_iter()
        .map(|action| {
            let drag = drag_links(&stack, &context, action, None, json!({}));
            (action, unwarmed(&drag, &links))
        })
        .collect();
    eprintln!(
        "sixteen masks of distinct shapes: {} plans, {} links; the Basic drags' unwarmed links \
         {colours:?}; first drags' {firsts:?}",
        plans.len(),
        links.len()
    );
    let warm = colours.iter().take_while(|missing| **missing == 0).count();
    assert!(
        warm > 0 && colours[warm..].iter().all(|missing| *missing == 2),
        "the first colour drags warmed, the last left out: {colours:?}"
    );
    assert_eq!(colours.len() - warm, 10, "{colours:?}");
    assert_eq!(
        firsts,
        [("set-curve", 2), ("set-mixer", 2), ("set-vignette", 1)],
        "the first drags are left out"
    );
}

/// The warm list's own order: the open stack's plans first — a drag of each layer it holds — then
/// the rest of the program set, the first drag of each module it does not hold, Detail's and
/// Presence's among them, so the surface compiles what a gesture over the photograph on screen
/// draws before anything else. Every drag of a layer the stack holds draws only links the open
/// part warms, and the first drag of a module it does not hold, a spatial one included, draws a
/// link only the rest warms.
#[test]
fn the_open_stacks_drags_warm_before_the_first_drags_of_the_rest() {
    let stack = recipe(vec![basic(json!({"exposure": 0.3}))]);
    let context = RenderContext::new();
    let (job, _) = draft_job_of(
        context.clone(),
        source(),
        "set-basic",
        stack.clone(),
        stack.clone(),
        0,
    );
    let warm =
        crate::render::gpu::plan_warm_list(&job.evaluation, crate::GpuView::Fit(bounds())).unwrap();
    assert!(
        warm.open > 0 && warm.open < warm.plans.len(),
        "{}",
        warm.open
    );
    let links_of = |plans: &[GpuPlan]| {
        let mut links: Vec<Vec<String>> = Vec::new();
        for link in plans.iter().flat_map(warm_links) {
            if !links.contains(&link) {
                links.push(link);
            }
        }
        links
    };
    let (open, all) = (links_of(&warm.plans[..warm.open]), links_of(&warm.plans));
    assert!(all.len() <= GPU_WARM_LINKS);
    let held = drag_links(
        &stack,
        &context,
        "set-basic",
        Some(0),
        json!({"exposure": 0.6}),
    );
    assert_eq!(
        unwarmed(&held, &open),
        0,
        "the Basic layer's drag is the open stack's"
    );
    let firsts = [
        "set-curve",
        "set-mixer",
        "set-vignette",
        "set-detail",
        "set-presence",
    ];
    for action in firsts {
        let first = drag_links(&stack, &context, action, None, json!({}));
        assert_eq!(unwarmed(&first, &all), 0, "{action}'s first drag is warmed");
        assert!(
            unwarmed(&first, &open) > 0,
            "{action}'s first drag is the rest's, after the open stack's"
        );
    }
    eprintln!(
        "a Basic stack's warm list: {} plans, the first {} the open stack's, {} links; the rest \
         warms the first drags of {firsts:?}",
        warm.plans.len(),
        warm.open,
        all.len()
    );
}
