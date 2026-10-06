//! Shared restoration placement and compile-context contracts.
use super::*;
use crate::modules::{
    ActionInput, ActionPlan, LayerReport, ModuleDescriptor, PresenceModule, StageContext,
};
use crate::{
    CompileStage, EffectDescriptor, EffectStage, Layer, Mask, Processing, Recipe, Stage, ToolModule,
};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

const EFFECT: &str = "luxforge.restorationprobe.adjust";
struct Probe {
    descriptor: ModuleDescriptor,
    seen: Mutex<Vec<CompileStage>>,
    presence: PresenceModule,
}
impl Probe {
    fn new() -> Self {
        Self {
            descriptor: ModuleDescriptor {
                id: "luxforge.restorationprobe".into(),
                title: "Restoration probe".into(),
                effects: vec![EffectDescriptor {
                    maskable: true,
                    ..EffectDescriptor::new(EFFECT, EffectStage::Restoration)
                }],
                ..ModuleDescriptor::default()
            },
            seen: Mutex::new(Vec::new()),
            presence: PresenceModule::new(),
        }
    }
}
impl ToolModule for Probe {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<ActionInput, crate::Error> {
        Err(crate::Error::validation("probe has no actions"))
    }
    fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, crate::Error> {
        Ok(ActionPlan::NoOp)
    }
    fn validate_payload(&self, _: &str, format: u32, payload: &Value) -> Result<(), crate::Error> {
        if payload.get("smooth").is_some() {
            return Ok(());
        }
        self.presence
            .validate_payload(crate::PRESENCE_EFFECT, format, payload)
    }
    fn describe(&self, _: &str, format: u32, payload: &Value) -> Result<LayerReport, crate::Error> {
        self.presence
            .describe(crate::PRESENCE_EFFECT, format, payload)
    }
    fn compile(
        &self,
        _: &str,
        format: u32,
        payload: &Value,
        at: CompileStage,
    ) -> Result<Processing, crate::Error> {
        self.seen.lock().unwrap().push(at);
        if payload.get("smooth").is_some() {
            return Ok(Processing::Spatial(crate::SpatialOperation::new(vec![
                Arc::new(Smooth),
            ])?));
        }
        self.presence
            .compile(crate::PRESENCE_EFFECT, format, payload, at)
    }
}
fn registry() -> (ModuleRegistry, Arc<Probe>) {
    let mut registry = ModuleRegistry::developer();
    let probe = Arc::new(Probe::new());
    registry.register(probe.clone()).unwrap();
    (registry, probe)
}
fn restoration() -> Layer {
    Layer::new(EFFECT, json!({"texture":40.0}))
}

#[test]
fn restoration_stage_serializes_as_restoration() {
    assert_eq!(
        serde_json::to_value(EffectStage::Restoration).unwrap(),
        json!("restoration")
    );
    assert_eq!(EffectStage::Restoration.as_str(), "restoration");
}
#[test]
fn restoration_is_proxy_eligible() {
    let (registry, _) = registry();
    registry
        .proxy_eligible(&Recipe {
            layers: vec![restoration()],
            ..Recipe::default()
        })
        .unwrap();
}
#[test]
fn restoration_compile_stage_reaches_modules() {
    let (registry, probe) = registry();
    let recipe = Recipe {
        layers: vec![restoration()],
        ..Recipe::default()
    };
    for (stage, full) in [
        (
            Stage {
                width: 800,
                height: 600,
            },
            Stage {
                width: 800,
                height: 600,
            },
        ),
        (
            Stage {
                width: 267,
                height: 201,
            },
            Stage {
                width: 800,
                height: 600,
            },
        ),
        (
            Stage {
                width: 400,
                height: 300,
            },
            Stage {
                width: 800,
                height: 600,
            },
        ),
    ] {
        registry
            .compile_sampled(
                stage.width,
                stage.height,
                full.width,
                full.height,
                &recipe,
                crate::mask_field::MaskSampling::ThinFeature,
            )
            .unwrap();
        assert_eq!(
            *probe.seen.lock().unwrap().last().unwrap(),
            CompileStage::sampled(stage, full)
        );
    }

    let source = crate::PreviewSource::Jpeg(crate::render::tests::gradient(2049, 129));
    let context = crate::RenderContext::new();
    let recipe = Recipe {
        layers: vec![
            restoration(),
            Layer::new(
                crate::CROP_EFFECT,
                json!({"angle":0.0,"x":0.55,"y":0.0,"width":0.2,"height":1.0}),
            ),
        ],
        ..Recipe::default()
    };
    let exact = crate::render(
        &registry,
        source.input(),
        &recipe,
        crate::RenderOptions::default(),
        &context,
    )
    .unwrap();
    let full = Stage {
        width: 2049,
        height: 129,
    };
    assert_eq!(
        *probe.seen.lock().unwrap().last().unwrap(),
        CompileStage::exact(full)
    );
    let fit = exact
        .proxy_plan(crate::ProxyBounds {
            width: 205,
            height: 65,
        })
        .unwrap();
    let proxy_stage = exact.proxy_window(&registry, &recipe, fit);
    let fit = proxy_stage.plan();
    assert!(fit.window.is_some(), "Fit recipe has a cut source window");
    assert_eq!(
        *probe.seen.lock().unwrap().last().unwrap(),
        CompileStage::sampled(
            Stage {
                width: fit.width,
                height: fit.height
            },
            full
        )
    );
    let proxy = source.proxy(fit).unwrap();
    exact
        .render_proxy(
            proxy.input(),
            proxy_stage,
            &crate::Cancel::never(),
            &context,
        )
        .unwrap()
        .frame(crate::SnapshotId::new())
        .unwrap();
}

