//! The `lightwell.vignette` module (TASK-008) end to end: real layers through the real host
//! pipeline (`ModuleRegistry`, `render`, `sample`, `render_linear`, `sample_linear`,
//! `EditorService`), matching the pattern `basic_tone.rs` and `basic_colour.rs` set for Basic.
//!
//! Descriptor shape, plan semantics (commit at the end, update in place, no-op, reset, the
//! missing-key-means-default rule), labels, `values`, `validate_payload` refusals, two-layer
//! ambiguity, `describe_layer` and `compile`'s neutral/non-neutral split are exercised in-crate,
//! next to the module, in `crates/lightwell-core/src/modules/vignette/mod.rs` and `unit.rs`. This
//! file covers what only the real host pipeline can prove: placement and recentring through
//! `EditorService`, production against every frozen oracle fixture through `render`, mirror/flip
//! symmetry on rendered bytes, identity/sharing, sample/render agreement through a crop resample,
//! and RAW linear-path agreement.

mod reference;

use lightwell_core::{
    AssetId, CropPayload, EFFECT_FORMAT, EditorService, Layer, LayerId, LinearImage,
    LinearSettings, ModuleRegistry, Mutation, Orientation, RECIPE_FORMAT, Recipe, SnapshotId,
    SourceImage, Transform, VIGNETTE_EFFECT, render, render_linear, sample, sample_linear, schemas,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

// -------------------------------------------------------------------------------------------
// Shared helpers, matching the other Basic/vignette test files' own copies.
// -------------------------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn jpeg() -> PathBuf {
    fixture("s0/orientation-1.jpg")
}

fn catalog(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightwell-vignette-module-{name}-{}.sqlite",
        std::process::id()
    ))
}

fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "vignette-module-test".into(),
    }
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
        fingerprint: "sha256:vignette-module-fixture".into(),
        orientation: 1,
    }
}

fn vignette_layer(payload: Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: VIGNETTE_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload,
    }
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
    }
}

fn layers_of(service: &EditorService, asset: &AssetId) -> Vec<Layer> {
    service
        .state(asset)
        .expect("state")
        .current_entry
        .snapshot
        .recipe
        .layers
}

// -------------------------------------------------------------------------------------------
// Descriptor / module.list / schema.list, through the real built-in registry.
// -------------------------------------------------------------------------------------------

/// The vignette module is registered last, declares a finish-stage effect with one collapsed
/// group of four sliders and both actions, and both actions surface as `edit.*` methods through
/// `schema.list`.
#[test]
fn the_builtin_registry_reports_vignette_last_with_the_finish_effect_and_both_actions() {
    let registry = ModuleRegistry::builtin();
    let descriptors = registry.descriptors();
    let last = descriptors.last().expect("at least one module");
    assert_eq!(last.id, "lightwell.vignette");
    assert!(last.collapsed);
    assert_eq!(last.effects.len(), 1);
    assert_eq!(last.effects[0].id, VIGNETTE_EFFECT);
    assert_eq!(last.effects[0].stage, lightwell_core::EffectStage::Finish);

    let schema = schemas(&registry);
    let methods = schema["methods"].as_object().expect("a methods object");
    assert!(
        methods.contains_key("edit.set-vignette"),
        "{:?}",
        methods.keys().collect::<Vec<_>>()
    );
    assert!(
        methods.contains_key("edit.reset-vignette"),
        "{:?}",
        methods.keys().collect::<Vec<_>>()
    );
}

// -------------------------------------------------------------------------------------------
// Placement: a committed vignette layer stays last through the whole host editing surface.
// -------------------------------------------------------------------------------------------

