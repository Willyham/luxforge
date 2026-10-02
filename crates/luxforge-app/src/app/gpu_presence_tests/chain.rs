//! Detail followed by Presence on the GPU: two spatial operations chained in one plan, a colour
//! layer between them, each operation's passes reading its input through everything before it,
//! against the CPU frame of the same stack, unmasked and with Presence through a mask.
use super::{measure_unit, photograph, radial};
use crate::app::gpu_qualification::{figures, worst};
use luxforge_core::{
    GpuAnswer, GpuPlanRequest, Layer, Mask, ModuleRegistry, RECIPE_FORMAT, Recipe, Stage, gpu_plan,
};
use serde_json::{Value, json};

/// Detail at `detail`, an exposure lift, then Presence at `presence`, through the radial mask when
/// `masking`.
fn chained(detail: Value, presence: Value, masking: bool) -> Recipe {
    let mut recipe = Recipe {
        format: RECIPE_FORMAT,
        layers: vec![
            Layer::new(luxforge_core::DETAIL_EFFECT, detail),
            Layer::new(luxforge_core::BASIC_EFFECT, json!({"exposure": 0.3})),
            Layer::new(luxforge_core::PRESENCE_EFFECT, presence),
        ],
        ..Recipe::default()
    };
    if masking {
        let mut mask = Mask::new("Mask 1");
        mask.components.push(radial());
        recipe.layers[2].mask = Some(mask.id.clone());
        recipe.masks.push(mask);
    }
    recipe
}

/// Detail's sharpening and noise reduction, then Presence's fields, chained in one plan over a
/// synthetic photograph on the linear path at two sizes: the program's output against the CPU
/// frame held to the spatial limits, Dehaze's light stored by the CPU frame or taken on the GPU.
#[test]
fn gpu_presence_after_detail_meets_the_spatial_limits() {
    let test = "gpu_presence_after_detail_meets_the_spatial_limits";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let registry = ModuleRegistry::builtin();
    let details = [
        json!({"sharpening": 60}),
        json!({"sharpening": 40, "luminance": 30, "colour": 30}),
    ];
    let presences = [
        json!({"clarity": 60}),
        json!({"texture": 50, "clarity": 40, "dehaze": 30}),
        json!({"texture": -60, "clarity": -40, "dehaze": -30}),
    ];
    let (mut programs, mut drawn, mut missed) = (Vec::new(), Vec::new(), Vec::new());
    for (width, height) in [(480, 320), (1536, 1024)] {
        let pixels = photograph(width, height, u64::from(width) + 7);
        for detail in &details {
            for presence in &presences {
                for masking in [false, true] {
                    let stack = chained(detail.clone(), presence.clone(), masking);
                    let request = GpuPlanRequest::exact(0, Stage { width, height })
                        .qualifying()
                        .linear();
                    let Ok(GpuAnswer::Plan(plan)) = gpu_plan(&registry, &stack, request) else {
                        panic!("{detail} then {presence}: a plan");
                    };
                    let layers: Vec<usize> = plan.spatial.iter().map(|step| step.layer).collect();
                    assert_eq!(layers, [0, 2], "Detail's operation, then Presence's");
                    let dehaze = presence.get("dehaze").is_some();
                    for stored in if dehaze {
                        vec![true, false]
                    } else {
                        vec![true]
                    } {
                        let measured = measure_unit(
                            &qualifier,
                            &registry,
                            &stack,
                            (width, height),
                            &pixels,
                            stored,
                        );
                        let name = format!(
                            "{detail} then {presence}{} at {width}x{height}{}",
                            if masking { " masked" } else { "" },
                            if dehaze && !stored {
                                ", light taken on the GPU"
                            } else {
                                ""
                            }
                        );
                        eprintln!(
                            "{name}: program {} | drawn {} | non-finite {} | planes {} B{}",
                            figures(&measured.program),
                            figures(&measured.drawn),
                            measured.non_finite,
                            measured.planes,
                            if measured.passed { "" } else { " MISS" }
                        );
                        programs.push(measured.program);
                        drawn.push(measured.drawn);
                        if !measured.passed {
                            missed.push(name);
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "{test}: worst of {} cases: program {} | drawn {}",
        programs.len(),
        figures(&worst(&programs)),
        figures(&worst(&drawn))
    );
    assert!(
        missed.is_empty(),
        "cases missing the spatial limits: {missed:?}"
    );
}

/// A tick of a chained plan runs only the passes its words change, layer by layer, and draws
/// exactly what a run of every pass draws: a Clarity drag none, a Texture drag Clarity's that read
/// Texture's output, and a drag of Detail or of the colour layer between the two Presence's passes
/// that read their input, Detail's own passes only where its words move. Chained steps share their
/// scratch textures, so the slot charges less than the two steps' planes apart.
#[test]
fn gpu_presence_after_detail_reruns_only_the_passes_a_tick_changes() {
    let test = "gpu_presence_after_detail_reruns_only_the_passes_a_tick_changes";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (480, 320);
    let pixels = photograph(width, height, 11);
    let plan_of = |stack: &Recipe| {
        let request = GpuPlanRequest::exact(0, Stage { width, height })
            .qualifying()
            .linear();
        let Ok(GpuAnswer::Plan(plan)) = gpu_plan(&registry, stack, request) else {
            panic!("a plan");
        };
        let held = luxforge_ui::photo_surface::gpu_preview::qualification::boundary(
            width, height, 1, &pixels,
        )
        .expect("a boundary");
        let passes: Vec<usize> = plan.spatial.iter().map(|step| step.passes.len()).collect();
        let planes: u64 = plan
            .spatial
            .iter()
            .map(|step| step.plane_bytes((0, 0), (width, height)))
            .sum();
        (
            crate::app::gpu_plan::surface_plan(&plan, held).expect("a runnable plan"),
            passes,
            planes,
        )
    };
    let start = || {
        chained(
            json!({"sharpening": 40, "luminance": 20}),
            json!({"texture": 40, "clarity": 30}),
            false,
        )
    };
    let (first, passes, planes) = plan_of(&start());
    let all: usize = passes.iter().sum();
    let charged = qualifier.charged_bytes(&first).expect("a charge");
    eprintln!("{test}: {passes:?} passes, planes {planes} B apart, slot {charged} B");
    let moved = |layer: usize, payload: Value| {
        let mut stack = start();
        stack.layers[layer].payload = payload;
        stack
    };
    let drags = [
        (
            "Clarity",
            moved(2, json!({"texture": 40, "clarity": -10})),
            Some(0),
        ),
        (
            "Texture",
            moved(2, json!({"texture": 75, "clarity": 30})),
            None,
        ),
        (
            "Detail",
            moved(0, json!({"sharpening": 70, "luminance": 20})),
            None,
        ),
        (
            "the colour between",
            moved(1, json!({"exposure": 0.6})),
            None,
        ),
    ];
    for (drag, stack, wanted) in drags {
        let (then, _, _) = plan_of(&stack);
        let (after, ran) = qualifier
            .evaluate_after(&first, &then)
            .expect("a readback after the first");
        let whole = qualifier.evaluate(&then).expect("a readback of every pass");
        eprintln!("{test}: {drag}: {ran} of {all} passes");
        if let Some(wanted) = wanted {
            assert_eq!(ran, wanted, "{drag}");
        }
        assert!(ran < all as u64, "{drag} skips something");
        let differing = after
            .iter()
            .zip(&whole)
            .filter(|(after, whole)| after.map(f32::to_bits) != whole.map(f32::to_bits))
            .count();
        assert_eq!(
            differing, 0,
            "{drag}: texels that differ from every pass run"
        );
    }
}
