//! The colour mixer module (`luxforge.mixer`) end to end: the frozen fixtures through the real
//! render path and the RAW linear path, grey invariance, and the grading fields — their
//! declaration and format marker, the reference through both render paths, dormant settings, and
//! history, resets and reopen through the editor service. What the mixer shares with every
//! field-patch module is proved once, for every such module, by the conformance suite
//! (`field_patch`), and its order after Basic by the Presence, mixer and vignette chapter of
//! `editor-acceptance`.
//!
//! Numerical rule, from `docs/design/mixer-study.md`'s frozen tolerance: a rendered code must
//! equal the f64 reference's code exactly, except where the reference's linear value sits within
//! `1e-5 + 1e-5 * |threshold|` of the exact linear threshold between two codes, where one code of
//! difference is permitted. Identity stacks and byte sharing are exact with no tolerance at all.

use luxforge_core::{
    AssetId, EditorService, ErrorKind, Layer, LinearSettings, MIXER_EFFECT, MIXER_EFFECT_FORMAT,
    ModuleRegistry, Mutation, MutationOutcome, ParameterKind, SnapshotId,
};
use luxforge_reference::{self as reference, grade, mixer::RANGE_NAMES};
use luxforge_testbase::paths;
use luxforge_testkit::fixtures::{self, linear_source_of, recipe, source_of};
use luxforge_testkit::fixtures::{render, render_linear};
use serde_json::{Map, Value, json};
use std::fs;

/// The mixer contract's relative band around a code threshold.
const CODE_BAND: f64 = 1e-5;

/// A global mixer layer holding `payload`.
fn layer(payload: Value) -> Layer {
    fixtures::layer(MIXER_EFFECT, payload)
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

// -------------------------------------------------------------------------------------------
// Field order
// -------------------------------------------------------------------------------------------

/// A layer that moves every field describes them in the declared order: the hue group, then
/// saturation, then luminance, each over the eight ranges in the reference's wheel order. The
/// expected order is rebuilt here from the reference, not read from the descriptor, so swapping two
/// fields in the module's table is caught. The conformance suite proves the description follows the
/// declared table, whatever the table says.
#[test]
fn a_layer_describes_its_fields_in_the_declared_wheel_order() {
    let registry = ModuleRegistry::builtin();
    let names = fields();
    let payload: Map<String, Value> = names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), json!(index as f64 + 1.0)))
        .collect();
    let expected: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let (range, property) = name.split_once('-').expect("a range and a property");
            let mut characters = range.chars();
            let range: String = characters
                .next()
                .expect("a range name")
                .to_uppercase()
                .chain(characters)
                .collect();
            format!("{range} {property} +{}", index + 1)
        })
        .collect();
    let report = registry
        .layer_report(&layer(Value::Object(payload)))
        .expect("the layer is described");
    assert_eq!(report.summary, expected.join(", "));
    assert!(!report.neutral);
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
                &recipe(vec![layer(json!({field.clone(): value}))]),
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
    let decoded: Vec<[f64; 3]> = pixels
        .iter()
        .map(|pixel| pixel.map(reference::srgb::decode))
        .collect();
    let linear = linear_source_of(width, height, &decoded);
    let mut desaturated = Map::new();
    for range in RANGE_NAMES {
        desaturated.insert(format!("{range}-saturation"), json!(-100.0));
    }
    let mut lit = desaturated.clone();
    for range in RANGE_NAMES {
        lit.insert(format!("{range}-luminance"), json!(40.0));
    }
    for payload in [Value::Object(desaturated), Value::Object(lit)] {
        let stack = recipe(vec![layer(payload.clone())]);
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
        let layer = layer(Value::Object(payload));

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
                    let code = reference::srgb::code(linear);
                    fixtures::assert_code_near_threshold(
                        pixel[channel],
                        code,
                        linear,
                        CODE_BAND,
                        &format!("{name} channel {channel}"),
                    );
                    let deviation = (i32::from(pixel[channel]) - i32::from(code)).unsigned_abs();
                    if f64::from(deviation) > worst {
                        worst = f64::from(deviation);
                        worst_case = format!("{name} channel {channel} (srgb8, output codes)");
                    }
                }
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
            let source = linear_source_of(pixels.len() as u32, 1, &pixels);
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
                    let code = reference::srgb::code(reference_linear);
                    fixtures::assert_code_near_threshold(
                        pixel[channel],
                        code,
                        reference_linear,
                        CODE_BAND,
                        &format!("{name} channel {channel}"),
                    );
                    let deviation = (i32::from(pixel[channel]) - i32::from(code)).unsigned_abs();
                    if f64::from(deviation) > worst {
                        worst = f64::from(deviation);
                        worst_case = format!("{name} channel {channel} (linear, output codes)");
                    }
                }
            }
        }
    }
    // Printed with --nocapture so the handoff can quote a measured figure.
    println!("maximum observed rendered-code deviation {worst} at {worst_case}");
}

