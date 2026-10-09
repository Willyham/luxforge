//! A render equals its point evaluator and a sample equals the render: one table over every stack
//! shape, both pixel domains and both sides of the parallel threshold.

use super::testing::{frame_in, linear, point_evaluated, sample_in};
use super::tests::*;
use super::*;
use crate::{
    Component, ComponentMode, Layer, Mask, Orientation, Recipe, SnapshotId, Transform,
    modules::CropPayload,
};
use serde_json::{Value, json};

/// One stack shape of the table.
pub(super) struct Case {
    pub(super) name: &'static str,
    pub(super) layers: Vec<Layer>,
    masks: Vec<Mask>,
    /// Whether the linear domain evaluates it: it refuses a stack with more than one resample
    /// (`render::linear::tests::malformed_sources_views_and_multiple_resamples_fail_closed`).
    linear: bool,
    /// Whether a unit reads its pixel's position, so the frame must vary across it.
    positional: bool,
}

impl Case {
    fn new(name: &'static str, layers: Vec<Layer>) -> Self {
        Self {
            name,
            layers,
            masks: Vec::new(),
            linear: true,
            positional: false,
        }
    }

    fn masked(mut self, mask: &Mask) -> Self {
        self.masks = vec![mask.clone()];
        self
    }

    fn byte_only(mut self) -> Self {
        self.linear = false;
        self
    }

    fn positional(mut self) -> Self {
        self.positional = true;
        self
    }

    pub(super) fn recipe(&self) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers: self.layers.clone(),
            masks: self.masks.clone(),
            ..Recipe::default()
        }
    }
}

fn effect(effect_id: &str, payload: Value) -> Layer {
    Layer {
        id: crate::LayerId::new(),
        effect_id: effect_id.into(),
        effect_format: crate::current_effect_format(effect_id),
        payload,
        mask: None,
        artifacts: Vec::new(),
    }
}

/// Every stack shape the rows, the resample, the spatial frames and the point evaluator cover. The
/// GPU plan's tests plan every one of them too (`gpu::plan_tests`).
pub(super) fn cases() -> Vec<Case> {
    let basic = effect(
        crate::BASIC_EFFECT,
        json!({"exposure": 0.4, "contrast": 20.0, "vibrance": 15.0}),
    );
    let full_basic = effect(
        crate::BASIC_EFFECT,
        json!({
            "exposure": 0.5, "contrast": 20.0, "highlights": -30.0, "shadows": 25.0,
            "whites": 40.0, "blacks": -10.0, "vibrance": 30.0, "saturation": 15.0
        }),
    );
    let mixer = effect(
        crate::MIXER_EFFECT,
        json!({"red-hue": 20.0, "aqua-saturation": -35.0, "blue-luminance": 15.0}),
    );
    let vignette = effect(
        crate::VIGNETTE_EFFECT,
        json!({"amount": -40.0, "midpoint": 30.0}),
    );
    // Dehaze reads a global estimate of its input, so the samples read the one the render stored.
    let presence = effect(
        crate::PRESENCE_EFFECT,
        json!({"clarity": 40.0, "texture": 25.0, "dehaze": 20.0}),
    );
    let crop = Layer::crop(CropPayload {
        angle: 4.0,
        x: 0.15,
        y: 0.1,
        width: 0.7,
        height: 0.75,
    });
    let straight = Layer::crop(CropPayload {
        angle: 0.0,
        x: 0.1,
        y: 0.1,
        width: 0.7,
        height: 0.7,
    });
    let steep = Layer::crop(CropPayload {
        angle: -30.0,
        x: 0.3,
        y: 0.3,
        width: 0.4,
        height: 0.4,
    });
    let turned = Layer::orientation(Orientation {
        mirror: true,
        turns: 1,
    });
    let mut mask = Mask::new("Mask 1");
    mask.components.push(Component::new(
        "Linear 1",
        ComponentMode::Add,
        "linear",
        json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
    ));
    let masked = |layer: &Layer| Layer {
        mask: Some(mask.id.clone()),
        ..layer.clone()
    };
    vec![
        Case::new("no layer: the source rows", Vec::new()),
        Case::new(
            "replacements on either side of a colour run",
            vec![
                Layer::pixel(3, 4, [250, 10, 20]),
                basic.clone(),
                Layer::pixel(5, 6, [1, 200, 30]),
                Layer::pixel(3, 4, [9, 9, 240]),
            ],
        ),
        Case::new(
            "a replacement behind an exact orientation",
            vec![Layer::pixel(2, 2, [40, 50, 60]), turned.clone()],
        ),
        Case::new("Basic and the colour mixer", vec![full_basic, mixer]),
        Case::new(
            "colour after a straightened crop's resample",
            vec![basic.clone(), crop.clone(), vignette.clone()],
        ),
        Case::new(
            "a replacement after an orientation and a resample",
            vec![
                basic.clone(),
                turned.clone(),
                crop.clone(),
                Layer::pixel(7, 3, [255, 0, 128]),
            ],
        ),
        Case::new(
            "a replacement before a steep crop",
            vec![
                basic.clone(),
                Layer::pixel(9, 8, [200, 100, 50]),
                steep.clone(),
            ],
        ),
        Case::new(
            "colour after a spatial operation's frame",
            vec![presence.clone(), basic.clone(), vignette.clone()],
        ),
        Case::new(
            "a spatial operation then a straightened crop",
            vec![presence.clone(), crop.clone()],
        ),
        Case::new("a masked colour layer", vec![masked(&basic)]).masked(&mask),
        Case::new(
            "a masked spatial operation behind geometry",
            vec![
                basic.clone(),
                masked(&presence),
                turned.clone(),
                crop.clone(),
            ],
        )
        .masked(&mask),
        Case::new(
            "a masked colour segment behind a steep crop",
            vec![masked(&basic), steep.clone(), vignette.clone()],
        )
        .masked(&mask),
        Case::new("a positional unit alone", vec![positional_layer()]).positional(),
        Case::new(
            "a positional unit after an exact rotation in its segment",
            vec![turn(Transform::RotateLeft), positional_layer()],
        )
        .positional(),
        Case::new(
            "a positional unit after a crop resample, in output coordinates",
            vec![crop.clone(), positional_layer()],
        )
        .positional(),
        Case::new(
            "a positional unit over the whole tail",
            vec![
                exposure_layer(&[0.5]),
                turn(Transform::MirrorHorizontal),
                straight,
                positional_layer(),
            ],
        )
        .positional(),
        // Two resamples: a point query blends four recursively evaluated blends.
        Case::new(
            "two resamples",
            vec![
                turn(Transform::MirrorHorizontal),
                steep,
                scale_layer(1.5),
                Layer::pixel(0, 0, [7, 8, 253]),
            ],
        )
        .byte_only(),
        Case::new(
            "an upscale",
            vec![scale_layer(2.0), Layer::pixel(5, 5, [254, 9, 10])],
        )
        .byte_only(),
    ]
}

