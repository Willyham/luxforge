//! The colour mixer module (`luxforge.mixer`) end to end, following `basic_colour.rs`'s
//! patterns: the frozen fixtures through the real render path and the RAW linear path, and grey
//! invariance. What the mixer shares with every field-patch module — discovery, neutral payloads,
//! drafts, history, one layer per target, sample equal to render through a crop, an unavailable
//! provider and reopen — is proved once, for every such module, by `field_patch_conformance.rs`,
//! and its order after Basic by the Presence, mixer and vignette chapter of `editor-acceptance`.
//!
//! Numerical rule, from `docs/design/mixer-study.md`'s frozen tolerance: a rendered code must
//! equal the f64 reference's code exactly, except where the reference's linear value sits within
//! `1e-5 + 1e-5 * |threshold|` of the exact linear threshold between two codes, where one code of
//! difference is permitted. Identity stacks and byte sharing are exact with no tolerance at all.

mod reference;

use luxforge_core::{
    EFFECT_FORMAT, Layer, LayerId, LinearImage, LinearSettings, MIXER_EFFECT, ModuleRegistry,
    RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, render, render_linear, sample, sample_linear,
};
use reference::mixer::RANGE_NAMES;
use serde_json::{Map, Value, json};
use std::fs;

// -------------------------------------------------------------------------------------------
// Shared helpers, matching `basic_colour.rs`'s patterns.
// -------------------------------------------------------------------------------------------

fn source_of(width: u32, height: u32, pixels: &[[u8; 3]]) -> SourceImage {
    assert_eq!(pixels.len() as u64, u64::from(width) * u64::from(height));
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        rgba.extend_from_slice(pixel);
        rgba.push(255);
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:mixer-module-fixture".into(),
        orientation: 1,
    }
}

/// A planar linear-sRGB source built from a row of `[r, g, b]` triples: R plane, then G, then B.
fn linear_source_of(pixels: &[[f64; 3]]) -> LinearImage {
    let width = pixels.len() as u32;
    let mut planes = Vec::with_capacity(pixels.len() * 3);
    for channel in 0..3 {
        planes.extend(pixels.iter().map(|pixel| pixel[channel] as f32));
    }
    LinearImage::with_fingerprint(width, 1, planes, "sha256:mixer-module-linear-fixture").unwrap()
}

fn mixer_layer(payload: Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: MIXER_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        mask: None,
        artifacts: Vec::new(),
    }
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks: Vec::new(),
        ..Recipe::default()
    }
}

/// Every declared mixer field, `<range>-<property>` in the module's payload order (hue group,
/// then saturation, then luminance, each in wheel order): the production module keeps this list
/// private, so the test rebuilds it from the shared reference's range order, which the module's
/// own unit tests hold equal to the frozen study table.
fn fields() -> Vec<String> {
    ["hue", "saturation", "luminance"]
        .iter()
        .flat_map(|property| {
            RANGE_NAMES
                .iter()
                .map(move |range| format!("{range}-{property}"))
        })
        .collect()
}

/// The declared tolerance: an exact code, unless the reference's linear value sits within
/// `1e-5 + 1e-5 * |threshold|` of the threshold between the two codes, where one code of
/// difference is permitted.
fn assert_colour_code(actual: u8, expected: u8, linear: f64, case: &str) {
    if actual == expected {
        return;
    }
    let difference = i32::from(actual) - i32::from(expected);
    assert!(
        difference.abs() <= 1,
        "{case}: rendered {actual}, reference {expected}"
    );
    let crossed = actual.max(expected);
    assert!(
        crossed >= 1,
        "{case}: code 0 has no lower threshold to sit on"
    );
    let threshold = reference::code_threshold(crossed);
    let tolerance = 1e-5 + 1e-5 * threshold.abs();
    assert!(
        (linear.clamp(0.0, 1.0) - threshold).abs() <= tolerance,
        "{case}: rendered {actual} against {expected}, but the reference value {linear} is not \
         within {tolerance} of the code threshold {threshold}"
    );
}