// -------------------------------------------------------------------------------------------
// Grading: the fourteen fields, the reference through the real render path, dormant settings
// -------------------------------------------------------------------------------------------

/// The grading fields as the payload names them, wheel by wheel, then Blending and Balance.
fn grade_fields() -> Vec<String> {
    grade::WHEEL_NAMES
        .iter()
        .flat_map(|wheel| {
            ["hue", "saturation", "luminance"].map(|property| format!("grade-{wheel}-{property}"))
        })
        .chain(["grade-blending".to_owned(), "grade-balance".to_owned()])
        .collect()
}

/// `set-mixer` declares all thirty-eight fields in payload order, the grading fields with their
/// ranges and defaults (Blending 50), and the effect is at its own format marker.
#[test]
fn set_mixer_declares_every_hsl_and_grading_field() {
    let registry = ModuleRegistry::builtin();
    let module = registry
        .descriptors()
        .into_iter()
        .find(|module| module.id == "luxforge.mixer")
        .expect("the mixer module");
    assert_eq!(module.effects[0].format, MIXER_EFFECT_FORMAT);
    assert_eq!(MIXER_EFFECT_FORMAT, 2);
    let action = module.action("set-mixer").expect("set-mixer");
    let names: Vec<&str> = action
        .parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect();
    let expected: Vec<String> = fields().into_iter().chain(grade_fields()).collect();
    assert_eq!(names, expected);
    for parameter in &action.parameters[24..] {
        let (min, max) = match parameter.kind {
            ParameterKind::Number { min, max } => (min, max),
            ref other => panic!("{} is {other:?}", parameter.name),
        };
        let expected = match parameter.name.rsplit('-').next() {
            Some("hue") => (0.0, 360.0, 0.0),
            Some("saturation") => (0.0, 100.0, 0.0),
            Some("luminance") | Some("balance") => (-100.0, 100.0, 0.0),
            Some("blending") => (0.0, 100.0, 50.0),
            other => panic!("unexpected field {other:?}"),
        };
        assert_eq!(
            (min, max, parameter.default.as_ref().and_then(Value::as_f64)),
            (expected.0, expected.1, Some(expected.2)),
            "{}",
            parameter.name
        );
    }
}

/// A stored mixer layer at the shared format 1, the shape before grading, is refused as
/// incompatible on every path that reads it, and is never rewritten.
#[test]
fn a_mixer_layer_at_an_unsupported_format_is_refused() {
    let registry = ModuleRegistry::builtin();
    let mut old = layer(json!({"red-hue": 20.0}));
    old.effect_format = luxforge_core::EFFECT_FORMAT;
    assert_ne!(old.effect_format, MIXER_EFFECT_FORMAT);
    let error = registry
        .layer_report(&old)
        .expect_err("an unsupported format is refused");
    assert!(error.contains("unsupported effect format 1"), "{error}");
    let source = source_of(2, 1, &[[10, 20, 30], [200, 100, 50]]);
    let error = render(&registry, &source, SnapshotId::new(), &recipe(vec![old]))
        .expect_err("the stack is refused");
    assert_eq!(error.kind, ErrorKind::Incompatible, "{error:?}");
    assert!(
        error.detail.contains("unsupported effect format 1"),
        "{error:?}"
    );
}

