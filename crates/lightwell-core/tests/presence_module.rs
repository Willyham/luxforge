//! The Presence module (`lightwell.presence`) end to end, following `mixer_module.rs`'s patterns:
//! real layers, real rendering and sampling, the independent `f64` reference through the real
//! render path, history and label behaviour, the two-layer refusal, placement against Basic, the
//! mixer and the geometry tail, and the RAW linear path.
//!
//! Numerical rule, from `docs/design/presence-study.md`'s frozen tolerance: production matches the
//! reference within `2e-4 + 2e-4 * |reference|` in linear light and at most one output code. This
//! file sees the quantized frame, so it asserts the code: an exact code everywhere except where the
//! reference's linear value sits within that tolerance of the threshold between two codes. The
//! linear-light half of the tolerance is asserted against the committed oracle in the crate's own
//! `modules::presence::oracle`, which can read the units' `f32` output before the host quantizes it.
//! Identity stacks and byte sharing are exact with no tolerance at all.

mod reference;

use lightwell_core::{
    ApiRequest, BASIC_EFFECT, ClientSession, EFFECT_FORMAT, EditorService, ErrorKind, Layer,
    LayerId, LinearImage, LinearSettings, MIXER_EFFECT, ModuleRegistry, Mutation, OwnerHandle,
    PRESENCE_EFFECT, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, render, render_linear, sample,
    sample_linear,
};
use reference::presence::{PresenceParams, Rgb, apply_presence};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

// -------------------------------------------------------------------------------------------
// Shared helpers.
// -------------------------------------------------------------------------------------------

static NEXT: AtomicU64 = AtomicU64::new(1);

fn fixture(name: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn jpeg() -> PathBuf {
    fixture("s0/orientation-1.jpg")
}

fn catalog(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-presence-module-{name}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_file(&path);
    path
}

fn presence_layer(payload: Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: PRESENCE_EFFECT.into(),
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

fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "presence-module-test".into(),
    }
}

/// The frozen production tolerance in linear light.
fn tolerance(reference: f64) -> f64 {
    2e-4 + 2e-4 * reference.abs()
}

/// The declared tolerance as the quantized frame shows it: an exact code, unless the reference's
/// linear value sits within the frozen tolerance of the threshold between the two codes, where one
/// code of difference is permitted.
fn assert_presence_code(actual: u8, expected: u8, linear: f64, case: &str) {
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
    assert!(
        (linear.clamp(0.0, 1.0) - threshold).abs() <= tolerance(threshold),
        "{case}: rendered {actual} against {expected}, but the reference value {linear} is not \
         within {} of the code threshold {threshold}",
        tolerance(threshold)
    );
}

// -------------------------------------------------------------------------------------------
// Deterministic images, in the fixture specs' own formulas.
// -------------------------------------------------------------------------------------------

struct Lcg(u64);

impl Lcg {
    fn next_unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

const HAZE_A: [f64; 3] = [0.85, 0.88, 0.95];
const HAZE_PALETTE: [[f64; 3]; 6] = [
    [0.80, 0.35, 0.20],
    [0.20, 0.70, 0.30],
    [0.15, 0.25, 0.85],
    [0.70, 0.70, 0.20],
    [0.45, 0.45, 0.45],
    [0.10, 0.55, 0.60],
];

/// The fixtures' multi-scale textured patch with a per-channel tint.
fn patch(width: i64, height: i64) -> Vec<[f64; 3]> {
    let mut rng = Lcg(4_242);
    let tint = [1.0, 0.9, 0.75];
    let mut pixels = vec![[0.0; 3]; (width * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f64, y as f64);
            let value = 0.3
                + 0.05
                    * (std::f64::consts::TAU * fx / 5.0).sin()
                    * (std::f64::consts::TAU * fy / 7.0).sin()
                + 0.04 * (std::f64::consts::TAU * fx / 19.0).sin()
                + 0.03 * (std::f64::consts::TAU * fy / 31.0).cos()
                + 0.01 * (rng.next_unit() * 2.0 - 1.0);
            pixels[(y * width + x) as usize] = std::array::from_fn(|channel| value * tint[channel]);
        }
    }
    pixels
}