/// A first `set-vignette` commits at the end of the stack; a rotation, a mirror and a crop after
/// it each join the geometry tail before the vignette layer, so the vignette stays last and
/// therefore keeps recentring on whatever the tail produces.
#[test]
fn a_vignette_layer_stays_last_through_rotate_mirror_and_crop() {
    let path = catalog("placement");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;

    service
        .apply_action(
            &asset,
            mutation(0, "vignette"),
            "set-vignette",
            json!({"amount": -35.0}),
        )
        .expect("a vignette commit");
    assert_eq!(
        layers_of(&service, &asset)
            .last()
            .expect("a layer")
            .effect_id,
        VIGNETTE_EFFECT
    );

    for (revision, request, action, parameters) in [
        (
            1,
            "rotate",
            "transform",
            json!({"transform": "rotate-left"}),
        ),
        (
            2,
            "mirror",
            "transform",
            json!({"transform": "mirror-horizontal"}),
        ),
        (
            3,
            "crop",
            "crop",
            json!({"x": 0.1, "y": 0.1, "width": 0.5, "height": 0.5}),
        ),
    ] {
        service
            .apply_action(&asset, mutation(revision, request), action, parameters)
            .unwrap_or_else(|error| panic!("{request}: {error:?}"));
        let stack = layers_of(&service, &asset);
        assert_eq!(
            stack.last().expect("a layer").effect_id,
            VIGNETTE_EFFECT,
            "after {request}: {:?}",
            stack
                .iter()
                .map(|layer| &layer.effect_id)
                .collect::<Vec<_>>()
        );
    }
    let _ = std::fs::remove_file(&path);
}

// -------------------------------------------------------------------------------------------
// Recentring: the vignette recomputes its mask on the crop's new stage, not the old one.
// -------------------------------------------------------------------------------------------

/// A vignette committed over a `96x64` source, then cropped down to exactly `24x16` (one of the
/// study's own frozen fixture sizes), renders the new stage's own corner and near-centre pixels
/// to what the frozen reference computes for a fresh `24x16` stage -- not the pre-crop `96x64`
/// one -- proving the mask is recomputed independently after the crop moves in front of it.
#[test]
fn the_vignette_recentres_on_the_stage_a_crop_produces() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (96u32, 64u32);
    let pixels = vec![[128u8, 96, 64]; (width * height) as usize];
    let source = source_of(width, height, &pixels);

    // x=0, y=0, width=0.25, height=0.25 of a 96x64 source crops out exactly 24x16: one of the
    // study's own three frozen fixture sizes.
    let crop = Layer::crop(CropPayload {
        angle: 0.0,
        x: 0.0,
        y: 0.0,
        width: 0.25,
        height: 0.25,
    });
    let amount = -100.0;
    let params = json!({"amount": amount, "midpoint": 50.0, "roundness": 0.0, "feather": 50.0});
    let stack = recipe(vec![crop, vignette_layer(params)]);

    let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a render");
    assert_eq!((rendered.width, rendered.height), (24, 16));

    let input_linear = [
        reference::srgb_to_linear(128),
        reference::srgb_to_linear(96),
        reference::srgb_to_linear(64),
    ];
    for (x, y) in [(0u32, 0u32), (23, 0), (0, 15), (23, 15), (12, 8)] {
        let expected_mask = reference::vignette::mask(
            x,
            y,
            24,
            16,
            &reference::vignette::VignetteParams {
                amount,
                midpoint: 50.0,
                roundness: 0.0,
                feather: 50.0,
            },
        );
        let expected_linear = reference::vignette::apply(input_linear, expected_mask, amount);
        let expected = [
            reference::linear_to_srgb_code(expected_linear[0]),
            reference::linear_to_srgb_code(expected_linear[1]),
            reference::linear_to_srgb_code(expected_linear[2]),
        ];
        let actual = rendered.pixel(x, y).expect("an in-bounds pixel");
        for channel in 0..3 {
            let diff = i32::from(actual[channel]).abs_diff(i32::from(expected[channel]));
            assert!(
                diff <= 1,
                "({x},{y}) channel {channel}: actual {} expected {} (mask {expected_mask})",
                actual[channel],
                expected[channel]
            );
        }
    }
    // The near-centre pixel of the new, smaller stage reads mask 0 at the defaults (the same
    // "recentres, does not inherit" property the reference proves for the mask alone): its byte
    // must be exactly the quantized input, unmoved by the vignette at all.
    let centre = rendered.pixel(12, 8).expect("an in-bounds pixel");
    let centre_expected = [
        reference::linear_to_srgb_code(input_linear[0]),
        reference::linear_to_srgb_code(input_linear[1]),
        reference::linear_to_srgb_code(input_linear[2]),
    ];
    assert_eq!(
        &centre[..3],
        &centre_expected[..],
        "near-centre must be unmoved"
    );
}

// -------------------------------------------------------------------------------------------
// Production against every frozen oracle fixture, through the real render path.
// -------------------------------------------------------------------------------------------

