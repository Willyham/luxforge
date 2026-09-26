//! The Basic module's Vibrance and Saturation parameters end to end: real layers, real rendering
//! and sampling, and the frozen internal order against the independent f64 reference composed the
//! same way. Basic's own label words are in `modules::basic`'s unit tests.
//!
//! Numerical rule, from `docs/design/basic-colour.md`'s frozen tolerance: the band around a code
//! threshold within which one code of difference is permitted is `1e-5 + 1e-5 · |threshold|`,
//! because production decodes, converts through Oklab and multiplies in f32.

use super::basic_layer;
use luxforge_core::{ModuleRegistry, SnapshotId};
use luxforge_reference::{RefOp, evaluate_pixel, exposure, srgb_to_linear};
use luxforge_testkit::fixtures::{self, recipe, source_of};
use luxforge_testkit::fixtures::{render, sample};
use serde_json::json;

/// The Colour contract's relative band around a code threshold.
const CODE_BAND: f64 = 1e-5;

/// The eight representative hues the combined-order test sweeps: primaries, secondaries, one
/// skin-like patch (named in the task) and mid grey.
fn hue_sweep() -> Vec<[u8; 3]> {
    vec![
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [0, 255, 255],
        [255, 0, 255],
        [255, 255, 0],
        [224, 172, 140],
        [128, 128, 128],
    ]
}

// ---------------------------------------------------------------------------------------------
// Real-buffer proofs
// ---------------------------------------------------------------------------------------------

/// Saturation −100 renders every pixel of a colourful buffer to equal R, G, B bytes, through a
/// real Basic layer and the host's own quantizer, not the reference's.
#[test]
fn saturation_negative_100_renders_grayscale_through_a_real_layer_and_quantizer() {
    let registry = ModuleRegistry::builtin();
    let pixels: Vec<[u8; 3]> = (0u32..256)
        .map(|i| {
            [
                (i % 256) as u8,
                ((i * 3 + 40) % 256) as u8,
                ((i * 7 + 90) % 256) as u8,
            ]
        })
        .collect();
    let source = source_of(pixels.len() as u32, 1, &pixels);
    let rendered = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![basic_layer(json!({"saturation": -100.0}))]),
    )
    .expect("a rendered saturation --100 frame");
    for x in 0..pixels.len() as u32 {
        let pixel = rendered.pixel(x, 0).expect("a rendered pixel");
        assert!(
            pixel[0] == pixel[1] && pixel[1] == pixel[2],
            "pixel {x} {pixel:?} is not grayscale"
        );
        assert_eq!(pixel[3], 255, "alpha is never touched");
    }
}

/// Greys stay grey through a real Basic layer and the host's quantizer for a sweep of `v` and
/// `s`, complementing the exhaustive f32-unit sweep in `modules::basic::colour::tests`.
#[test]
fn greys_stay_grey_through_a_real_layer_for_a_sweep_of_v_and_s() {
    let registry = ModuleRegistry::builtin();
    let codes: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, code, code]).collect();
    let source = source_of(256, 1, &codes);
    for v in [-100.0, -50.0, 0.0, 50.0, 100.0] {
        for s in [-100.0, -50.0, 0.0, 50.0, 100.0] {
            let rendered = render(
                &registry,
                &source,
                SnapshotId::new(),
                &recipe(vec![basic_layer(json!({"vibrance": v, "saturation": s}))]),
            )
            .expect("a rendered grey sweep frame");
            for x in 0..256u32 {
                let pixel = rendered.pixel(x, 0).expect("a rendered pixel");
                let spread = pixel[0].abs_diff(pixel[1]).max(pixel[1].abs_diff(pixel[2]));
                assert!(
                    spread <= 1,
                    "grey code {x} drifted at v={v} s={s}: {pixel:?}"
                );
            }
        }
    }
}

/// A sample and a rendered byte are the same evaluation for a combined exposure/vibrance/
/// saturation layer over a small hue sweep, and both agree with the independent f64 reference
/// composed in the frozen internal order: exposure, then vibrance, then saturation.
#[test]
fn combined_vibrance_saturation_with_exposure_matches_the_reference_in_frozen_order() {
    let registry = ModuleRegistry::builtin();
    let pixels = hue_sweep();
    let source = source_of(pixels.len() as u32, 1, &pixels);
    for (ev, v, s) in [
        (0.5, 30.0, -20.0),
        (1.0, -50.0, 50.0),
        (-1.0, 100.0, 100.0),
        (0.0, -100.0, 0.0),
    ] {
        let stack = recipe(vec![basic_layer(
            json!({"exposure": ev, "vibrance": v, "saturation": s}),
        )]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack)
            .expect("a rendered combined frame");
        for (index, input) in pixels.iter().enumerate() {
            let pixel = rendered.pixel(index as u32, 0).expect("a rendered pixel");
            assert_eq!(pixel[3], 255, "alpha is never touched");
            let reference_bytes = evaluate_pixel(
                *input,
                &[
                    RefOp::Exposure(ev),
                    RefOp::Vibrance(v),
                    RefOp::Saturation(s),
                ],
            );
            // The pre-quantization linear reference value, composed the same way, for the
            // boundary tolerance check.
            let mut channels = [
                srgb_to_linear(input[0]),
                srgb_to_linear(input[1]),
                srgb_to_linear(input[2]),
            ];
            for channel in &mut channels {
                *channel = exposure(*channel, ev);
            }
            channels = luxforge_reference::colour::apply_vibrance(channels, v);
            channels = luxforge_reference::colour::apply_saturation(channels, s);
            for (channel, (&reference_byte, &linear)) in
                reference_bytes.iter().zip(channels.iter()).enumerate()
            {
                fixtures::assert_code_near_threshold(
                    pixel[channel],
                    reference_byte,
                    linear,
                    CODE_BAND,
                    &format!("input {input:?} channel {channel} ev={ev} v={v} s={s}"),
                );
            }

            let sampled = sample(&registry, &source, &stack, index as u32, 0)
                .expect("a sample")
                .rgba
                .expect("an opaque pixel");
            assert_eq!(sampled, pixel, "sample disagreed with the rendered byte");
        }
    }
}

/// A large positive exposure pushes linear values well above 1.0 before vibrance and saturation
/// run; both stay finite through the signed cube root and the render succeeds, clamping only at
/// the output boundary.
#[test]
fn an_out_of_gamut_positive_exposure_stays_finite_and_renders_with_vibrance_and_saturation() {
    let registry = ModuleRegistry::builtin();
    let pixels = hue_sweep();
    let source = source_of(pixels.len() as u32, 1, &pixels);
    let stack = recipe(vec![basic_layer(
        json!({"exposure": 5.0, "vibrance": 100.0, "saturation": 100.0}),
    )]);
    let rendered = render(&registry, &source, SnapshotId::new(), &stack)
        .expect("an out-of-gamut render stays finite and succeeds");
    assert_eq!(rendered.width, pixels.len() as u32);
    for x in 0..pixels.len() as u32 {
        let pixel = rendered.pixel(x, 0).expect("a rendered pixel");
        assert_eq!(pixel[3], 255);
        // Every channel is a valid u8 by construction; the meaningful assertion is that the
        // render produced one at all instead of failing with `resource-limit`.
        let _ = pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32;
    }
}
