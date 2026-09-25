//! The `lightwell.vignette` module end to end: real layers through the real host pipeline
//! (`ModuleRegistry`, `render`, `EditorService`).
//!
//! The descriptor, the amount-0 neutrality rule, history label words and `compile`'s
//! neutral/non-neutral split are exercised in-crate, next to the module, in
//! `crates/lightwell-core/src/modules/vignette/mod.rs` and `unit.rs`. What the vignette shares with
//! every field-patch module — the field-patch rules, neutral payloads sharing the source, one layer
//! per target, sample equal to render on both paths and through a resampling crop, drafts, history,
//! an unavailable provider and reopen — is proved once by the conformance suite (`field_patch`),
//! and the vignette recentring after a crop update through the JSON method table by the Presence,
//! mixer and vignette chapter of `editor-acceptance`. This module covers the rest of what is the
//! vignette's own: placement at the end of the stack through rotations, mirrors and a crop,
//! recentring on the stage a crop produces against the frozen reference, production against every
//! frozen oracle fixture through `render`, and mirror/flip symmetry on rendered bytes.

use lightwell_core::{
    AssetId, CropPayload, EditorService, Layer, ModuleRegistry, Mutation, Orientation, SnapshotId,
    Transform, VIGNETTE_EFFECT,
};
use lightwell_reference as reference;
use lightwell_testkit::fixtures::render;
use lightwell_testkit::fixtures::{self, recipe, source_of};
use serde_json::{Value, json};

/// A global vignette layer holding `payload`.
fn layer(payload: Value) -> Layer {
    fixtures::layer(VIGNETTE_EFFECT, payload)
}

fn mutation(revision: u64, request: &str) -> Mutation {
    fixtures::mutation(revision, request, "vignette-test")
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
// Placement: a committed vignette layer stays last through the whole host editing surface.
// -------------------------------------------------------------------------------------------

/// A first `set-vignette` commits at the end of the stack; a rotation, a mirror and a crop after
/// it each join the geometry tail before the vignette layer, so the vignette stays last and
/// therefore keeps recentring on whatever the tail produces.
#[test]
fn a_vignette_layer_stays_last_through_rotate_mirror_and_crop() {
    let path = fixtures::temp_catalog("vignette-placement");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service
        .import(&fixtures::jpeg())
        .expect("an import")
        .asset
        .id;

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
    let stack = recipe(vec![crop, layer(params)]);

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
    let raw = std::fs::read_to_string(fixtures::fixture("vignette/amount-cases.json"))
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
        let stack = recipe(vec![layer(json!({
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
    let raw = std::fs::read_to_string(fixtures::fixture("vignette/mask-cases.json"))
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
        let stack = recipe(vec![layer(json!({
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
        &recipe(vec![layer(params.clone())]),
    )
    .expect("a plain render");

    let after_mirror = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![
            Layer::orientation(Orientation::of(Transform::MirrorHorizontal)),
            layer(params.clone()),
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
            layer(params),
        ]),
    )
    .expect("a render after flip");
    assert_eq!(raster_pixels(&after_flip), flip_vertically(&plain));
}
