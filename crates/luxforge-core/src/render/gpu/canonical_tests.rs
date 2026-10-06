//! The GPU plan of every stack a person builds from the panels, each layer where the host places
//! it: every combination of the geometry modules — a turn, a lens profile, perspective and a crop,
//! plain or straightened — with Basic and every combination of the Tone curve, a masked colour
//! layer, Detail, Presence, their masks and the vignette. Each is planned as the editor plans a
//! frame, from the source, and draws the CPU frame.
use super::plan_tests::{cpu_frame_difference, smooth};
use super::{GpuAnswer, GpuPlanRequest, gpu_plan};
use crate::{
    ComponentMode, EffectStage, Layer, Mask, ModuleRegistry, Recipe, Transform,
    modules::Stage,
    render::tests::{fitted_crop, turn},
};
use serde_json::json;
use std::collections::BTreeMap;

const WIDTH: u32 = 180;
const HEIGHT: u32 = 120;

/// One panel's layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    Turn,
    Lens,
    Perspective,
    Crop,
    Straighten,
    Basic,
    Curve,
    MaskedBasic,
    Detail,
    MaskedDetail,
    Presence,
    MaskedPresence,
    Vignette,
}

impl Panel {
    /// The layer, a geometry layer's built against the `stage` it receives.
    fn layer(self, stage: Stage, mask: &Mask) -> Layer {
        let masked = |layer: Layer| Layer {
            mask: Some(mask.id.clone()),
            ..layer
        };
        let crop = |angle: f64| {
            Layer::crop(fitted_crop(
                stage.width,
                stage.height,
                angle,
                [0.1, 0.12, 0.8, 0.75],
            ))
        };
        match self {
            Self::Turn => turn(Transform::RotateRight),
            Self::Lens => crate::render::testing::frozen_lens(stage.width, stage.height, 24.0),
            Self::Perspective => Layer::new(
                crate::PERSPECTIVE_EFFECT,
                json!({"horizontal": 40, "vertical": -30}),
            ),
            Self::Crop => crop(0.0),
            Self::Straighten => crop(3.0),
            Self::Basic => Layer::new(
                crate::BASIC_EFFECT,
                json!({"exposure": 0.4, "contrast": 20.0}),
            ),
            Self::Curve => Layer::new(
                crate::CURVE_EFFECT,
                json!({"luminance": [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]}),
            ),
            Self::MaskedBasic => masked(Layer::new(crate::BASIC_EFFECT, json!({"exposure": -0.3}))),
            Self::Detail => Layer::new(
                crate::DETAIL_EFFECT,
                json!({"sharpening": 40.0, "luminance": 20.0}),
            ),
            Self::MaskedDetail => masked(Layer::new(
                crate::DETAIL_EFFECT,
                json!({"sharpening": 25.0}),
            )),
            Self::Presence => Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 30.0})),
            Self::MaskedPresence => {
                masked(Layer::new(crate::PRESENCE_EFFECT, json!({"texture": 20.0})))
            }
            Self::Vignette => Layer::new(crate::VIGNETTE_EFFECT, json!({"amount": -40.0})),
        }
    }
}

fn mask() -> Mask {
    let mut mask = Mask::new("Mask 1");
    mask.components.push(crate::Component::new(
        "Linear 1",
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
    ));
    mask
}

/// `panels` added in turn, each where the host places a new layer of its effect and target, a
/// geometry layer built against the stage the stack before it hands it.
fn stack(registry: &ModuleRegistry, panels: &[Panel], mask: &Mask) -> Recipe {
    let full = Stage {
        width: WIDTH,
        height: HEIGHT,
    };
    let mut recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        masks: vec![mask.clone()],
        ..Recipe::default()
    };
    for panel in panels {
        let probe = panel.layer(full, mask);
        let at = registry.insertion_index_for_target(
            &recipe.layers,
            &probe.effect_id,
            probe.mask.as_ref(),
            &recipe.masks,
        );
        let before = Recipe {
            layers: recipe.layers[..at].to_vec(),
            ..recipe.clone()
        };
        let stage = registry.compile(WIDTH, HEIGHT, &before).unwrap().stage();
        recipe.layers.insert(at, panel.layer(stage, mask));
    }
    recipe
}