#[test]
fn restoration_and_spatial_reasons_are_reported_separately() {
    let (registry, _) = registry();
    let mut recipe = Recipe {
        layers: vec![restoration()],
        ..Recipe::default()
    };
    let approx = registry.compile(800, 600, &recipe).unwrap().approximation();
    assert!(approx.restoration && !approx.spatial);
    recipe
        .layers
        .push(Layer::new(crate::PRESENCE_EFFECT, json!({"clarity":40.0})));
    let approx = registry.compile(800, 600, &recipe).unwrap().approximation();
    assert!(approx.restoration && approx.spatial);
}
#[test]
fn restoration_boundary_is_the_leading_source_pixel_restoration_run() {
    let (registry, _) = registry();
    let mut layers = vec![
        Layer::pixel(0, 0, [1, 2, 3]),
        restoration(),
        Layer::new(crate::BASIC_EFFECT, json!({"exposure":1.0})),
    ];
    assert_eq!(
        registry
            .compile(
                10,
                10,
                &Recipe {
                    layers: layers.clone(),
                    ..Recipe::default()
                }
            )
            .unwrap()
            .restoration_boundary(),
        Some(1)
    );
    layers.swap(0, 2);
    assert_eq!(
        registry
            .compile(
                10,
                10,
                &Recipe {
                    layers,
                    ..Recipe::default()
                }
            )
            .unwrap()
            .restoration_boundary(),
        None
    );
}
#[test]
fn pixel_placement_without_restoration_is_unchanged() {
    let (registry, _) = registry();
    let layers = vec![
        Layer::new(crate::BASIC_EFFECT, json!({"exposure":1.0})),
        Layer::new(crate::PRESENCE_EFFECT, json!({"texture":20.0})),
    ];
    assert_eq!(
        registry.insertion_index_for(&layers, crate::PIXEL_EFFECT),
        1
    );
}
#[test]
fn restoration_placement_keeps_noncanonical_colour_order() {
    let (registry, _) = registry();
    let mut layers = vec![Layer::new(crate::MIXER_EFFECT, json!({})), restoration()];
    let index = registry.insertion_index_for(&layers, crate::BASIC_EFFECT);
    layers.insert(index, Layer::new(crate::BASIC_EFFECT, json!({})));
    assert_eq!(
        layers
            .iter()
            .map(|l| l.effect_id.as_str())
            .collect::<Vec<_>>(),
        vec![crate::BASIC_EFFECT, crate::MIXER_EFFECT, EFFECT]
    );
}
#[test]
fn restoration_placement_keeps_a_pixel_proof_after_colour() {
    let (registry, _) = registry();
    let layers = vec![
        Layer::new(crate::BASIC_EFFECT, json!({})),
        Layer::pixel(0, 0, [1, 2, 3]),
    ];
    assert_eq!(registry.insertion_index_for(&layers, EFFECT), 0);
}
#[test]
fn restoration_placement_all_creation_permutations_and_targets() {
    let (mut registry, _) = registry();
    let mut colour5 = super::tests::TestModule::new(
        "luxforge.colourprobe",
        "luxforge.colourprobe.adjust",
        "set-test-colour5",
        crate::Availability::Available,
    );
    colour5.0.effects[0].stage = EffectStage::Color;
    colour5.0.effects[0].order = 5;
    colour5.0.effects[0].maskable = true;
    registry.register(Arc::new(colour5)).unwrap();
    let effects = [
        EFFECT,
        crate::BASIC_EFFECT,
        "luxforge.colourprobe.adjust",
        crate::MIXER_EFFECT,
        crate::PRESENCE_EFFECT,
        crate::CROP_EFFECT,
        crate::VIGNETTE_EFFECT,
    ];
    fn permutations(values: &mut [usize], at: usize, run: &mut impl FnMut(&[usize])) {
        if at == values.len() {
            run(values);
            return;
        }
        for i in at..values.len() {
            values.swap(at, i);
            permutations(values, at + 1, run);
            values.swap(at, i);
        }
    }
    let masks = [Mask::new("Mask 1"), Mask::new("Mask 2")];
    let mut order = [0, 1, 2, 3, 4, 5, 6];
    permutations(&mut order, 0, &mut |order| {
        for raw in [false, true] {
            for target in [None, Some(&masks[0].id), Some(&masks[1].id)] {
                let mut layers = Vec::new();
                if raw {
                    layers.push(Layer::new(crate::modules::raw::RAW_EFFECT, json!({})));
                }
                layers.push(Layer::pixel(0, 0, [1, 2, 3]));
                for index in order {
                    let effect = effects[*index];
                    let mut layer = Layer::new(effect, json!({}));
                    if registry.effect_maskable(effect) {
                        layer.mask = target.cloned();
                    }
                    let at = registry.insertion_index_for_target(
                        &layers,
                        effect,
                        layer.mask.as_ref(),
                        &masks,
                    );
                    layers.insert(at, layer);
                }
                let actual: Vec<_> = layers.iter().map(|l| l.effect_id.as_str()).collect();
                let mut expected = Vec::new();
                if raw {
                    expected.push(crate::modules::raw::RAW_EFFECT);
                }
                expected.push(crate::PIXEL_EFFECT);
                expected.extend(effects);
                assert_eq!(actual, expected, "{order:?}, raw={raw}, target={target:?}");
            }
        }
    });
}