/// The fixtures' synthetic scene built through the forward haze model, with black cells so the
/// dark-channel prior holds and pure-haze rows at the top for the atmospheric-light estimator.
fn haze(width: i64, height: i64) -> Vec<[f64; 3]> {
    let (t_min, t_max, sky_rows, cell) = (0.35, 0.8, 4, 4);
    let mut pixels = vec![[0.0; 3]; (width * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let clear = if y < sky_rows {
                HAZE_A
            } else {
                let (bx, by) = (x / cell, y / cell);
                if bx % 2 == 0 && by % 2 == 0 {
                    [0.0; 3]
                } else {
                    HAZE_PALETTE[(((bx / 2) + 3 * (by / 2)) % 6) as usize]
                }
            };
            let fraction = x as f64 / (width - 1) as f64;
            let t = t_min + (t_max - t_min) * fraction;
            pixels[(y * width + x) as usize] =
                std::array::from_fn(|channel| t * clear[channel] + (1.0 - t) * HAZE_A[channel]);
        }
    }
    pixels
}

/// A grey ramp, for the achromatic proof.
fn greys(width: i64, height: i64) -> Vec<[f64; 3]> {
    let mut pixels = vec![[0.0; 3]; (width * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let value = reference::srgb_to_linear(((x * 7 + y * 3) % 256) as u8);
            pixels[(y * width + x) as usize] = [value; 3];
        }
    }
    pixels
}

/// One 8-bit source from linear pixels, and the linear values it actually decodes to, which is what
/// the reference must be evaluated on for the byte path.
fn byte_source(width: i64, height: i64, pixels: &[[f64; 3]]) -> (SourceImage, Vec<[f64; 3]>) {
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    let mut decoded = Vec::with_capacity(pixels.len());
    for pixel in pixels {
        let codes = pixel.map(reference::linear_to_srgb_code);
        rgba.extend_from_slice(&codes);
        rgba.push(255);
        decoded.push(codes.map(reference::srgb_to_linear));
    }
    // The estimate cache is keyed by immutable source identity. Distinct synthetic pictures must
    // not borrow each other's atmosphere just because their dimensions and recipe match.
    let fingerprint = format!("sha256:{:x}", Sha256::digest(&rgba));
    (
        SourceImage {
            width: width as u32,
            height: height as u32,
            rgba: rgba.into(),
            fingerprint,
            orientation: 1,
        },
        decoded,
    )
}

/// One planar linear source, and the `f32`-rounded values the reference must be evaluated on.
fn linear_source(width: i64, height: i64, pixels: &[[f64; 3]]) -> (LinearImage, Vec<[f64; 3]>) {
    let mut planes = Vec::with_capacity(pixels.len() * 3);
    for channel in 0..3 {
        planes.extend(pixels.iter().map(|pixel| pixel[channel] as f32));
    }
    let rounded = pixels
        .iter()
        .map(|pixel| pixel.map(|value| f64::from(value as f32)))
        .collect();
    let mut fingerprint = Sha256::new();
    for value in &planes {
        fingerprint.update(value.to_bits().to_le_bytes());
    }
    (
        LinearImage::with_fingerprint(
            width as u32,
            height as u32,
            planes,
            format!("sha256:{:x}", fingerprint.finalize()),
        )
        .unwrap(),
        rounded,
    )
}

fn expected_frame(width: i64, height: i64, pixels: &[[f64; 3]], params: PresenceParams) -> Rgb {
    apply_presence(
        &Rgb::from_pixels(width, height, pixels),
        params,
        width.max(height),
    )
}

