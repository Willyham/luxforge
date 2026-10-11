//! The GPU's picture at rest of a derived photograph (an HDR merge's result, `docs/design/hdr-merge.md`):
//! planned on the linear path from the stored planes, signed and with headroom, as a RAW's
//! developed planes are, and drawn by the reference executor within the plan's tolerance of the
//! reference renderer's frame for each pointwise, masked and geometry stack; its warm list holds
//! no RAW-only drag, neither the look nor a sensor white balance.
use super::{GpuAnswer, GpuPlan, GpuView, interpret, plan_rest, plan_tests::tolerance};
use crate::{
    AssetId, Cancel, ComponentMode, EntryId, Evaluation, HistoryEntry, Layer, LinearImage,
    LinearSettings, Mask, ModuleRegistry, PreviewSource, ProxyBounds, RECIPE_FORMAT, Recipe,
    RenderContext, RenderOptions, Snapshot, SnapshotId, SourceTag, render::tests::fitted_crop,
};
use serde_json::json;
use std::sync::Arc;

const WIDTH: u32 = 120;
const HEIGHT: u32 = 80;

/// Planes of a merge's result: ramps from below zero to four, a column out of gamut with a
/// negative red, and fine detail.
fn planes() -> LinearImage {
    let plane = (WIDTH * HEIGHT) as usize;
    let mut values = vec![0.0_f32; 3 * plane];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let u = x as f32 / (WIDTH - 1) as f32;
            let v = y as f32 / (HEIGHT - 1) as f32;
            let detail = 0.05 * ((x + y) % 3) as f32;
            let mut rgb = [
                -0.2 + 4.2 * u * u + detail,
                (-0.1 + 3.0 * v * u) + detail,
                -0.15 + 1.9 * v * (1.0 - u),
            ];
            if x % 17 < 2 {
                rgb = [-0.3, 0.8 + v, 0.4];
            }
            let at = (y * WIDTH + x) as usize;
            for (channel, value) in rgb.into_iter().enumerate() {
                values[channel * plane + at] = value;
            }
        }
    }
    LinearImage::with_fingerprint(WIDTH, HEIGHT, values, "source-derived").unwrap()
}

/// The one mask every stack carries.
fn mask() -> Mask {
    static MASK: std::sync::OnceLock<Mask> = std::sync::OnceLock::new();
    MASK.get_or_init(new_mask).clone()
}

fn new_mask() -> Mask {
    let mut mask = Mask::new("Mask 1");
    mask.components.push(crate::Component::new(
        "Linear 1",
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
    ));
    mask
}

/// `layers` placed where the host places each new layer of its effect and target.
fn stack(registry: &ModuleRegistry, layers: Vec<Layer>) -> Recipe {
    let mut recipe = Recipe {
        format: RECIPE_FORMAT,
        masks: vec![mask()],
        ..Recipe::default()
    };
    for layer in layers {
        let at = registry.insertion_index_for_target(
            &recipe.layers,
            &layer.effect_id,
            layer.mask.as_ref(),
            &recipe.masks,
        );
        recipe = recipe.with_layer_inserted(at, layer).unwrap();
    }
    recipe
}

/// The committed `recipe` of a derived photograph over `image`, as its job evaluates it.
fn evaluation(image: &LinearImage, recipe: Recipe) -> Evaluation {
    let asset = AssetId::new();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence: 1,
        action_id: "set-basic".into(),
        label: "Basic".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: asset,
            recipe: recipe.clone(),
        },
        undo_parent: None,
        restore_target: None,
    };
    Evaluation::new(
        Arc::new(ModuleRegistry::builtin()),
        RenderContext::new(),
        PreviewSource::Raw {
            image: image.clone(),
            settings: LinearSettings::default(),
        },
        entry,
        recipe,
        None,
    )
    .of_kind(SourceTag::Derived)
}

fn planned(answer: &GpuAnswer, what: &str) -> GpuPlan {
    match answer {
        GpuAnswer::Plan(plan) => (**plan).clone(),
        GpuAnswer::Fallback(reason) => panic!("{what}: expected a plan, got {reason}"),
    }
}