/// An independent horizontal smoother isolates the host's quantization hand-off.
struct Smooth;
impl crate::modules::SpatialUnit for Smooth {
    fn halo(&self, _: Stage) -> u32 {
        64
    }
    fn scratch_bytes(&self, _: Stage) -> u64 {
        0
    }
    fn is_finite(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "test horizontal mean r=64".into()
    }
    fn apply(
        &self,
        input: &crate::modules::Planes<'_>,
        output: &mut crate::modules::PlanesMut<'_>,
        _: Option<&crate::modules::Global>,
        _: &mut [f32],
        _: crate::modules::Parallelism,
    ) -> Result<(), crate::Error> {
        let region = output.region();
        for y in region.y0..region.y1() {
            for x in region.x0..region.x1() {
                let center = input.sample(i64::from(x), i64::from(y));
                let mut mean = center;
                for dx in -64..=64 {
                    if dx == 0 {
                        continue;
                    }
                    let sample = input.sample(i64::from(x) + dx, i64::from(y));
                    for channel in 0..3 {
                        mean[channel] += (sample[channel] - center[channel]) / 129.0;
                    }
                }
                output.set(x, y, mean);
            }
        }
        Ok(())
    }
}
#[test]
fn restoration_jpeg_ramp_under_exposure_shows_no_new_steps() {
    let (registry, _) = registry();
    let mut source = crate::render::tests::gradient(1024, 5);
    let mut pixels = Vec::new();
    for _ in 0..5 {
        for x in 0..1024 {
            let code = (8 + x * 32 / 1023) as u8;
            pixels.extend([code, code, code, 255]);
        }
    }
    source.rgba = Arc::new(pixels);
    let recipe = Recipe {
        layers: vec![
            Layer::new(EFFECT, json!({"smooth":true})),
            Layer::new(crate::BASIC_EFFECT, json!({"exposure":2.0,"shadows":100.0})),
        ],
        ..Recipe::default()
    };
    let raster =
        crate::render::testing::render(&registry, &source, crate::SnapshotId::new(), &recipe)
            .unwrap();
    for x in 1..raster.width {
        let previous = raster.pixel(x - 1, 2).unwrap()[0];
        let current = raster.pixel(x, 2).unwrap()[0];
        assert!(
            current.abs_diff(previous) <= 1,
            "at {x}: {previous} -> {current}"
        );
    }
}
