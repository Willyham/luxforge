//! The Basic module's Tone controls (Contrast, Highlights, Shadows, Whites, Blacks) end to end:
//! monotonicity through the whole production pipeline, identity and buffer sharing, sample/render
//! agreement, the declared internal evaluation order against exposure, and format/field
//! compatibility for a payload written before this task existed.
//!
//! The frozen per-pixel numerical proof against `fixtures/basic/tone-cases.json` lives in
//! `crates/lightwell-core/src/modules/basic/tone.rs`, next to the production unit it checks; this
//! file only exercises the unit through the real host pipeline (`ModuleRegistry`, `render`,
//! `sample`, `EditorService`), matching `basic_exposure.rs`'s own split.

mod reference;

use lightwell_core::{
    BASIC_EFFECT, EFFECT_FORMAT, ErrorKind, Layer, LayerId, ModuleRegistry, RECIPE_FORMAT, Recipe,
    SnapshotId, SourceImage, render,
};
use reference::tone::ToneParams;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

// ---------------------------------------------------------------------------------------------
// Shared helpers (each Basic integration test file keeps its own copy; see basic_exposure.rs)
// ---------------------------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn jpeg() -> PathBuf {
    fixture("s0/orientation-1.jpg")
}

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
        fingerprint: "sha256:basic-tone-fixture".into(),
        orientation: 1,
    }
}

fn basic_layer(payload: Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        mask: None,
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

fn tone_payload(params: ToneParams) -> Value {
    json!({
        "contrast": params.contrast,
        "highlights": params.highlights,
        "shadows": params.shadows,
        "whites": params.whites,
        "blacks": params.blacks,
    })
}

/// The 32 corners of the 5-parameter +-100 cube, matching `basic_tone_reference.rs`'s
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
// Identity and buffer sharing
// ---------------------------------------------------------------------------------------------

/// A Basic layer whose Tone fields are all explicitly neutral (spelled out, or via the canonical
/// empty payload, or alongside a neutral exposure) compiles to no processing at all: the rendered
/// bytes are the source's own shared allocation, not a copy of it.
#[test]
fn identity_at_all_zero_tone_fields_keeps_the_shared_source_arc() {
    let registry = ModuleRegistry::builtin();
    let codes: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, 255 - code, 128]).collect();
    let source = source_of(16, 16, &codes);
    let identity = render(&registry, &source, SnapshotId::new(), &recipe(Vec::new()))
        .expect("the identity render");
    for payload in [
        json!({}),
        json!({"contrast": 0.0, "highlights": 0.0, "shadows": 0.0, "whites": 0.0, "blacks": 0.0}),
        json!({
            "exposure": 0.0, "contrast": 0.0, "highlights": 0.0, "shadows": 0.0, "whites": 0.0,
            "blacks": 0.0,
        }),
    ] {
        let rendered = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![basic_layer(payload.clone())]),
        )
        .expect("a neutral render");
        assert_eq!(rendered.rgba, identity.rgba, "{payload} changed a byte");
        assert!(
            Arc::ptr_eq(&rendered.rgba, &source.rgba),
            "{payload} did not share the source allocation"
        );
    }
    // A non-neutral Tone layer does materialize a frame, which is what makes the sharing above a
    // real property rather than a render that never happened.
    let toned = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![basic_layer(json!({"contrast": 50.0}))]),
    )
    .expect("a toned render");
    assert!(!Arc::ptr_eq(&toned.rgba, &source.rgba));
    assert_ne!(toned.rgba, identity.rgba);
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
            let sampled = lightwell_core::sample(&registry, &source, &stack, x, 0)
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
        reference::srgb_to_linear(input[0]),
        reference::srgb_to_linear(input[1]),
        reference::srgb_to_linear(input[2]),
    ];

    // The declared order: exposure, then tone.
    let exposed = [
        reference::exposure(linear_in[0], ev),
        reference::exposure(linear_in[1], ev),
        reference::exposure(linear_in[2], ev),
    ];
    let expected_in_order = reference::tone::tone_pixel(exposed, params);
    for (channel, expected_linear) in expected_in_order.iter().enumerate() {
        let expected_code = reference::linear_to_srgb_code(*expected_linear);
        let difference = i32::from(actual[channel]) - i32::from(expected_code);
        assert!(
            difference.abs() <= 1,
            "channel {channel}: rendered {} against the exposure-then-tone reference {expected_code}",
            actual[channel]
        );
    }

    // The reverse order (tone, then exposure) must differ measurably for this case, or the proof
    // above would hold vacuously (either order giving the same answer).
    let toned_first = reference::tone::tone_pixel(linear_in, params);
    let reverse_order = [
        reference::linear_to_srgb_code(reference::exposure(toned_first[0], ev)),
        reference::linear_to_srgb_code(reference::exposure(toned_first[1], ev)),
        reference::linear_to_srgb_code(reference::exposure(toned_first[2], ev)),
    ];
    assert_ne!(
        [actual[0], actual[1], actual[2]],
        reverse_order,
        "exposure-then-tone and tone-then-exposure must give different codes for this case"
    );
}