/// Every `fixtures/vignette/amount-cases.json` case, quantized to the 8-bit input the byte path
/// actually starts from (the fixture's raw `f64` linear input cannot be fed byte-exact through an
/// 8-bit `SourceImage`), rendered through a real `lightwell.vignette` layer and compared to the
/// reference recomputed from that same quantized input, within one output code per channel.
#[test]
fn production_matches_every_amount_case_through_the_real_render_path() {
    let registry = ModuleRegistry::builtin();
    let raw = std::fs::read_to_string(fixture("vignette/amount-cases.json"))
        .expect("fixtures/vignette/amount-cases.json");
    let cases: Vec<Value> = serde_json::from_str(&raw).expect("a JSON array");
    assert_eq!(cases.len(), 60);
    for case in &cases {
        let width = case["width"].as_u64().unwrap() as u32;
        let height = case["height"].as_u64().unwrap() as u32;
        let x = case["x"].as_u64().unwrap() as u32;
        let y = case["y"].as_u64().unwrap() as u32;
        let input = case["input_linear_rgb"].as_array().unwrap();
        let input_linear = [
            input[0].as_f64().unwrap(),
            input[1].as_f64().unwrap(),
            input[2].as_f64().unwrap(),
        ];
        let byte = input_linear.map(reference::linear_to_srgb_code);
        let quantized_linear = byte.map(reference::srgb_to_linear);
        let params = &case["params"];
        let amount = params["amount"].as_f64().unwrap();
        let expected_mask = case["expected_mask"].as_f64().unwrap();
        let expected_linear = reference::vignette::apply(quantized_linear, expected_mask, amount);
        let expected = expected_linear.map(reference::linear_to_srgb_code);

        let pixels = vec![byte; (width * height) as usize];
        let source = source_of(width, height, &pixels);
        let stack = recipe(vec![vignette_layer(json!({
            "amount": amount,
            "midpoint": params["midpoint"],
            "roundness": params["roundness"],
            "feather": params["feather"],
        }))]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack)
            .unwrap_or_else(|error| panic!("{}: {error:?}", case["label"]));
        let actual = rendered.pixel(x, y).expect("an in-bounds pixel");
        for channel in 0..3 {
            let diff = i32::from(actual[channel]).abs_diff(i32::from(expected[channel]));
            assert!(
                diff <= 1,
                "{}: channel {channel} actual {} expected {}",
                case["label"],
                actual[channel],
                expected[channel]
            );
        }
    }
}

/// Every `fixtures/vignette/mask-cases.json` case records the mask at `amount = 0`, which is
/// invisible in a rendered byte by design (the whole layer is neutral there). To check the mask
/// geometry itself through the real render path, each case's geometry (`width`, `height`, `x`,
/// `y`, `midpoint`, `roundness`, `feather`) is reused with `amount` overridden to `-100` (full
/// darken, `gain = 1 - mask`), so the fixture's own `expected_mask` becomes directly observable
/// as a byte through a real vignette layer.
#[test]
fn production_matches_every_mask_case_geometry_through_the_real_render_path() {
    let registry = ModuleRegistry::builtin();
    let raw = std::fs::read_to_string(fixture("vignette/mask-cases.json"))
        .expect("fixtures/vignette/mask-cases.json");
    let cases: Vec<Value> = serde_json::from_str(&raw).expect("a JSON array");
    assert_eq!(cases.len(), 202);
    let grey_byte = 160u8;
    let grey_linear = reference::srgb_to_linear(grey_byte);
    for case in &cases {
        let width = case["width"].as_u64().unwrap() as u32;
        let height = case["height"].as_u64().unwrap() as u32;
        let x = case["x"].as_u64().unwrap() as u32;
        let y = case["y"].as_u64().unwrap() as u32;
        let params = &case["params"];
        let expected_mask = case["expected_mask"].as_f64().unwrap();
        let amount = -100.0;
        let expected_linear = reference::vignette::apply([grey_linear; 3], expected_mask, amount);
        let expected = reference::linear_to_srgb_code(expected_linear[0]);

        let pixels = vec![[grey_byte; 3]; (width * height) as usize];
        let source = source_of(width, height, &pixels);
        let stack = recipe(vec![vignette_layer(json!({
            "amount": amount,
            "midpoint": params["midpoint"],
            "roundness": params["roundness"],
            "feather": params["feather"],
        }))]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack)
            .unwrap_or_else(|error| panic!("{}: {error:?}", case["label"]));
        let actual = rendered.pixel(x, y).expect("an in-bounds pixel");
        let diff = i32::from(actual[0]).abs_diff(i32::from(expected));
        assert!(
            diff <= 1,
            "{}: mask {expected_mask} actual byte {} expected {}",
            case["label"],
            actual[0],
            expected
        );
    }
}