/// The grading parameters of a payload's grading fields, defaults filled.
fn grade_params(payload: &Map<String, Value>) -> grade::GradeParams {
    let number =
        |name: &str, default: f64| payload.get(name).and_then(Value::as_f64).unwrap_or(default);
    grade::GradeParams {
        wheels: std::array::from_fn(|wheel| {
            let name = grade::WHEEL_NAMES[wheel];
            grade::Wheel {
                hue: number(&format!("grade-{name}-hue"), 0.0),
                saturation: number(&format!("grade-{name}-saturation"), 0.0),
                luminance: number(&format!("grade-{name}-luminance"), 0.0),
            }
        }),
        blending: number("grade-blending", 50.0),
        balance: number("grade-balance", 0.0),
    }
}

/// The HSL parameters of a payload's HSL fields.
fn hsl_params(payload: &Map<String, Value>) -> reference::mixer::MixerParams {
    let mut params = reference::mixer::MixerParams::neutral();
    for (range, name) in RANGE_NAMES.iter().enumerate() {
        let number = |property: &str| {
            payload
                .get(&format!("{name}-{property}"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
        };
        params.hue[range] = number("hue");
        params.saturation[range] = number("saturation");
        params.luminance[range] = number("luminance");
    }
    params
}

/// Grading payloads across the contract: every wheel, luminance alone, opposing tints, Blending and
/// Balance extremes, the hue seam, and grading over moved HSL fields.
fn grading_payloads() -> Vec<(&'static str, Value)> {
    vec![
        (
            "shadows-teal",
            json!({"grade-shadows-hue": 190, "grade-shadows-saturation": 80}),
        ),
        (
            "midtones-seam",
            json!({"grade-midtones-hue": 360, "grade-midtones-saturation": 100}),
        ),
        (
            "highlights-gold-dim",
            json!({"grade-highlights-hue": 45, "grade-highlights-saturation": 60, "grade-highlights-luminance": -100}),
        ),
        (
            "global-blue-odd-overlap",
            json!({"grade-global-hue": 240, "grade-global-saturation": 50, "grade-blending": 0, "grade-balance": 100}),
        ),
        ("shadows-lift", json!({"grade-shadows-luminance": 100})),
        (
            "split-full-blend",
            json!({"grade-shadows-hue": 20, "grade-shadows-saturation": 100, "grade-highlights-hue": 200, "grade-highlights-saturation": 100, "grade-blending": 100, "grade-balance": -60}),
        ),
        (
            "with-hsl",
            json!({"orange-hue": -40, "blue-saturation": 60, "red-luminance": -30, "grade-midtones-hue": 30, "grade-midtones-saturation": 40, "grade-global-luminance": 25}),
        ),
    ]
}

/// Production versus the f64 references (HSL, then grading) on the colourful image, through the
/// 8-bit path and the RAW linear path: every rendered code equals the reference's code except
/// within the shared code band of a threshold.
#[test]
fn grading_matches_the_reference_through_the_real_render_path() {
    let registry = ModuleRegistry::builtin();
    let (width, height, pixels) = colourful_image();
    let source = source_of(width, height, &pixels);
    let decoded: Vec<[f64; 3]> = pixels
        .iter()
        .map(|pixel| pixel.map(reference::srgb::decode))
        .collect();
    let linear = linear_source_of(width, height, &decoded);
    for (name, payload) in grading_payloads() {
        let object = payload.as_object().expect("an object");
        let (hsl, grading) = (hsl_params(object), grade_params(object));
        assert!(grading.is_active(), "{name}");
        let stack = recipe(vec![layer(payload.clone())]);
        let eight = render(&registry, &source, SnapshotId::new(), &stack)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let raw = render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap_or_else(|error| panic!("{name} (linear): {error}"));
        for (path, rendered) in [("8-bit", &eight), ("linear", &raw)] {
            for (index, input) in decoded.iter().enumerate() {
                let (x, y) = (index as u32 % width, index as u32 / width);
                let pixel = rendered.pixel(x, y).expect("a pixel");
                assert_eq!(pixel[3], 255, "{name}: alpha is never touched");
                let expected = grade::apply(reference::mixer::mix(*input, &hsl), &grading);
                for channel in 0..3 {
                    let code = reference::srgb::code(expected[channel]);
                    fixtures::assert_code_near_threshold(
                        pixel[channel],
                        code,
                        expected[channel],
                        CODE_BAND,
                        &format!("{name} {path} pixel {index} channel {channel}"),
                    );
                }
            }
        }
    }
}

/// A hue, Blending or Balance with every grading amount at zero is a kept setting that changes no
/// pixel: an HSL layer renders byte for byte the same with or without them, and a layer holding
/// only them renders exactly the source.
#[test]
fn dormant_grading_settings_change_no_pixel() {
    let registry = ModuleRegistry::builtin();
    let (width, height, pixels) = colourful_image();
    let source = source_of(width, height, &pixels);
    let dormant = json!({
        "grade-shadows-hue": 210, "grade-midtones-hue": 360, "grade-highlights-hue": 45,
        "grade-global-hue": 120, "grade-blending": 0, "grade-balance": -100,
    });
    let hsl = json!({"orange-hue": 35, "aqua-saturation": -60, "purple-luminance": 20});
    let mut both = hsl.as_object().cloned().expect("an object");
    both.extend(dormant.as_object().cloned().expect("an object"));
    let bytes = |stack| {
        render(&registry, &source, SnapshotId::new(), &recipe(stack))
            .expect("a render")
            .rgba
            .to_vec()
    };
    assert_eq!(
        bytes(vec![layer(hsl)]),
        bytes(vec![layer(Value::Object(both))])
    );
    assert_eq!(bytes(vec![layer(dormant.clone())]), bytes(Vec::new()));
    let report = registry.layer_report(&layer(dormant)).expect("described");
    assert!(
        !report.neutral,
        "a dormant setting is a stored, custom setting"
    );
    assert_eq!(report.values["grade-blending"], json!(0.0));
}

fn mutation(revision: u64, request: &str) -> Mutation {
    fixtures::mutation(revision, request, "mixer-test")
}

fn mixer_values(service: &EditorService, asset: &AssetId) -> Map<String, Value> {
    let registry = ModuleRegistry::builtin();
    let stack = service
        .state(asset)
        .expect("state")
        .current_entry
        .snapshot
        .recipe
        .layers;
    let mixers: Vec<&Layer> = stack
        .iter()
        .filter(|layer| layer.effect_id == MIXER_EFFECT)
        .collect();
    match mixers.as_slice() {
        [] => Map::new(),
        [one] => {
            assert_eq!(one.effect_format, MIXER_EFFECT_FORMAT);
            registry.layer_report(one).expect("described").values
        }
        more => panic!("{} mixer layers", more.len()),
    }
}

/// Through the editor service: a dormant hue commits a layer and survives a later edit, undo/redo
/// and reopen; a wheel's two-field patch is one entry; the Grading reset keeps HSL and the HSL
/// group resets keep grading; the module reset clears both, Blending back to 50.
#[test]
fn grading_state_survives_history_resets_and_reopen() {
    let path = paths::temp_catalog("mixer-grading");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&paths::jpeg()).expect("an import").asset.id;
    let mut revision = 0;
    let mut act = |service: &mut EditorService, request: &str, action: &str, parameters: Value| {
        let result = service
            .apply_action(&asset, mutation(revision, request), action, parameters)
            .unwrap_or_else(|error| panic!("{request}: {error:?}"));
        revision = result.revision;
        result
    };

    // A dormant hue on a photo without a mixer layer is a kept setting: it commits.
    act(
        &mut service,
        "dormant",
        "set-mixer",
        json!({"grade-shadows-hue": 210}),
    );
    assert_eq!(
        mixer_values(&service, &asset)["grade-shadows-hue"],
        json!(210.0)
    );
    let label = |service: &EditorService| service.state(&asset).expect("state").current_entry.label;
    assert_eq!(label(&service), "Shadows hue 210");

    // A wheel's hue and saturation in one patch are one entry.
    let before = service
        .history(&asset, None, 100)
        .expect("history")
        .entries
        .len();
    act(
        &mut service,
        "wheel",
        "set-mixer",
        json!({"grade-midtones-hue": 30, "grade-midtones-saturation": 45}),
    );
    assert_eq!(
        service
            .history(&asset, None, 100)
            .expect("history")
            .entries
            .len(),
        before + 1
    );
    act(&mut service, "hsl", "set-mixer", json!({"red-hue": 20}));
    let values = mixer_values(&service, &asset);
    assert_eq!(
        values["grade-shadows-hue"],
        json!(210.0),
        "the dormant hue is kept"
    );
    assert_eq!(values["grade-midtones-saturation"], json!(45.0));

    // Reset Grading keeps HSL, and its label says so.
    let grading_reset: Map<String, Value> = ModuleRegistry::builtin()
        .descriptors()
        .into_iter()
        .find(|module| module.id == "luxforge.mixer")
        .expect("the mixer")
        .action("set-mixer")
        .expect("set-mixer")
        .parameters
        .iter()
        .filter(|parameter| parameter.name.starts_with("grade-"))
        .map(|parameter| {
            (
                parameter.name.clone(),
                parameter.default.clone().expect("a default"),
            )
        })
        .collect();
    assert_eq!(grading_reset["grade-blending"], json!(50.0));
    act(
        &mut service,
        "reset-grading",
        "set-mixer",
        Value::Object(grading_reset),
    );
    assert_eq!(label(&service), "Reset Grading");
    let values = mixer_values(&service, &asset);
    assert_eq!(values["red-hue"], json!(20.0));
    assert_eq!(values["grade-shadows-hue"], json!(0.0));
    assert_eq!(values["grade-blending"], json!(50.0));

    // Undo brings the grading back exactly, redo resets it again.
    let result = service
        .undo(&asset, mutation(revision, "undo"))
        .expect("undo");
    revision = result.revision;
    assert_eq!(
        mixer_values(&service, &asset)["grade-shadows-hue"],
        json!(210.0)
    );
    let result = service
        .redo(&asset, mutation(revision, "redo"))
        .expect("redo");
    revision = result.revision;
    assert_eq!(
        mixer_values(&service, &asset)["grade-shadows-hue"],
        json!(0.0)
    );
    let result = service
        .undo(&asset, mutation(revision, "undo-again"))
        .expect("undo");
    revision = result.revision;

    // Reopen keeps every field and the format.
    drop(service);
    let mut service = EditorService::open(&path).expect("reopened");
    let values = mixer_values(&service, &asset);
    assert_eq!(values["grade-shadows-hue"], json!(210.0));
    assert_eq!(values["grade-midtones-hue"], json!(30.0));
    assert_eq!(values["red-hue"], json!(20.0));

    // The module reset clears HSL and grading together.
    let result = service
        .apply_action(
            &asset,
            mutation(revision, "reset"),
            "reset-mixer",
            json!({}),
        )
        .expect("reset");
    assert_eq!(result.outcome, MutationOutcome::Applied);
    let values = mixer_values(&service, &asset);
    assert!(
        values.iter().all(|(name, value)| *value
            == json!(if name == "grade-blending" { 50.0 } else { 0.0 })),
        "{values:?}"
    );
    let _ = std::fs::remove_file(&path);
}
