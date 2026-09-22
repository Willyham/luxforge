//! The Basic module's Vibrance and Saturation parameters end to end: real layers, real rendering
//! and sampling, the frozen internal order against the independent f64 reference composed the
//! same way, history/label behaviour, format refusal and reopen compatibility with the
//! exposure-only payloads TASK-003 shipped.
//!
//! Numerical rule, from `docs/design/basic-colour.md`'s frozen tolerance: a rendered code must
//! equal the f64 reference's code exactly, except where the reference's linear value sits within
//! `1e-5 + 1e-5 · |threshold|` of the exact linear threshold between two codes, where one code of
//! difference is permitted because production decodes, converts through Oklab and multiplies in
//! f32. Identity stacks and byte sharing are exact with no tolerance at all.

mod reference;

use lightwell_core::{
    BASIC_EFFECT, EFFECT_FORMAT, EditorService, ErrorKind, Layer, LayerId, ModuleRegistry,
    Mutation, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, render,
};
use reference::{RefOp, code_threshold, evaluate_pixel, exposure, srgb_to_linear};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc};

// ---------------------------------------------------------------------------------------------
// Shared helpers, matching `basic_exposure.rs`'s patterns.
// ---------------------------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
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
        fingerprint: "sha256:basic-colour-fixture".into(),
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
    }
}

fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "basic-colour-test".into(),
    }
}

fn catalog(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-basic-colour-{name}-{}.sqlite",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    path
}

/// The declared tolerance: an exact code, unless the reference's linear value sits within
/// `1e-5 + 1e-5 · |threshold|` of the threshold between the two codes, where one code of
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
    let threshold = code_threshold(crossed);
    let tolerance = 1e-5 + 1e-5 * threshold.abs();
    assert!(
        (linear.clamp(0.0, 1.0) - threshold).abs() <= tolerance,
        "{case}: rendered {actual} against {expected}, but the reference value {linear} is not \
         within {tolerance} of the code threshold {threshold}"
    );
}

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

/// A neutral colour-only patch (`vibrance: 0, saturation: 0` sent explicitly) is the canonical
/// neutral payload and renders the source buffer itself, exactly like the empty object.
#[test]
fn a_neutral_colour_layer_shares_the_source_buffer() {
    let registry = ModuleRegistry::builtin();
    let pixels: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, 255 - code, 128]).collect();
    let source = source_of(16, 16, &pixels);
    let identity = render(&registry, &source, SnapshotId::new(), &recipe(Vec::new()))
        .expect("the identity render");
    for payload in [
        json!({}),
        json!({"vibrance": 0.0, "saturation": 0.0}),
        json!({"exposure": 0.0, "vibrance": -0.0, "saturation": -0.0}),
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
    // A non-neutral colour layer does materialize a frame, so the sharing above is a real
    // property, not a render that never happened.
    let coloured = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![basic_layer(json!({"vibrance": 30.0}))]),
    )
    .expect("a coloured render");
    assert!(!Arc::ptr_eq(&coloured.rgba, &source.rgba));
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
            channels = reference::colour::apply_vibrance(channels, v);
            channels = reference::colour::apply_saturation(channels, s);
            for (channel, (&reference_byte, &linear)) in
                reference_bytes.iter().zip(channels.iter()).enumerate()
            {
                assert_colour_code(
                    pixel[channel],
                    reference_byte,
                    linear,
                    &format!("input {input:?} channel {channel} ev={ev} v={v} s={s}"),
                );
            }

            let sampled = lightwell_core::sample(&registry, &source, &stack, index as u32, 0)
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

// ---------------------------------------------------------------------------------------------
// History, reopen and format compatibility
// ---------------------------------------------------------------------------------------------