/// The four parameter sets every numerical test below runs: each unit alone at an endpoint and all
/// three together at intermediate amounts.
fn parameter_sets() -> Vec<(&'static str, Value, PresenceParams)> {
    vec![
        (
            "texture +100",
            json!({"texture": 100.0}),
            PresenceParams {
                texture: 100.0,
                clarity: 0.0,
                dehaze: 0.0,
            },
        ),
        (
            "clarity -60",
            json!({"clarity": -60.0}),
            PresenceParams {
                texture: 0.0,
                clarity: -60.0,
                dehaze: 0.0,
            },
        ),
        (
            "dehaze +100",
            json!({"dehaze": 100.0}),
            PresenceParams {
                texture: 0.0,
                clarity: 0.0,
                dehaze: 100.0,
            },
        ),
        (
            "all three",
            json!({"texture": 40.0, "clarity": -20.0, "dehaze": 15.0}),
            PresenceParams {
                texture: 40.0,
                clarity: -20.0,
                dehaze: 15.0,
            },
        ),
    ]
}

// -------------------------------------------------------------------------------------------
// Descriptor and API discovery
// -------------------------------------------------------------------------------------------

/// `module.list` and `schema.list` describe the Presence module exactly as the design specifies:
/// collapsed, one spatial effect, one expanded group of three sliders, three parameters and both
/// generated API methods.
#[test]
fn module_list_and_schema_list_describe_the_presence_module() {
    let dir = catalog("api-descriptor");
    fs::create_dir_all(dir.parent().unwrap()).ok();
    let (owner, join) = OwnerHandle::start(&dir).unwrap();
    let client = owner.register();
    let call = |method: &str, params: Value| -> Value {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap();
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.unwrap()
    };

    let modules = call("module.list", json!({}));
    let listed = modules["modules"].as_array().unwrap().clone();
    let presence = listed
        .iter()
        .find(|module| module["id"] == json!("lightwell.presence"))
        .expect("the presence module is registered")
        .clone();
    assert_eq!(presence["title"], json!("Presence"));
    assert_eq!(presence["hint"], json!("Texture, clarity and dehaze"));
    assert_eq!(presence["collapsed"], json!(true));
    assert_eq!(presence["developer"], json!(false));
    assert_eq!(
        presence["effects"],
        json!([{
            "id": "lightwell.presence.adjust",
            "format": 1,
            "stage": "spatial",
            "order": 0,
            // Presence is one of the three effects a mask may be attached to, so its descriptor says
            // so and `edit.set-presence` carries the host's optional `mask` field. The masked
            // *spatial* primitive is a later step, so a committed masked Presence layer is refused
            // by name until it lands rather than rendered as if it applied everywhere.
            "maskable": true,
        }])
    );
    assert_eq!(
        presence["reset"],
        json!({"action": "reset-presence", "preset": {}})
    );
    // Registered after Basic and before the mixer, which is the panel order too.
    let positions: Vec<&str> = listed
        .iter()
        .map(|module| module["id"].as_str().unwrap())
        .collect();
    let index = |id: &str| positions.iter().position(|listed| *listed == id).unwrap();
    assert!(index("lightwell.basic") < index("lightwell.presence"));
    assert!(index("lightwell.presence") < index("lightwell.mixer"));

    let groups = presence["controls"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["label"], json!("Presence"));
    assert!(
        !groups[0]["collapsed"].as_bool().unwrap_or(false),
        "the one group starts expanded"
    );
    let sliders = groups[0]["controls"].as_array().unwrap();
    assert_eq!(
        sliders
            .iter()
            .map(|slider| slider["label"].clone())
            .collect::<Vec<_>>(),
        vec![json!("Texture"), json!("Clarity"), json!("Dehaze")]
    );
    for slider in sliders {
        assert_eq!(slider["kind"], json!("number"));
        assert!(slider["rail"].is_null(), "{slider} declares a rail");
    }
    let reset = &groups[0]["reset"];
    assert_eq!(reset["action"], json!("set-presence"));
    assert_eq!(
        reset["preset"],
        json!({"texture": 0.0, "clarity": 0.0, "dehaze": 0.0})
    );

    let actions = presence["actions"].as_array().unwrap();
    let set = actions
        .iter()
        .find(|action| action["id"] == json!("set-presence"))
        .expect("set-presence");
    assert_eq!(
        set["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|parameter| parameter["name"].clone())
            .collect::<Vec<_>>(),
        vec![json!("texture"), json!("clarity"), json!("dehaze")]
    );
    for parameter in set["parameters"].as_array().unwrap() {
        assert_eq!(parameter["kind"], json!("number"));
        assert_eq!(parameter["min"], json!(-100.0));
        assert_eq!(parameter["max"], json!(100.0));
        assert_eq!(parameter["default"], json!(0.0));
        assert_eq!(parameter["step"], json!(1.0));
        assert_eq!(parameter["precision"], json!(0));
        assert_eq!(parameter["zero"], json!(0.0));
    }
    assert!(
        actions
            .iter()
            .any(|action| action["id"] == json!("reset-presence"))
    );

    let schema = call("schema.list", json!({}));
    assert!(schema["methods"]["edit.set-presence"].is_object());
    assert!(schema["methods"]["edit.reset-presence"].is_object());
    assert_eq!(
        schema["methods"]["edit.set-presence"]["parameters"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        schema["methods"]["edit.reset-presence"]["parameters"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    owner.stop();
    drop(join);
    let _ = fs::remove_dir_all(&dir);
    let _ = ClientSession::default();
}

// -------------------------------------------------------------------------------------------
// Plan semantics, labels and values
// -------------------------------------------------------------------------------------------

/// Commit, update-in-place, no-op and reset semantics through the real `EditorService`, and the
/// label a client sees for one field, a multi-field patch and the module reset.
#[test]
fn plan_labels_and_values_through_the_editor_service() {
    let path = catalog("plan-labels");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    let label_of = |service: &EditorService, asset: &_, entry: Option<lightwell_core::EntryId>| {
        service
            .entry(asset, &entry.expect("an entry"))
            .expect("the entry")
            .label
            .clone()
    };

    let first = service
        .apply_action(
            &asset,
            mutation(0, "one"),
            "set-presence",
            json!({"texture": 40.0}),
        )
        .expect("a single-field set");
    assert_eq!(
        label_of(&service, &asset, first.created_entry_id.clone()),
        "Texture +40"
    );

    let second = service
        .apply_action(
            &asset,
            mutation(1, "two"),
            "set-presence",
            json!({"clarity": -20.0}),
        )
        .expect("a second single-field set updates the same layer");
    assert_eq!(
        label_of(&service, &asset, second.created_entry_id.clone()),
        "Clarity -20"
    );

    let third = service
        .apply_action(
            &asset,
            mutation(2, "three"),
            "set-presence",
            json!({"dehaze": 15.0}),
        )
        .expect("a third single-field set");
    assert_eq!(
        label_of(&service, &asset, third.created_entry_id.clone()),
        "Dehaze +15"
    );

    let described = service.describe_entry(&asset, None).expect("a description");
    let row = described
        .layers
        .iter()
        .find(|layer| layer.effect == PRESENCE_EFFECT)
        .expect("the presence row");
    assert_eq!(row.values["texture"], json!(40.0));
    assert_eq!(row.values["clarity"], json!(-20.0));
    assert_eq!(row.values["dehaze"], json!(15.0));
    assert_eq!(row.values.len(), 3);
    assert_eq!(row.summary, "Texture +40, Clarity -20, Dehaze +15");

    // The group's own reset is a three-field patch, which labels itself by its field count.
    let group_reset = service
        .apply_action(
            &asset,
            mutation(3, "group-reset"),
            "set-presence",
            json!({"texture": 0.0, "clarity": 0.0, "dehaze": 0.0}),
        )
        .expect("the group reset");
    assert_eq!(
        label_of(&service, &asset, group_reset.created_entry_id.clone()),
        "Reset Presence"
    );

    // `reset-presence` labels itself, keeps the layer and is a no-op on an already-neutral one.
    service
        .apply_action(
            &asset,
            mutation(4, "again"),
            "set-presence",
            json!({"texture": -30.0}),
        )
        .expect("an edit to reset");
    let module_reset = service
        .apply_action(
            &asset,
            mutation(5, "module-reset"),
            "reset-presence",
            json!({}),
        )
        .expect("the module reset");
    assert_eq!(
        label_of(&service, &asset, module_reset.created_entry_id.clone()),
        "Reset Presence"
    );
    let described = service.describe_entry(&asset, None).expect("a description");
    assert!(
        described
            .layers
            .iter()
            .any(|layer| layer.effect == PRESENCE_EFFECT),
        "a reset keeps the layer"
    );
    let no_op = service
        .apply_action(
            &asset,
            mutation(6, "already-neutral"),
            "reset-presence",
            json!({}),
        )
        .expect("resetting an already-neutral layer");
    assert!(
        no_op.created_entry_id.is_none(),
        "a no-op reset creates no history entry"
    );

    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

// -------------------------------------------------------------------------------------------
// Two layers, placement
// -------------------------------------------------------------------------------------------

/// Planning or rendering against a stack holding two Presence layers fails explicitly and rewrites
/// nothing.
#[test]
fn two_presence_layers_are_refused_without_being_rewritten() {
    let registry = ModuleRegistry::builtin();
    let stack = recipe(vec![
        presence_layer(json!({"texture": 10.0})),
        presence_layer(json!({"clarity": 20.0})),
    ]);
    let (source, _) = byte_source(4, 4, &patch(4, 4));
    let render_error =
        render(&registry, &source, SnapshotId::new(), &stack).expect_err("rendering refuses two");
    assert_eq!(render_error.kind, ErrorKind::Validation);
    assert_eq!(render_error.detail, "ambiguous Presence layers");

    let sample_error = sample(&registry, &source, &stack, 0, 0).expect_err("sampling refuses too");
    assert_eq!(sample_error.kind, ErrorKind::Validation);
    assert_eq!(sample_error.detail, "ambiguous Presence layers");
    assert_eq!(
        stack.layers.len(),
        2,
        "the refused stack is kept as it stands"
    );
}

/// The spatial layer reads what the pointwise colour run produced, so the stack is always Basic,
/// mixer, presence whichever action was applied first, and a geometry edit lands after all three.
#[test]
fn a_presence_layer_always_follows_the_colour_run_whichever_was_touched_first() {
    let payload = |action: &str| -> Value {
        match action {
            "set-basic" => json!({"exposure": 0.3}),
            "set-mixer" => json!({"red-hue": 10.0}),
            _ => json!({"texture": 25.0}),
        }
    };
    for order in [
        ["set-basic", "set-mixer", "set-presence"],
        ["set-presence", "set-basic", "set-mixer"],
        ["set-mixer", "set-presence", "set-basic"],
        ["set-presence", "set-mixer", "set-basic"],
    ] {
        let path = catalog(&format!("order-{}", order.join("-")));
        let mut service = EditorService::open(&path).expect("a catalog");
        let asset = service.import(&jpeg()).expect("an import").asset.id;
        for (revision, action) in order.iter().enumerate() {
            service
                .apply_action(
                    &asset,
                    mutation(revision as u64, &format!("edit-{revision}")),
                    action,
                    payload(action),
                )
                .unwrap_or_else(|error| panic!("{order:?}: {action}: {error}"));
        }
        // A geometry edit after all three joins the tail, after the spatial layer.
        service
            .apply_action(
                &asset,
                mutation(3, "turn"),
                "transform",
                json!({"transform": "rotate-right"}),
            )
            .expect("a geometry edit");
        let described = service.describe_entry(&asset, None).expect("a description");
        let effects: Vec<&str> = described
            .layers
            .iter()
            .map(|layer| layer.effect.as_str())
            .collect();
        assert_eq!(
            effects,
            vec![
                BASIC_EFFECT,
                MIXER_EFFECT,
                PRESENCE_EFFECT,
                "lightwell.geometry.orientation"
            ],
            "{order:?}"
        );
        drop(service);
        fs::remove_file(path).expect("the catalog is removed");
    }
}

// -------------------------------------------------------------------------------------------
// Real-buffer proofs
// -------------------------------------------------------------------------------------------

/// A neutral Presence payload (`{}` or every field explicitly zero) opens no stage boundary: the
/// render keeps the identity byte path and shares the source allocation.
#[test]
fn a_neutral_presence_layer_shares_the_source_buffer() {
    let registry = ModuleRegistry::builtin();
    let (source, _) = byte_source(32, 24, &patch(32, 24));
    let identity = render(&registry, &source, SnapshotId::new(), &recipe(Vec::new()))
        .expect("the identity render");
    for payload in [
        json!({}),
        json!({"texture": 0.0}),
        json!({"texture": 0.0, "clarity": 0.0, "dehaze": 0.0}),
    ] {
        let rendered = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![presence_layer(payload.clone())]),
        )
        .expect("a neutral render");
        assert_eq!(rendered.rgba, identity.rgba, "{payload} changed a byte");
        assert!(
            Arc::ptr_eq(&rendered.rgba, &source.rgba),
            "{payload} did not share the source allocation"
        );
    }
    let edited = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![presence_layer(json!({"texture": 20.0}))]),
    )
    .expect("an edited render");
    assert!(!Arc::ptr_eq(&edited.rgba, &source.rgba));
}

/// Production against the independent `f64` reference through the real render path, on the 8-bit
/// source path and on the RAW linear path, with `render.sample` equal to the rendered byte at every
/// pixel on both.
#[test]
fn production_matches_the_reference_through_both_render_paths() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (40_i64, 32_i64);
    let images: [(&str, Vec<[f64; 3]>); 2] = [
        ("patch", patch(width, height)),
        ("haze", haze(width, height)),
    ];
    let mut worst = 0_i32;
    let mut worst_case = String::new();

    for (image_name, pixels) in &images {
        let (byte_image, decoded) = byte_source(width, height, pixels);
        let (linear_image, rounded) = linear_source(width, height, pixels);
        for (set_name, payload, params) in parameter_sets() {
            let stack = recipe(vec![presence_layer(payload.clone())]);
            let case = format!("{image_name} {set_name}");

            let expected = expected_frame(width, height, &decoded, params);
            let rendered = render(&registry, &byte_image, SnapshotId::new(), &stack)
                .unwrap_or_else(|error| panic!("{case} (byte path): {error}"));
            for y in 0..height {
                for x in 0..width {
                    let pixel = rendered
                        .pixel(x as u32, y as u32)
                        .expect("a rendered pixel");
                    assert_eq!(pixel[3], 255, "{case}: alpha is never touched");
                    let reference = expected.get(x, y);
                    for channel in 0..3 {
                        let linear = reference[channel];
                        let code = reference::linear_to_srgb_code(linear);
                        assert_presence_code(
                            pixel[channel],
                            code,
                            linear,
                            &format!("{case} byte pixel ({x}, {y}) channel {channel}"),
                        );
                        let deviation = (i32::from(pixel[channel]) - i32::from(code)).abs();
                        if deviation > worst {
                            worst = deviation;
                            worst_case = format!("{case} byte ({x}, {y}) channel {channel}");
                        }
                    }
                    let sampled = sample(&registry, &byte_image, &stack, x as u32, y as u32)
                        .expect("a sample")
                        .rgba
                        .expect("an opaque pixel");
                    assert_eq!(
                        sampled, pixel,
                        "{case}: the sample at ({x}, {y}) disagreed with the rendered byte"
                    );
                }
            }

            let expected = expected_frame(width, height, &rounded, params);
            let rendered = render_linear(
                &registry,
                &linear_image,
                SnapshotId::new(),
                &stack,
                LinearSettings::default(),
            )
            .unwrap_or_else(|error| panic!("{case} (linear path): {error}"));
            for y in 0..height {
                for x in 0..width {
                    let pixel = rendered
                        .pixel(x as u32, y as u32)
                        .expect("a rendered pixel");
                    let reference = expected.get(x, y);
                    for channel in 0..3 {
                        let linear = reference[channel];
                        let code = reference::linear_to_srgb_code(linear);
                        assert_presence_code(
                            pixel[channel],
                            code,
                            linear,
                            &format!("{case} linear pixel ({x}, {y}) channel {channel}"),
                        );
                        let deviation = (i32::from(pixel[channel]) - i32::from(code)).abs();
                        if deviation > worst {
                            worst = deviation;
                            worst_case = format!("{case} linear ({x}, {y}) channel {channel}");
                        }
                    }
                    let sampled = sample_linear(
                        &registry,
                        &linear_image,
                        &stack,
                        LinearSettings::default(),
                        x as u32,
                        y as u32,
                    )
                    .expect("a linear sample")
                    .rgba
                    .expect("an opaque pixel");
                    assert_eq!(
                        sampled, pixel,
                        "{case}: the linear sample at ({x}, {y}) disagreed with the rendered byte"
                    );
                }
            }
        }
    }
    // Printed with --nocapture so the handoff can quote a measured figure.
    println!("maximum observed rendered-code deviation {worst} at {worst_case}");
}

