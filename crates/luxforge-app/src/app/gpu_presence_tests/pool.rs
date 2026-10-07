//! Masked Presence layers chained in one plan, each a link that takes its scratch planes from the
//! slot's one pool (`docs/design/gpu-preview.md`, "Plane sharing and precision"): drags of
//! different links in turn, each tick's passes counted link by link and its frame held bit for bit
//! to the whole evaluation, with the poison on too; and the one pass count sharing moves, a Dehaze
//! drag after another link ran.
use super::{photograph, stage};
use crate::app::gpu_plan::surface_plan;
use crate::app::gpu_qualification::{headless, held_to_whole, lit, lit_fixed};
use luxforge_core::{
    BASIC_EFFECT, Component, ComponentMode, GpuAnswer, GpuPlanRequest, Layer, LinearImage,
    LinearSettings, Mask, ModuleRegistry, PRESENCE_EFFECT, PreviewSource, Recipe, gpu_plan,
};
use luxforge_gpu::{GpuPlan, qualification::boundary};
use serde_json::{Value, json};

/// A mask named `name` of one feathered radial centred at `(x, y)` of the stage.
fn radial_mask(name: &str, x: f64, y: f64) -> Mask {
    let mut mask = Mask::new(name);
    mask.components.push(Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        json!({"x": x, "y": y, "radius_x": 0.14, "radius_y": 0.12, "angle": 15.0,
               "feather": 50.0}),
    ));
    mask
}

/// A recipe of `payloads`' layers, each of `effects`' effect and masked by the mask of the same
/// index when there is one, holding `masks`.
fn layered(effects: &[&str], payloads: &[Value], masks: &[Mask]) -> Recipe {
    let mut recipe = Recipe::default();
    for (index, (effect, payload)) in effects.iter().zip(payloads).enumerate() {
        recipe.layers.push(Layer {
            mask: masks.get(index).map(|mask| mask.id.clone()),
            ..Layer::new(*effect, payload.clone())
        });
    }
    recipe.masks = masks.to_vec();
    recipe
}

/// A masked Basic layer, three masked Presence layers holding every field, each through a radial
/// of its own, and a global Presence layer after them, at a Fit stage's thin-feature scale: five
/// links, the Basic layer's colour link and four spatial ones taking their scratch planes from one
/// pool. Drags of every layer in turn, 22 ticks after the first, each one value a tick, change one
/// link's words: the links before it run nothing, it runs the passes its words change, and every
/// link after it, whose input moved, all of its own. A Texture drag runs 5 of its link's 20
/// passes and a Clarity drag none, as one link alone does. A Dehaze drag runs all 20 when a later
/// masked layer, which holds Dehaze's planes too, wrote that link's scratch since, and 19, as
/// alone, for the third masked layer, after which only the global layer runs: it holds no Dehaze
/// plane, so the textures of Dehaze's own classes are still the third layer's. Every tick's frame
/// is the whole evaluation's, bit for bit, both reading one light, and so it is with the poison
/// on; each tick's passes are pinned link by link.
#[test]
fn gpu_presence_chained_masked_layers_dragged_in_turn_draw_what_a_whole_evaluation_does() {
    let test =
        "gpu_presence_chained_masked_layers_dragged_in_turn_draw_what_a_whole_evaluation_does";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (480, 320);
    let pixels = photograph(width, height, 5);
    let held = boundary(width, height, 1, &pixels).expect("a boundary");
    let masks = [
        radial_mask("Basic", 0.25, 0.3),
        radial_mask("Presence 1", 0.3, 0.62),
        radial_mask("Presence 2", 0.55, 0.4),
        radial_mask("Presence 3", 0.76, 0.64),
    ];
    let effects = [
        BASIC_EFFECT,
        PRESENCE_EFFECT,
        PRESENCE_EFFECT,
        PRESENCE_EFFECT,
        PRESENCE_EFFECT,
    ];
    let mut payloads = [
        json!({"exposure": 0.4}),
        json!({"texture": 40, "clarity": 30, "dehaze": 20}),
        json!({"texture": -30, "clarity": 50, "dehaze": 35}),
        json!({"texture": 60, "clarity": -20, "dehaze": -25}),
        json!({"texture": 25, "clarity": 20}),
    ];
    let request =
        GpuPlanRequest::fit(0, stage(width, height), stage(width * 4, height * 4)).qualifying();
    let planned = |payloads: &[Value]| {
        let stack = layered(&effects, payloads, &masks);
        match gpu_plan(&registry, &stack, request).expect("the stack compiles") {
            GpuAnswer::Plan(plan) => surface_plan(&plan, held.clone()).expect("a runnable plan"),
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        }
    };
    // Each tick after the first: the layer its drag moves, the field and the value, none of them
    // crossing zero, which would change the plan's shape. Then each link's passes: the colour
    // link's none, then each Presence link's.
    let drags: [(usize, &str, f64, [u64; 5]); 22] = [
        (1, "texture", 45.0, [0, 5, 20, 20, 13]),
        (1, "texture", 50.0, [0, 5, 20, 20, 13]),
        (2, "clarity", 40.0, [0, 0, 0, 20, 13]),
        (2, "clarity", 30.0, [0, 0, 0, 20, 13]),
        (3, "dehaze", -15.0, [0, 0, 0, 19, 13]),
        (3, "dehaze", -5.0, [0, 0, 0, 19, 13]),
        (4, "texture", 35.0, [0, 0, 0, 0, 5]),
        (4, "texture", 45.0, [0, 0, 0, 0, 5]),
        (0, "exposure", 0.6, [0, 20, 20, 20, 13]),
        (0, "exposure", 0.8, [0, 20, 20, 20, 13]),
        (1, "dehaze", 30.0, [0, 20, 20, 20, 13]),
        (1, "dehaze", 40.0, [0, 20, 20, 20, 13]),
        (3, "texture", 40.0, [0, 0, 0, 5, 13]),
        (2, "dehaze", 45.0, [0, 0, 20, 20, 13]),
        (4, "clarity", 10.0, [0, 0, 0, 0, 0]),
        (4, "clarity", 5.0, [0, 0, 0, 0, 0]),
        (1, "clarity", 10.0, [0, 0, 20, 20, 13]),
        (2, "texture", -10.0, [0, 0, 5, 20, 13]),
        (3, "clarity", -35.0, [0, 0, 0, 0, 13]),
        (0, "exposure", 0.2, [0, 20, 20, 20, 13]),
        (1, "texture", 20.0, [0, 5, 20, 20, 13]),
        (4, "texture", 30.0, [0, 0, 0, 0, 5]),
    ];
    let mut plans = vec![planned(&payloads)];
    for (layer, field, value, _) in &drags {
        payloads[*layer][*field] = json!(value);
        plans.push(planned(&payloads));
    }
    let ticks: Vec<(GpuPlan, Option<[u32; 4]>)> =
        plans.into_iter().map(|plan| (plan, None)).collect();
    // The tick held to the whole evaluation, both reading the one light each Dehaze layer reads.
    lit_fixed(&qualifier, &ticks[0].0);
    let whole: Vec<_> = ticks
        .iter()
        .map(|(plan, _)| qualifier.evaluate(plan).expect("a readback"))
        .collect();
    let name = |number: usize| match number {
        0 => "the first tick".to_owned(),
        _ => {
            let (layer, field, value, _) = drags[number - 1];
            format!("tick {number}, layer {layer}'s {field} at {value}")
        }
    };
    let passes = held_to_whole(&qualifier, &ticks, &whole, &name);
    for (number, ran) in passes.iter().enumerate() {
        eprintln!("{test}: {}: {ran:?} passes", name(number));
    }
    assert_eq!(
        passes[0],
        [0, 20, 20, 20, 13],
        "the first tick runs every pass"
    );
    for (number, (_, _, _, wanted)) in drags.iter().enumerate() {
        assert_eq!(passes[number + 1], wanted, "{}", name(number + 1));
    }
}