/// An exposure-only payload, saved before Vibrance and Saturation existed in a running process,
/// still opens, still renders the same bytes, and reports the new fields at neutral through
/// `values`.
#[test]
fn reopen_of_an_exposure_only_payload_is_unaffected_by_the_new_colour_fields() {
    let path = catalog("reopen");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    service
        .apply_action(
            &asset,
            mutation(0, "expose"),
            "set-basic",
            json!({"exposure": 0.75}),
        )
        .expect("an exposure-only set");
    let rendered = service.render_current(&asset).expect("a render");

    drop(service);
    let reopened = EditorService::open(&path).expect("the reopened catalog");
    assert_eq!(
        reopened.render_current(&asset).expect("after reopen").rgba,
        rendered.rgba,
        "an exposure-only payload evaluates the same bytes with vibrance/saturation implemented"
    );
    let (module, _) = reopened
        .registry()
        .effect(BASIC_EFFECT)
        .expect("the Basic provider");
    let stored = &reopened
        .describe_entry(&asset, None)
        .expect("a description")
        .layers;
    let row = stored
        .iter()
        .find(|layer| layer.effect == BASIC_EFFECT)
        .expect("the Basic row");
    assert_eq!(
        row.values,
        json!({
            "temperature": 0.0,
            "tint": 0.0,
            "exposure": 0.75,
            "contrast": 0.0,
            "highlights": 0.0,
            "shadows": 0.0,
            "whites": 0.0,
            "blacks": 0.0,
            "vibrance": 0.0,
            "saturation": 0.0,
        })
        .as_object()
        .cloned()
        .unwrap(),
        "values includes the new fields at neutral for an exposure-only stored payload"
    );
    let _ = module;

    drop(reopened);
    fs::remove_file(path).expect("the catalog is removed");
}

/// A stored payload of an unsupported format is refused, whether or not it holds colour fields.
#[test]
fn format_2_is_refused_for_a_payload_holding_colour_fields() {
    let registry = ModuleRegistry::builtin();
    let future = Layer {
        effect_format: 2,
        ..basic_layer(json!({"vibrance": 50.0, "saturation": 20.0}))
    };
    let error = registry
        .validate_layer(&future)
        .expect_err("an unsupported format");
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(error.detail.contains("unsupported effect format 2"));

    let source = source_of(2, 1, &[[10, 20, 30], [40, 50, 60]]);
    let render_error = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![future.clone()]),
    )
    .expect_err("an unsupported format never renders");
    assert_eq!(render_error.kind, ErrorKind::Incompatible);
    assert_eq!(
        future.payload,
        json!({"vibrance": 50.0, "saturation": 20.0}),
        "unchanged"
    );
}

/// History labels a single-field Colour edit by its value, a patch that returns both Colour
/// fields to neutral as `Reset Colour`, and any other multi-field Colour patch as `Basic (n
/// fields)`; `values` reports both new fields on the stored entry.
#[test]
fn labels_report_reset_colour_and_basic_n_fields_and_values_include_the_new_fields() {
    let path = catalog("labels");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;

    let vibrance_only = service
        .apply_action(
            &asset,
            mutation(0, "vibrance"),
            "set-basic",
            json!({"vibrance": 30.0}),
        )
        .expect("a vibrance-only set");
    assert_eq!(
        service
            .entry(
                &asset,
                &vibrance_only.created_entry_id.clone().expect("an entry")
            )
            .expect("the entry")
            .label,
        "Vibrance +30"
    );

    let mixed = service
        .apply_action(
            &asset,
            mutation(1, "mixed"),
            "set-basic",
            json!({"vibrance": 50.0, "saturation": 20.0}),
        )
        .expect("a mixed colour patch");
    let mixed_entry = service
        .entry(&asset, &mixed.created_entry_id.clone().expect("an entry"))
        .expect("the entry");
    assert_eq!(mixed_entry.label, "Basic (2 fields)");
    let described = service.describe_entry(&asset, None).expect("a description");
    let row = described
        .layers
        .iter()
        .find(|layer| layer.effect == BASIC_EFFECT)
        .expect("the Basic row");
    assert_eq!(
        row.values,
        json!({
            "temperature": 0.0,
            "tint": 0.0,
            "exposure": 0.0,
            "contrast": 0.0,
            "highlights": 0.0,
            "shadows": 0.0,
            "whites": 0.0,
            "blacks": 0.0,
            "vibrance": 50.0,
            "saturation": 20.0,
        })
        .as_object()
        .cloned()
        .unwrap()
    );

    let colour_reset = service
        .apply_action(
            &asset,
            mutation(2, "colour-reset"),
            "set-basic",
            json!({"vibrance": 0.0, "saturation": 0.0}),
        )
        .expect("the Colour group reset");
    assert_eq!(
        service
            .entry(&asset, &colour_reset.created_entry_id.expect("an entry"))
            .expect("the entry")
            .label,
        "Reset Colour"
    );

    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}