/// Every stack shape, on the byte domain and on the linear domain exact and approximately
/// white-balanced, over a stage narrower than one linear tap block and one several blocks wide
/// and several colour row chunks tall (with a partial last chunk), each rendered serially and
/// forced onto the pool: the frame is the point evaluator's byte at every pixel and opaque, a
/// sample through the same evaluation a sample reads is the rendered byte at every pixel, and a
/// sample through the entry point answers the stage and the rendered byte inside it and nothing
/// outside it. The linear sources are views, so the rows read them through an orientation.
#[test]
fn slow_a_frame_equals_its_point_evaluator_and_a_sample_equals_the_frame() {
    let registry = colour_registry();
    let balance = WhiteBalanceApproximation::from_matrix([
        [1.21, -0.11, -0.02],
        [-0.06, 1.08, -0.02],
        [0.01, -0.13, 1.12],
    ])
    .unwrap();
    let sizes = [
        (
            gradient(41, 29),
            varied(41, 29).with_view([1, 2, 38, 26], 6).unwrap(),
        ),
        (
            gradient(157, 101),
            varied(157, 101).with_view([2, 1, 150, 97], 3).unwrap(),
        ),
    ];
    let cases = cases();
    for (byte, planes) in &sizes {
        let sources = [
            ("byte", RenderSource::Byte(byte)),
            ("linear", linear(planes, LinearSettings::default())),
            (
                "linear, white-balanced",
                linear(
                    planes,
                    LinearSettings {
                        white_balance: Some(balance),
                    },
                ),
            ),
        ];
        for (domain, source) in sources {
            for case in &cases {
                if !case.linear && matches!(source, RenderSource::Linear { .. }) {
                    continue;
                }
                let recipe = case.recipe();
                let (width, height) = source.dimensions();
                let what = format!("{domain} {width}x{height}, {}", case.name);
                // One context, so the samples read the estimates the render stored, as a pointer
                // readout does beside the preview.
                let context = RenderContext::new();
                let (stage_width, stage_height, expected) =
                    point_evaluated(&context, &registry, source, &recipe, SpatialMode::Frames)
                        .unwrap();
                let mut rendered = None;
                for pooled in [false, true] {
                    parallel::force(Some(pooled));
                    let frame = frame_in(
                        &context,
                        &registry,
                        source,
                        SnapshotId::new(),
                        &recipe,
                        RenderOptions::default(),
                    )
                    .unwrap();
                    parallel::force(None);
                    assert_eq!(
                        (frame.width, frame.height),
                        (stage_width, stage_height),
                        "{what}"
                    );
                    assert!(
                        frame.rgba.as_slice() == expected.as_slice(),
                        "{what}, pooled {pooled}: the frame is not the point evaluator's"
                    );
                    rendered = Some(frame);
                }
                let rendered = rendered.expect("rendered");
                assert!(
                    rendered.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255),
                    "{what}: every frame is opaque"
                );
                let (_, _, sampled) =
                    point_evaluated(&context, &registry, source, &recipe, SpatialMode::Frames)
                        .unwrap();
                assert!(
                    sampled == expected,
                    "{what}: a sample is not the rendered byte"
                );
                let (w, h) = (rendered.width, rendered.height);
                for (x, y) in [(0, 0), (w / 2, h / 3), (w - 1, h - 1), (w, 0), (0, h)] {
                    let sample = sample_in(
                        &context,
                        &registry,
                        source,
                        &recipe,
                        RenderOptions::default(),
                        x,
                        y,
                    )
                    .unwrap();
                    assert_eq!((sample.width, sample.height), (w, h), "{what}");
                    assert_eq!(sample.rgba, rendered.pixel(x, y), "{what} at ({x}, {y})");
                }
                if case.positional {
                    assert_ne!(
                        rendered.pixel(0, 0),
                        rendered.pixel(w - 1, h - 1),
                        "{what}: the unit varies across the frame"
                    );
                }
            }
        }
    }
}