/// Texture and Clarity reconstruct RGB by the luminance ratio, so an achromatic pixel stays
/// achromatic bit for bit through the real render path and quantizer.
#[test]
fn achromatic_pixels_stay_achromatic_under_texture_and_clarity() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (48_i64, 32_i64);
    let (source, _) = byte_source(width, height, &greys(width, height));
    for payload in [
        json!({"texture": 100.0}),
        json!({"texture": -100.0}),
        json!({"clarity": 100.0}),
        json!({"clarity": -100.0}),
        json!({"texture": 100.0, "clarity": 100.0}),
    ] {
        let rendered = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![presence_layer(payload.clone())]),
        )
        .unwrap_or_else(|error| panic!("{payload}: {error}"));
        for y in 0..height {
            for x in 0..width {
                let pixel = rendered
                    .pixel(x as u32, y as u32)
                    .expect("a rendered pixel");
                assert!(
                    pixel[0] == pixel[1] && pixel[1] == pixel[2],
                    "{payload}: grey ({x}, {y}) drifted to {pixel:?}"
                );
            }
        }
    }
}

/// A spatial layer is a stage boundary, so a crop after it resamples the finished frame and the
/// point sample still equals the rendered byte everywhere.
#[test]
fn a_crop_after_a_presence_layer_still_samples_the_rendered_byte() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (40_i64, 32_i64);
    let (source, _) = byte_source(width, height, &patch(width, height));
    let path = catalog("crop-after-presence");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    service
        .apply_action(
            &asset,
            mutation(0, "presence"),
            "set-presence",
            json!({"clarity": 30.0}),
        )
        .expect("a presence edit");
    service
        .apply_action(
            &asset,
            mutation(1, "crop"),
            "crop",
            json!({"x": 0.1, "y": 0.1, "width": 0.6, "height": 0.6}),
        )
        .expect("a crop after it");
    let described = service.describe_entry(&asset, None).expect("a description");
    let effects: Vec<&str> = described
        .layers
        .iter()
        .map(|layer| layer.effect.as_str())
        .collect();
    assert_eq!(effects, vec![PRESENCE_EFFECT, "lightwell.geometry.crop"]);
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");

    // The same stack against a small synthetic source, where every pixel is checked.
    let stack = recipe(vec![
        presence_layer(json!({"clarity": 30.0})),
        Layer {
            id: LayerId::new(),
            effect_id: "lightwell.geometry.crop".into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({
                "x": 0.1, "y": 0.1, "width": 0.6, "height": 0.6, "angle": 0.0,
            }),
            mask: None,
            artifacts: Vec::new(),
        },
    ]);
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a cropped render");
    assert!(rendered.width > 1 && rendered.height > 1);
    for y in 0..rendered.height {
        for x in 0..rendered.width {
            let sampled = sample(&registry, &source, &stack, x, y).expect("a sample");
            assert_eq!(
                sampled.rgba,
                rendered.pixel(x, y),
                "crop after presence: sample at ({x}, {y})"
            );
        }
    }
}
