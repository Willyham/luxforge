//! The Basic module's Tone controls (Contrast, Highlights, Shadows, Whites, Blacks) end to end:
//! monotonicity through the whole production pipeline, sample/render agreement, and the declared
//! internal evaluation order against exposure.
//!
//! The frozen per-pixel numerical proof against `fixtures/basic/tone-cases.json` lives in
//! `crates/luxforge-core/src/modules/basic/tone.rs`, next to the production unit it checks; this
//! module only exercises the unit through the real host pipeline (`ModuleRegistry`, `render`,
//! `sample`), as the Exposure module does.

use super::basic_layer;
use luxforge_core::{ModuleRegistry, SnapshotId};
use luxforge_reference::tone::ToneParams;
use luxforge_testkit::fixtures::{recipe, source_of};
use luxforge_testkit::fixtures::{render, sample};
use serde_json::{Value, json};

fn tone_payload(params: ToneParams) -> Value {
    json!({
        "contrast": params.contrast,
        "highlights": params.highlights,
        "shadows": params.shadows,
        "whites": params.whites,
        "blacks": params.blacks,
    })
}

/// The 32 corners of the 5-parameter +-100 cube, matching the tone study's
/// `cube_corners`, kept as this file's own independent copy.
fn cube_corners() -> Vec<ToneParams> {
    (0u32..32)
        .map(|bits| {
            let axis = |i: u32| if (bits >> i) & 1 == 1 { 100.0 } else { -100.0 };
            ToneParams {
                contrast: axis(0),
                highlights: axis(1),
                shadows: axis(2),
                whites: axis(3),
                blacks: axis(4),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Monotonicity through the whole production pipeline
// ---------------------------------------------------------------------------------------------

/// A 1x1024 grey ramp, rendered through a real Basic layer for every cube corner of the Tone
/// parameters: the rendered output codes must never decrease along the ramp, and a grey input must
/// stay exactly grey (the same per-pixel scalar is applied to all three equal channels).
#[test]
fn nondecreasing_bytes_through_a_real_basic_layer_for_every_tone_cube_corner() {
    let width = 1024u32;
    let pixels: Vec<[u8; 3]> = (0..width)
        .map(|x| {
            let level = ((f64::from(x) / f64::from(width - 1)) * 255.0).round() as u8;
            [level, level, level]
        })
        .collect();
    let source = source_of(width, 1, &pixels);
    let registry = ModuleRegistry::builtin();
    for params in cube_corners() {
        let layer = basic_layer(tone_payload(params));
        let rendered = render(&registry, &source, SnapshotId::new(), &recipe(vec![layer]))
            .expect("a rendered ramp");
        let mut previous: Option<u8> = None;
        for x in 0..width {
            let pixel = rendered.pixel(x, 0).expect("a rendered pixel");
            assert_eq!(
                pixel[0], pixel[1],
                "{params:?} at x={x}: a grey input must stay grey"
            );
            assert_eq!(
                pixel[1], pixel[2],
                "{params:?} at x={x}: a grey input must stay grey"
            );
            if let Some(prev) = previous {
                assert!(
                    pixel[0] >= prev,
                    "{params:?}: the ramp decreased at x={x}: {prev} -> {}",
                    pixel[0]
                );
            }
            previous = Some(pixel[0]);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Contrast's sign through the real module
// ---------------------------------------------------------------------------------------------

fn standard_deviation(codes: &[u8]) -> f64 {
    let mean = codes.iter().map(|&c| f64::from(c)).sum::<f64>() / codes.len() as f64;
    (codes
        .iter()
        .map(|&c| (f64::from(c) - mean).powi(2))
        .sum::<f64>()
        / codes.len() as f64)
        .sqrt()
}

/// The owner's report as a rendered regression: Contrast `-c` must not render as `+c`. On a
/// 256-code grey ramp rendered through a real Basic layer, `-c` pulls the output codes together and
/// `+c` pushes them apart (the rendered codes' standard deviation falls below and rises above the
/// source's), and a dark and a light midtone move toward the pivot at `-c` and away from it at
/// `+c`. `+-10` moves no 8-bit grey code (see the tone study), so this starts at 50.
#[test]
fn negative_contrast_renders_flatter_than_the_source_and_positive_contrast_steeper() {
    let codes: Vec<u8> = (0u8..=255).collect();
    let pixels: Vec<[u8; 3]> = codes.iter().map(|&c| [c, c, c]).collect();
    let source = source_of(codes.len() as u32, 1, &pixels);
    let registry = ModuleRegistry::builtin();
    let rendered = |contrast: f64| -> Vec<u8> {
        let stack = recipe(vec![basic_layer(json!({ "contrast": contrast }))]);
        let frame = render(&registry, &source, SnapshotId::new(), &stack)
            .unwrap_or_else(|error| panic!("contrast {contrast}: {error}"));
        (0..codes.len() as u32)
            .map(|x| frame.pixel(x, 0).expect("a rendered pixel")[0])
            .collect()
    };
    let neutral = standard_deviation(&codes);
    for magnitude in [50.0, 100.0] {
        let flattened = rendered(-magnitude);
        let steepened = rendered(magnitude);
        assert_ne!(
            flattened, steepened,
            "-{magnitude} rendered the same codes as +{magnitude}"
        );
        let (narrow, wide) = (
            standard_deviation(&flattened),
            standard_deviation(&steepened),
        );
        assert!(
            narrow < neutral && neutral < wide,
            "+-{magnitude}: expected {narrow} < {neutral} < {wide}"
        );
        for (code, toward_pivot_is_up) in [(64usize, true), (192usize, false)] {
            let source_code = codes[code];
            let (low, high) = (flattened[code], steepened[code]);
            if toward_pivot_is_up {
                assert!(
                    low > source_code && high < source_code,
                    "code {code}: -{magnitude} gave {low}, +{magnitude} gave {high}"
                );
            } else {
                assert!(
                    low < source_code && high > source_code,
                    "code {code}: -{magnitude} gave {low}, +{magnitude} gave {high}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Sample and render agreement
// ---------------------------------------------------------------------------------------------

/// Isolated single-field values and one combined value all render and sample identically, over
/// every pixel of a small gradient.
#[test]
fn isolated_and_combined_tone_values_render_and_sample_identically() {
    let registry = ModuleRegistry::builtin();
    let width = 32u32;
    let pixels: Vec<[u8; 3]> = (0..width)
        .map(|x| {
            let level = (x * 255 / (width - 1)) as u8;
            [level, level / 2, 255 - level]
        })
        .collect();
    let source = source_of(width, 1, &pixels);
    let payloads = [
        json!({"contrast": 40.0}),
        json!({"highlights": -60.0}),
        json!({"shadows": 60.0}),
        json!({"whites": -40.0}),
        json!({"blacks": 40.0}),
        json!({
            "contrast": 40.0, "highlights": -30.0, "shadows": 20.0, "whites": 10.0,
            "blacks": -15.0,
        }),
    ];
    for payload in payloads {
        let stack = recipe(vec![basic_layer(payload.clone())]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack)
            .unwrap_or_else(|error| panic!("{payload}: {error}"));
        for x in 0..width {
            let sampled = sample(&registry, &source, &stack, x, 0)
                .expect("a sample")
                .rgba
                .expect("an opaque pixel");
            assert_eq!(
                sampled,
                rendered.pixel(x, 0).expect("a rendered pixel"),
                "{payload} at x={x}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Internal evaluation order: exposure, then tone
// ---------------------------------------------------------------------------------------------

/// A payload combining Exposure and Tone fields evaluates in the declared internal order (exposure,
/// then tone), proved against the independent f64 reference composed in that same order; the
/// reverse composition (tone, then exposure) is shown to give a measurably different result for
/// this case, so the proof is not vacuous.
#[test]
fn set_basic_with_exposure_and_tone_fields_evaluates_in_the_declared_internal_order() {
    let registry = ModuleRegistry::builtin();
    // A mid-bright, unsaturated-but-not-grey pixel: Exposure pushes it well past linear 1.0 before
    // Whites/Blacks and Contrast's endpoint-relative behaviour can react to that, which is exactly
    // the composition-order-sensitive regime this test needs.
    let input = [180u8, 150, 90];
    let source = source_of(1, 1, &[input]);
    let ev = 1.5;
    let params = ToneParams {
        contrast: 60.0,
        highlights: -40.0,
        shadows: 30.0,
        whites: -20.0,
        blacks: 20.0,
    };
    let mut payload = tone_payload(params);
    payload["exposure"] = json!(ev);
    let stack = recipe(vec![basic_layer(payload)]);
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a rendered pixel");
    let actual = rendered.pixel(0, 0).expect("the pixel");

    let linear_in = [
        luxforge_reference::srgb_to_linear(input[0]),
        luxforge_reference::srgb_to_linear(input[1]),
        luxforge_reference::srgb_to_linear(input[2]),
    ];

    // The declared order: exposure, then tone.
    let exposed = [
        luxforge_reference::exposure(linear_in[0], ev),
        luxforge_reference::exposure(linear_in[1], ev),
        luxforge_reference::exposure(linear_in[2], ev),
    ];
    let expected_in_order = luxforge_reference::tone::tone_pixel(exposed, params);
    for (channel, expected_linear) in expected_in_order.iter().enumerate() {
        let expected_code = luxforge_reference::linear_to_srgb_code(*expected_linear);
        let difference = i32::from(actual[channel]) - i32::from(expected_code);
        assert!(
            difference.abs() <= 1,
            "channel {channel}: rendered {} against the exposure-then-tone reference {expected_code}",
            actual[channel]
        );
    }

    // The reverse order (tone, then exposure) must differ measurably for this case, or the proof
    // above would hold vacuously (either order giving the same answer).
    let toned_first = luxforge_reference::tone::tone_pixel(linear_in, params);
    let reverse_order = [
        luxforge_reference::linear_to_srgb_code(luxforge_reference::exposure(toned_first[0], ev)),
        luxforge_reference::linear_to_srgb_code(luxforge_reference::exposure(toned_first[1], ev)),
        luxforge_reference::linear_to_srgb_code(luxforge_reference::exposure(toned_first[2], ev)),
    ];
    assert_ne!(
        [actual[0], actual[1], actual[2]],
        reverse_order,
        "exposure-then-tone and tone-then-exposure must give different codes for this case"
    );
}