// -------------------------------------------------------------------------------------------
// Real-buffer proofs: grey invariance, fixtures, the linear path
// -------------------------------------------------------------------------------------------

/// Every grey code on a rendered ramp is byte-invariant under every one of the 24 sliders at
/// `±100`, through the real render path and quantizer: the same code in, the same code out, in
/// all three channels.
#[test]
fn greys_stay_byte_invariant_under_every_slider_on_a_rendered_ramp() {
    let registry = ModuleRegistry::builtin();
    let codes: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, code, code]).collect();
    let source = source_of(256, 1, &codes);
    for field in fields() {
        for value in [-100.0, 100.0] {
            let rendered = render(
                &registry,
                &source,
                SnapshotId::new(),
                &recipe(vec![mixer_layer(json!({field.clone(): value}))]),
            )
            .unwrap_or_else(|error| panic!("{field} at {value}: {error}"));
            for x in 0..256u32 {
                let pixel = rendered.pixel(x, 0).expect("a rendered pixel");
                let code = x as u8;
                assert_eq!(
                    pixel,
                    [code, code, code, 255],
                    "{field} at {value}: grey {x} drifted"
                );
            }
        }
    }
}

/// A colourful test image: a fully saturated HSV hue wheel on a near-grey backdrop (the smoke
/// scenario's fixture shape, generated here rather than read from a JPEG), the same wheel with a
/// deterministic ±3-code noise on every channel, noisy near-greys around every grey level, every
/// dark code up to 23 in each channel and a colour cube in steps of 5.
fn colourful_image() -> (u32, u32, Vec<[u8; 3]>) {
    const SIZE: u32 = 240;
    fn hsv(hue_deg: f32, saturation: f32) -> [u8; 3] {
        let x = saturation * (1.0 - ((hue_deg / 60.0).rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match (hue_deg / 60.0) as u32 % 6 {
            0 => (saturation, x, 0.0),
            1 => (x, saturation, 0.0),
            2 => (0.0, saturation, x),
            3 => (0.0, x, saturation),
            4 => (x, 0.0, saturation),
            _ => (saturation, 0.0, x),
        };
        [r, g, b].map(|channel| ((channel + 1.0 - saturation) * 255.0).round() as u8)
    }
    let mut state = 0x853c_49e6_748f_ea9bu64;
    let mut noise = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 33) % 7) as i32 - 3
    };
    let mut wheel = Vec::new();
    let radius = SIZE as f32 / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 + 0.5 - radius, y as f32 + 0.5 - radius);
            let r = (dx * dx + dy * dy).sqrt();
            wheel.push(if r > radius {
                [40, 40, 42]
            } else {
                hsv(dy.atan2(dx).to_degrees().rem_euclid(360.0), r / radius)
            });
        }
    }
    let mut pixels = wheel.clone();
    pixels.extend(
        wheel
            .iter()
            .map(|pixel| pixel.map(|c| (i32::from(c) + noise()).clamp(0, 255) as u8)),
    );
    for grey in 0..=255i32 {
        for _ in 0..30 {
            pixels.push([(); 3].map(|_| (grey + noise()).clamp(0, 255) as u8));
        }
    }
    for r in 0..24u8 {
        for g in 0..24u8 {
            for b in 0..24u8 {
                pixels.push([r, g, b]);
            }
        }
    }
    for r in (0..=255u8).step_by(5) {
        for g in (0..=255u8).step_by(5) {
            for b in (0..=255u8).step_by(5) {
                pixels.push([r, g, b]);
            }
        }
    }
    while pixels.len() % SIZE as usize != 0 {
        pixels.push([0, 0, 0]);
    }
    let height = (pixels.len() / SIZE as usize) as u32;
    (SIZE, height, pixels)
}