/// Each stack a person builds on a derived photograph from the pointwise panels, a mask and the
/// geometry, at Fit at its own size: the picture at rest is the linear path's plan from the stored
/// planes, and the reference executor's frame of it is within the plan's tolerance of the reference
/// renderer's frame of the same planes. The source holds values below zero and up to four. On the
/// linear path a resample reads float planes the reference reads in `f64` and the plan in `f32`, so
/// a stack that resamples is allowed a code more, as a RAW's developed planes are through a
/// straightened crop (`source_drag_tests`).
#[test]
fn a_derived_source_is_drawn_from_its_planes_within_tolerance() {
    let registry = ModuleRegistry::builtin();
    let image = planes();
    let basic = || {
        Layer::new(
            crate::BASIC_EFFECT,
            json!({"exposure": -1.0, "contrast": 20.0, "highlights": -30.0, "shadows": 25.0,
                   "temperature": 20.0, "tint": -10.0, "vibrance": 25.0}),
        )
    };
    let curve = || {
        Layer::new(
            crate::CURVE_EFFECT,
            json!({"luminance": [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]}),
        )
    };
    let mixer = || {
        Layer::new(
            crate::MIXER_EFFECT,
            json!({"red-hue": 25, "blue-saturation": -30, "grade-highlights-hue": 40,
                   "grade-highlights-saturation": 30}),
        )
    };
    let masked = || Layer {
        mask: Some(mask().id),
        ..Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.7}))
    };
    let vignette = || Layer::new(crate::VIGNETTE_EFFECT, json!({"amount": -45.0}));
    let crop = || Layer::crop(fitted_crop(WIDTH, HEIGHT, 5.0, [0.1, 0.1, 0.8, 0.75]));
    let perspective = || {
        Layer::new(
            crate::PERSPECTIVE_EFFECT,
            json!({"horizontal": 25, "vertical": -15}),
        )
    };
    let stacks: Vec<(&str, Vec<Layer>, bool)> = vec![
        ("the Original", vec![], false),
        ("Basic", vec![basic()], false),
        (
            "the colour stack",
            vec![basic(), curve(), mixer(), vignette()],
            false,
        ),
        ("a masked Basic", vec![basic(), masked()], false),
        ("a straightened crop", vec![crop(), basic()], true),
        (
            "perspective",
            vec![perspective(), basic(), vignette()],
            true,
        ),
    ];
    let view = GpuView::Fit(ProxyBounds {
        width: 4096,
        height: 4096,
    });
    let texels: Vec<[f32; 3]> = (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .map(|(x, y)| image.pixel(x, y).expect("inside the planes"))
        .collect();
    assert!(texels.iter().flatten().any(|value| *value < 0.0));
    assert!(texels.iter().flatten().any(|value| *value > 3.5));
    for (what, layers, resamples) in stacks {
        let recipe = stack(&registry, layers);
        let evaluation = evaluation(&image, recipe.clone());
        let rest = plan_rest(&evaluation, view).unwrap();
        let plan = planned(&rest.view.answer, what);
        assert!(plan.linear, "{what}: the linear path");
        assert_eq!(plan.boundary.layer, 0, "{what}: from the stored planes");
        let compiled = registry.compile(WIDTH, HEIGHT, &recipe).unwrap();
        let gpu = interpret::execute(&plan, &texels, &compiled.colour_operations()).unwrap();
        let cpu = crate::render(
            &registry,
            crate::RenderSource::Linear {
                image: &image,
                settings: LinearSettings::default(),
            },
            &recipe,
            RenderOptions::exact(&Cancel::never()),
            &RenderContext::new(),
        )
        .unwrap()
        .frame(SnapshotId::new())
        .unwrap();
        assert_eq!(cpu.rgba.len(), gpu.len(), "{what}: one output stage");
        let largest = cpu
            .rgba
            .iter()
            .zip(&gpu)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let allowed = tolerance(&plan, false).max(u8::from(resamples));
        assert!(
            largest <= allowed,
            "{what}: the plan draws {largest} codes from the reference, {allowed} allowed"
        );
    }
}

/// A derived photograph's warm list holds no drag of a module that applies to RAW photos only —
/// the look — and no sensor white-balance drag, since it has no development layer: its kind, not
/// its linear planes, says what it offers.
#[test]
fn a_derived_source_warm_list_holds_no_raw_only_drag() {
    let registry = ModuleRegistry::builtin();
    let image = planes();
    let recipe = stack(
        &registry,
        vec![Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.2}))],
    );
    let warm = super::plan_warm_list(
        &evaluation(&image, recipe),
        GpuView::Fit(ProxyBounds {
            width: 60,
            height: 40,
        }),
    )
    .unwrap();
    assert!(!warm.plans.is_empty(), "the open stack is warmed");
    for plan in &warm.plans {
        for operation in &plan.content {
            assert!(
                operation
                    .units
                    .iter()
                    .all(|unit| unit.program.entry != "lf_look_look"),
                "no look"
            );
        }
        // A RAW's white-balance drag is a leading step of the white-balance program alone, over
        // the source; a derived photograph's white balance is Basic's relative one.
        assert!(
            !plan
                .content
                .first()
                .is_some_and(|first| first.units.len() == 1
                    && std::ptr::eq(
                        first.units[0].program,
                        &crate::modules::basic::WHITE_BALANCE_PROGRAM
                    )),
            "no sensor white-balance drag"
        );
    }
}