// -------------------------------------------------------------------------------------------
// Mirror/flip symmetry and centre invariance on production bytes.
// -------------------------------------------------------------------------------------------

fn mirror_horizontally(rendered: &lightwell_core::Raster) -> Vec<[u8; 4]> {
    (0..rendered.height)
        .flat_map(|y| {
            (0..rendered.width).map(move |x| rendered.pixel(rendered.width - 1 - x, y).unwrap())
        })
        .collect()
}

fn flip_vertically(rendered: &lightwell_core::Raster) -> Vec<[u8; 4]> {
    (0..rendered.height)
        .flat_map(|y| {
            (0..rendered.width).map(move |x| rendered.pixel(x, rendered.height - 1 - y).unwrap())
        })
        .collect()
}

fn raster_pixels(rendered: &lightwell_core::Raster) -> Vec<[u8; 4]> {
    (0..rendered.height)
        .flat_map(|y| (0..rendered.width).map(move |x| rendered.pixel(x, y).unwrap()))
        .collect()
}

/// `render(vignette after mirror)` equals `mirror(render(vignette))` byte for byte: the vignette
/// mask's exact mirror symmetry (`docs/design/vignette-study.md`) holds all the way through the
/// real 8-bit pipeline, not only in the frozen `f64` reference. Likewise for a vertical flip.
#[test]
fn production_is_exactly_mirror_and_flip_symmetric() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (9u32, 7u32);
    let pixels: Vec<[u8; 3]> = (0..width * height)
        .map(|i| {
            [
                ((i * 7) % 256) as u8,
                ((i * 53) % 256) as u8,
                ((i * 131) % 256) as u8,
            ]
        })
        .collect();
    let source = source_of(width, height, &pixels);
    let params = json!({"amount": -60.0, "midpoint": 40.0, "roundness": 30.0, "feather": 60.0});

    let plain = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![vignette_layer(params.clone())]),
    )
    .expect("a plain render");

    let after_mirror = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![
            Layer::orientation(Orientation::of(Transform::MirrorHorizontal)),
            vignette_layer(params.clone()),
        ]),
    )
    .expect("a render after mirror");
    assert_eq!(raster_pixels(&after_mirror), mirror_horizontally(&plain));

    let after_flip = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![
            Layer::orientation(Orientation::of(Transform::FlipVertical)),
            vignette_layer(params),
        ]),
    )
    .expect("a render after flip");
    assert_eq!(raster_pixels(&after_flip), flip_vertically(&plain));
}

// -------------------------------------------------------------------------------------------
// Identity and buffer sharing.
// -------------------------------------------------------------------------------------------

/// A vignette layer whose amount is 0 -- however `midpoint`, `roundness` and `feather` read --
/// compiles to no processing at all: the rendered bytes are the source's own shared allocation.
#[test]
fn a_zero_amount_layer_keeps_the_shared_source_allocation_whatever_the_other_fields_hold() {
    let registry = ModuleRegistry::builtin();
    let codes: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, 255 - code, 128]).collect();
    let source = source_of(16, 16, &codes);
    let identity = render(&registry, &source, SnapshotId::new(), &recipe(Vec::new()))
        .expect("the identity render");
    for payload in [
        json!({}),
        json!({"midpoint": 90.0, "roundness": -80.0, "feather": 5.0}),
        json!({"amount": 0.0, "midpoint": 10.0}),
    ] {
        let rendered = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![vignette_layer(payload.clone())]),
        )
        .unwrap_or_else(|error| panic!("{payload}: {error:?}"));
        assert_eq!(rendered.rgba, identity.rgba, "{payload} changed a byte");
        assert!(
            std::sync::Arc::ptr_eq(&rendered.rgba, &source.rgba),
            "{payload} did not share the source allocation"
        );
    }
    // A non-neutral amount does materialize a frame, which is what makes the sharing above a
    // real property rather than a render that never happened.
    let darkened = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![vignette_layer(json!({"amount": -60.0}))]),
    )
    .expect("a darkened render");
    assert!(!std::sync::Arc::ptr_eq(&darkened.rgba, &source.rgba));
    assert_ne!(darkened.rgba, identity.rgba);
}