/// Every saturation slider at `-100` renders an arbitrary colourful image as exact greys, `R == G
/// == B` on every output pixel, through the 8-bit path and through the RAW linear path — near-grey
/// noise and the darkest pixels included, which the chroma ramp would otherwise have left
/// partly coloured — and with every luminance slider set as well.
#[test]
fn every_saturation_slider_at_minus_100_renders_exact_greys() {
    let registry = ModuleRegistry::builtin();
    let (width, height, pixels) = colourful_image();
    let source = source_of(width, height, &pixels);
    let linear = {
        let decode = |code: u8| reference::srgb_to_linear(code) as f32;
        let mut planes = Vec::with_capacity(pixels.len() * 3);
        for channel in 0..3 {
            planes.extend(pixels.iter().map(|pixel| decode(pixel[channel])));
        }
        LinearImage::with_fingerprint(width, height, planes, "sha256:mixer-module-colourful")
            .unwrap()
    };
    let mut desaturated = Map::new();
    for range in RANGE_NAMES {
        desaturated.insert(format!("{range}-saturation"), json!(-100.0));
    }
    let mut lit = desaturated.clone();
    for range in RANGE_NAMES {
        lit.insert(format!("{range}-luminance"), json!(40.0));
    }
    for payload in [Value::Object(desaturated), Value::Object(lit)] {
        let stack = recipe(vec![mixer_layer(payload.clone())]);
        let jpeg_path = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        let raw_path = render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap();
        for (path, rendered) in [("8-bit", &jpeg_path), ("linear", &raw_path)] {
            let mut worst = [0u8; 2];
            for y in 0..height {
                for x in 0..width {
                    let pixel = rendered.pixel(x, y).unwrap();
                    worst[0] = worst[0].max(pixel[0].abs_diff(pixel[1]));
                    worst[1] = worst[1].max(pixel[1].abs_diff(pixel[2]));
                }
            }
            // Printed with --nocapture so the handoff can quote the measured figure.
            println!(
                "{path} path, {} pixels, {payload}: max |R-G| {}, max |G-B| {}",
                width * height,
                worst[0],
                worst[1]
            );
            assert_eq!(worst, [0, 0], "{path} path under {payload}");
        }
    }
}