/// The one pass count sharing the scratch moves: a Dehaze drag of the first of two masked Presence
/// layers of every field, each through a radial of its own, on a tick after the second has run.
/// The second link wrote every pool texture the first takes its scratch from, so the first runs
/// all 20 of its passes, where alone, its scratch its own, it runs 19; the second runs all 20, its
/// input moved. Each light its light link's, computed from the whole stage; every frame the whole
/// evaluation's, bit for bit, and so with the poison on.
#[test]
fn gpu_presence_a_dehaze_drag_after_another_link_ran_reruns_what_that_link_wrote() {
    let test = "gpu_presence_a_dehaze_drag_after_another_link_ran_reruns_what_that_link_wrote";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (480, 320);
    let pixels = photograph(width, height, 3);
    let len = pixels.len();
    let mut planes = vec![0.0_f32; 3 * len];
    for (index, pixel) in pixels.iter().enumerate() {
        for channel in 0..3 {
            planes[channel * len + index] = pixel[channel];
        }
    }
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:gpu-presence-pool")
        .expect("a linear image");
    let source = PreviewSource::Raw {
        image,
        settings: LinearSettings::default(),
    };
    let masks = [
        radial_mask("Presence 1", 0.35, 0.5),
        radial_mask("Presence 2", 0.65, 0.45),
    ];
    let start = json!({"texture": 40, "clarity": 30, "dehaze": 25});
    let dragged = json!({"texture": 40, "clarity": 30, "dehaze": 60});
    let stack = |payloads: &[Value]| {
        let effects = vec![PRESENCE_EFFECT; payloads.len()];
        layered(&effects, payloads, &masks[..payloads.len()])
    };
    let planned = |payloads: &[Value]| {
        let plan = match gpu_plan(
            &registry,
            &stack(payloads),
            GpuPlanRequest::exact(0, stage(width, height))
                .qualifying()
                .linear(),
        )
        .expect("the stack compiles")
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        let held = boundary(width, height, 1, &pixels).expect("a boundary");
        surface_plan(&plan, held).expect("a runnable plan")
    };
    // Each case: the layers before and after the drag, and each link's passes on its tick.
    for (layers, before, after, wanted) in [
        (
            "one layer",
            vec![start.clone()],
            vec![dragged.clone()],
            vec![19],
        ),
        (
            "two layers",
            vec![start.clone(), start.clone()],
            vec![dragged.clone(), start.clone()],
            vec![20, 20],
        ),
    ] {
        let ticks = vec![(planned(&before), None), (planned(&after), None)];
        // The lights depend on the stage alone, which the drag does not move.
        lit(&qualifier, &source, &ticks[0].0).expect("the lights");
        let whole: Vec<_> = ticks
            .iter()
            .map(|(plan, _)| qualifier.evaluate(plan).expect("a readback"))
            .collect();
        let passes = held_to_whole(&qualifier, &ticks, &whole, &|number| {
            format!("{layers}, tick {number}")
        });
        eprintln!(
            "{test}: {layers}: {:?} passes, then {:?}",
            passes[0], passes[1]
        );
        assert_eq!(
            passes[0],
            vec![20; wanted.len()],
            "{layers}: every pass first"
        );
        assert_eq!(passes[1], wanted, "{layers}");
    }
}