// -------------------------------------------------------------------------------------------
// Sample / render agreement through a non-zero-angle crop resample.
// -------------------------------------------------------------------------------------------

/// `render.sample` equals the rendered raster at every pixel of a stack whose crop resamples
/// (non-zero angle, so the vignette's finish-stage layer runs after a `Resample` segment, not
/// only an `ExactGeometry` one) before the vignette layer.
#[test]
fn sample_equals_the_rendered_raster_through_a_vignette_after_a_resampling_crop() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (12u32, 10u32);
    let pixels: Vec<[u8; 3]> = (0..width * height)
        .map(|i| {
            [
                ((i * 17) % 256) as u8,
                ((i * 61) % 256) as u8,
                ((i * 199) % 256) as u8,
            ]
        })
        .collect();
    let source = source_of(width, height, &pixels);
    let stack = recipe(vec![
        Layer::crop(CropPayload {
            angle: 12.0,
            x: 0.2,
            y: 0.2,
            width: 0.5,
            height: 0.5,
        }),
        vignette_layer(
            json!({"amount": 45.0, "midpoint": 30.0, "roundness": -20.0, "feather": 70.0}),
        ),
    ]);
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a render");
    assert!(rendered.width > 0 && rendered.height > 0);
    for y in 0..rendered.height {
        for x in 0..rendered.width {
            let sampled = sample(&registry, &source, &stack, x, y)
                .unwrap_or_else(|error| panic!("({x},{y}): {error:?}"))
                .rgba
                .expect("an in-bounds sample");
            assert_eq!(
                sampled,
                rendered.pixel(x, y).expect("an in-bounds pixel"),
                "({x},{y}) disagreed"
            );
        }
    }
}

// -------------------------------------------------------------------------------------------
// The RAW linear path: render_linear and sample_linear agree through a vignette after a crop.
// -------------------------------------------------------------------------------------------

fn linear_source(width: u32, height: u32, pixels: &[[f32; 3]]) -> LinearImage {
    assert_eq!(pixels.len() as u64, u64::from(width) * u64::from(height));
    let mut planes = vec![0.0f32; pixels.len() * 3];
    let (r, rest) = planes.split_at_mut(pixels.len());
    let (g, b) = rest.split_at_mut(pixels.len());
    for (i, pixel) in pixels.iter().enumerate() {
        r[i] = pixel[0];
        g[i] = pixel[1];
        b[i] = pixel[2];
    }
    LinearImage::with_fingerprint(width, height, planes, "sha256:vignette-linear-fixture")
        .expect("a valid linear source")
}

/// The linear (RAW) path hands the vignette's positional unit the same output-stage coordinates
/// the 8-bit path does: `sample_linear` equals `render_linear` pixel for pixel through a crop
/// resample followed by a vignette layer.
#[test]
fn render_linear_and_sample_linear_agree_through_a_vignette_after_a_crop() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (8u32, 6u32);
    let pixels: Vec<[f32; 3]> = (0..width * height)
        .map(|i| {
            let t = i as f32 / (width * height) as f32;
            [t, 0.4, 1.0 - t]
        })
        .collect();
    let source = linear_source(width, height, &pixels);
    let stack = recipe(vec![
        Layer::crop(CropPayload {
            angle: 0.0,
            x: 0.1,
            y: 0.1,
            width: 0.6,
            height: 0.6,
        }),
        vignette_layer(
            json!({"amount": -40.0, "midpoint": 45.0, "roundness": 10.0, "feather": 55.0}),
        ),
    ]);
    let raster = render_linear(
        &registry,
        &source,
        SnapshotId::new(),
        &stack,
        LinearSettings::default(),
    )
    .expect("a linear render");
    assert!(raster.width > 0 && raster.height > 0);
    for y in 0..raster.height {
        for x in 0..raster.width {
            let sampled =
                sample_linear(&registry, &source, &stack, LinearSettings::default(), x, y)
                    .unwrap_or_else(|error| panic!("({x},{y}): {error:?}"))
                    .rgba
                    .expect("an in-bounds sample");
            assert_eq!(
                sampled,
                raster.pixel(x, y).expect("an in-bounds pixel"),
                "({x},{y}) disagreed"
            );
        }
    }
}