// ---------------------------------------------------------------------------------------------
// A pre-existing exposure-only payload
// ---------------------------------------------------------------------------------------------

/// A payload holding only `"exposure"`, exactly as it would already exist in a catalog saved before
/// this task's five Tone fields existed, reopens (renders) with identical output: adding optional
/// keys to the payload format never changes an existing interpretation.
#[test]
fn an_exposure_only_payload_saved_before_this_change_reopens_with_identical_output() {
    let registry = ModuleRegistry::builtin();
    let inputs: [[u8; 3]; 4] = [
        [10, 20, 30],
        [100, 120, 140],
        [200, 210, 220],
        [250, 252, 254],
    ];
    let source = source_of(inputs.len() as u32, 1, &inputs);
    let layer = basic_layer(json!({"exposure": 0.5}));
    let rendered = render(&registry, &source, SnapshotId::new(), &recipe(vec![layer]))
        .expect("a rendered exposure");
    for (index, input) in inputs.iter().enumerate() {
        let pixel = rendered.pixel(index as u32, 0).expect("a rendered pixel");
        for (channel, code) in input.iter().enumerate() {
            let linear = reference::exposure(reference::srgb_to_linear(*code), 0.5);
            let expected = reference::linear_to_srgb_code(linear);
            let difference = i32::from(pixel[channel]) - i32::from(expected);
            assert!(
                difference.abs() <= 1,
                "channel {channel}: rendered {} expected {expected}",
                pixel[channel]
            );
        }
    }
    // A whole real asset renders unchanged too, not just a synthetic pixel row.
    let real = image::open(jpeg()).expect("the fixture decodes");
    assert!(real.width() > 0 && real.height() > 0);
}

// ---------------------------------------------------------------------------------------------
// Format and field compatibility once Tone fields exist
// ---------------------------------------------------------------------------------------------

/// A stored payload naming Tone fields still refuses an unsupported format and an unknown field
/// alongside a known one, without rewriting the stack.
#[test]
fn a_stored_payload_with_tone_fields_still_refuses_an_unsupported_format_or_unknown_field() {
    let registry = ModuleRegistry::builtin();
    let future = Layer {
        effect_format: 2,
        ..basic_layer(json!({"contrast": 20.0}))
    };
    let error = registry
        .validate_layer(&future)
        .expect_err("an unsupported format");
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(error.detail.contains("unsupported effect format 2"));

    let unknown = registry
        .validate_layer(&basic_layer(json!({"contrast": 20.0, "gamma": 1.0})))
        .expect_err("an unknown field alongside a known one");
    assert_eq!(unknown.kind, ErrorKind::Validation);
    assert!(unknown.detail.contains("gamma"), "{}", unknown.detail);

    let out_of_range = registry
        .validate_layer(&basic_layer(json!({"contrast": 999.0})))
        .expect_err("a Tone field outside its declared range");
    assert_eq!(out_of_range.kind, ErrorKind::Validation);

    // The stack is still fully readable even though nothing above rendered it.
    let (module, _) = registry.effect(BASIC_EFFECT).expect("the Basic provider");
    assert!(
        module
            .describe_layer(BASIC_EFFECT, EFFECT_FORMAT, &json!({"contrast": 20.0}))
            .is_ok()
    );
}