/// Production versus every one of the 576 frozen fixture cases, through the real render path
/// (the 8-bit JPEG path for `srgb8` inputs, the RAW linear path for `linear` ones), grouped by
/// parameter set so each set costs one render rather than one per case. This complements
/// `modules::mixer::unit::tests::production_matches_every_frozen_fixture_case_within_the_frozen_tolerance`,
/// which checks the unit directly; this test checks it through the host's quantizer too.
#[test]
fn production_matches_every_frozen_fixture_case_through_the_real_render_path() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/mixer/mixer-cases.json");
    let file: Value =
        serde_json::from_str(&fs::read_to_string(&path).expect("the mixer fixtures")).unwrap();
    let range_order: Vec<String> = file["range_order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_owned())
        .collect();
    let parameter_sets = file["parameter_sets"].as_array().unwrap();
    let cases = file["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 576, "every frozen case is checked");

    let registry = ModuleRegistry::builtin();
    let mut worst = 0.0f64;
    let mut worst_case = String::new();

    for set in parameter_sets {
        let set_name = set["name"].as_str().unwrap();
        let mut payload = Map::new();
        for property in ["hue", "saturation", "luminance"] {
            let values = set[property].as_array().unwrap();
            for (range, value) in range_order.iter().zip(values) {
                let value = value.as_f64().unwrap();
                if value != 0.0 {
                    payload.insert(format!("{range}-{property}"), json!(value));
                }
            }
        }
        let layer = mixer_layer(Value::Object(payload));

        let own_cases: Vec<&Value> = cases
            .iter()
            .filter(|case| case["parameters"] == json!(set_name))
            .collect();
        let srgb8: Vec<&Value> = own_cases
            .iter()
            .filter(|case| case["input"]["kind"] == json!("srgb8"))
            .copied()
            .collect();
        let linear: Vec<&Value> = own_cases
            .iter()
            .filter(|case| case["input"]["kind"] == json!("linear"))
            .copied()
            .collect();

        if !srgb8.is_empty() {
            let pixels: Vec<[u8; 3]> = srgb8
                .iter()
                .map(|case| {
                    let rgb = case["input"]["rgb"].as_array().unwrap();
                    [
                        rgb[0].as_u64().unwrap() as u8,
                        rgb[1].as_u64().unwrap() as u8,
                        rgb[2].as_u64().unwrap() as u8,
                    ]
                })
                .collect();
            let source = source_of(pixels.len() as u32, 1, &pixels);
            let rendered = render(
                &registry,
                &source,
                SnapshotId::new(),
                &recipe(vec![layer.clone()]),
            )
            .unwrap_or_else(|error| panic!("{set_name} (srgb8): {error}"));
            for (index, case) in srgb8.iter().enumerate() {
                let name = case["name"].as_str().unwrap();
                let pixel = rendered.pixel(index as u32, 0).unwrap();
                assert_eq!(pixel[3], 255, "{name}: alpha is never touched");
                let expected = case["expected_linear"].as_array().unwrap();
                for channel in 0..3 {
                    let linear = expected[channel].as_f64().unwrap();
                    let code = reference::linear_to_srgb_code(linear);
                    assert_colour_code(
                        pixel[channel],
                        code,
                        linear,
                        &format!("{name} channel {channel}"),
                    );
                    let deviation = (i32::from(pixel[channel]) - i32::from(code)).unsigned_abs();
                    if f64::from(deviation) > worst {
                        worst = f64::from(deviation);
                        worst_case = format!("{name} channel {channel} (srgb8, output codes)");
                    }
                }
                let sampled = sample(
                    &registry,
                    &source,
                    &recipe(vec![layer.clone()]),
                    index as u32,
                    0,
                )
                .expect("a sample")
                .rgba
                .expect("an opaque pixel");
                assert_eq!(
                    sampled, pixel,
                    "{name}: sample disagreed with the rendered byte"
                );
            }
        }

        if !linear.is_empty() {
            let pixels: Vec<[f64; 3]> = linear
                .iter()
                .map(|case| {
                    let rgb = case["input"]["rgb"].as_array().unwrap();
                    [
                        rgb[0].as_f64().unwrap(),
                        rgb[1].as_f64().unwrap(),
                        rgb[2].as_f64().unwrap(),
                    ]
                })
                .collect();
            let source = linear_source_of(&pixels);
            let rendered = render_linear(
                &registry,
                &source,
                SnapshotId::new(),
                &recipe(vec![layer.clone()]),
                LinearSettings::default(),
            )
            .unwrap_or_else(|error| panic!("{set_name} (linear): {error}"));
            for (index, case) in linear.iter().enumerate() {
                let name = case["name"].as_str().unwrap();
                let pixel = rendered.pixel(index as u32, 0).unwrap();
                let expected = case["expected_linear"].as_array().unwrap();
                for channel in 0..3 {
                    let reference_linear = expected[channel].as_f64().unwrap();
                    let code = reference::linear_to_srgb_code(reference_linear);
                    assert_colour_code(
                        pixel[channel],
                        code,
                        reference_linear,
                        &format!("{name} channel {channel}"),
                    );
                    let deviation = (i32::from(pixel[channel]) - i32::from(code)).unsigned_abs();
                    if f64::from(deviation) > worst {
                        worst = f64::from(deviation);
                        worst_case = format!("{name} channel {channel} (linear, output codes)");
                    }
                }
                let sampled = sample_linear(
                    &registry,
                    &source,
                    &recipe(vec![layer.clone()]),
                    LinearSettings::default(),
                    index as u32,
                    0,
                )
                .expect("a linear sample")
                .rgba
                .expect("an opaque pixel");
                assert_eq!(
                    sampled, pixel,
                    "{name}: linear sample disagreed with the rendered byte"
                );
            }
        }
    }
    // Printed with --nocapture so the handoff can quote a measured figure.
    println!("maximum observed rendered-code deviation {worst} at {worst_case}");
}