/// Every subset of `items`, in order.
fn subsets(items: &[Panel]) -> impl Iterator<Item = Vec<Panel>> + '_ {
    (0..1u32 << items.len()).map(move |bits| {
        items
            .iter()
            .enumerate()
            .filter(|(at, _)| bits & (1 << at) != 0)
            .map(|(_, panel)| *panel)
            .collect()
    })
}

/// Every one of the 3,072 stacks answers a plan from the source on the byte path, on the RAW
/// linear path and with each of its colour, restoration, spatial or finish layers in the drafted
/// GPU shape: no geometry the panels make leaves a colour layer without a pass. Among them, Detail
/// with colour after it and a lens, perspective or straightened crop after that, whose colour the
/// CPU runs in Detail's own segment before the tail's resample. Every stack with at most two of the
/// optional content layers, 696 of them, draws the CPU frame within the plan's tolerance.
#[test]
fn every_stack_the_panels_build_plans_from_the_source_and_draws_the_cpu_frame() {
    let registry = ModuleRegistry::builtin();
    let source = smooth(WIDTH, HEIGHT);
    let mask = mask();
    let full = Stage {
        width: WIDTH,
        height: HEIGHT,
    };
    let geometry = [Panel::Turn, Panel::Lens, Panel::Perspective];
    let content = [
        Panel::Curve,
        Panel::MaskedBasic,
        Panel::Detail,
        Panel::MaskedDetail,
        Panel::Presence,
        Panel::MaskedPresence,
        Panel::Vignette,
    ];
    let mut missed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let (mut stacks, mut drawn) = (0, 0);
    for crop in [None, Some(Panel::Crop), Some(Panel::Straighten)] {
        for mut panels in subsets(&geometry) {
            panels.extend(crop);
            panels.push(Panel::Basic);
            for optional in subsets(&content) {
                let panels: Vec<Panel> = panels.iter().copied().chain(optional.clone()).collect();
                let recipe = stack(&registry, &panels, &mask);
                stacks += 1;
                let request = GpuPlanRequest::exact(0, full).from_source();
                let drafted = (0..recipe.layers.len())
                    .filter(|at| {
                        !matches!(
                            registry.effect_stage(&recipe.layers[*at].effect_id),
                            Some(EffectStage::Geometry | EffectStage::Source)
                        )
                    })
                    .map(|at| (format!("drafting layer {at}"), request.drafted(at)));
                let requests = [
                    ("source".to_owned(), request),
                    ("linear".to_owned(), request.linear()),
                ]
                .into_iter()
                .chain(drafted);
                for (from, request) in requests {
                    if let GpuAnswer::Fallback(reason) =
                        gpu_plan(&registry, &recipe, request).unwrap()
                    {
                        missed
                            .entry(reason.code().to_owned())
                            .or_default()
                            .push(format!("{panels:?} {from}"));
                    }
                }
                if optional.len() > 2 {
                    continue;
                }
                let GpuAnswer::Plan(plan) = gpu_plan(&registry, &recipe, request).unwrap() else {
                    continue;
                };
                drawn += 1;
                let what = format!("{panels:?}");
                let (difference, tolerance) =
                    cpu_frame_difference(&registry, &source, &recipe, &plan, &what);
                if difference > tolerance {
                    missed
                        .entry(format!("{difference} codes from the CPU frame"))
                        .or_default()
                        .push(what);
                }
            }
        }
    }
    assert_eq!((stacks, drawn), (3072, 696));
    let summary: Vec<String> = missed
        .iter()
        .map(|(what, cases)| {
            format!(
                "{what}: {} cases, as {:?}",
                cases.len(),
                &cases[..3.min(cases.len())]
            )
        })
        .collect();
    assert!(summary.is_empty(), "{summary:#?}");
}
