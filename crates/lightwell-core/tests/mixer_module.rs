//! The colour mixer module (`lightwell.mixer`) end to end, following `basic_colour.rs`'s
//! patterns: real layers, real rendering and sampling, the frozen fixtures through the real
//! render path, history/label behaviour, the two-layer refusal, ordering against Basic and the
//! RAW linear path.
//!
//! Numerical rule, from `docs/design/mixer-study.md`'s frozen tolerance: a rendered code must
//! equal the f64 reference's code exactly, except where the reference's linear value sits within
//! `1e-5 + 1e-5 * |threshold|` of the exact linear threshold between two codes, where one code of
//! difference is permitted. Identity stacks and byte sharing are exact with no tolerance at all.

mod reference;

use lightwell_core::{
    ApiRequest, BASIC_EFFECT, ClientSession, EFFECT_FORMAT, EditorService, ErrorKind, Layer,
    LayerId, LinearImage, LinearSettings, MIXER_EFFECT, ModuleRegistry, Mutation, OwnerHandle,
    RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, render, render_linear, sample, sample_linear,
};
use reference::mixer::RANGE_NAMES;
use serde_json::{Map, Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

// -------------------------------------------------------------------------------------------
// Shared helpers, matching `basic_colour.rs`'s patterns.
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
        actor: "mixer-module-test".into(),
    }
}

fn catalog(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-mixer-module-{name}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_file(&path);
    path
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
// Descriptor and API discovery
// -------------------------------------------------------------------------------------------

/// `module.list` and `schema.list` describe the mixer module exactly as the design specifies:
/// collapsed, order 10, three groups with rails, 24 parameters and both generated API methods.
#[test]
fn module_list_and_schema_list_describe_the_mixer_module() {
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
    let mixer = modules["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == json!("lightwell.mixer"))
        .expect("the mixer module is registered")
        .clone();
    assert_eq!(mixer["title"], json!("Colour mixer"));
    assert_eq!(
        mixer["hint"],
        json!("Hue, saturation and luminance by range")
    );
    assert_eq!(mixer["collapsed"], json!(true));
    assert_eq!(mixer["developer"], json!(false));
    assert_eq!(
        mixer["effects"],
        json!([{
            "id": "lightwell.mixer.hsl",
            "format": 1,
            "stage": "color",
            "order": 10,
        }])
    );
    let groups = mixer["controls"].as_array().unwrap();
    assert_eq!(groups.len(), 3);
    assert_eq!(
        groups
            .iter()
            .map(|group| group["label"].clone())
            .collect::<Vec<_>>(),
        vec![json!("Hue"), json!("Saturation"), json!("Luminance")]
    );
    assert_eq!(
        groups
            .iter()
            .map(|group| group["collapsed"].as_bool().unwrap_or(false))
            .collect::<Vec<_>>(),
        vec![false, true, true],
        "Hue starts expanded, Saturation and Luminance collapsed"
    );
    for group in groups {
        let sliders = group["controls"].as_array().unwrap();
        assert_eq!(sliders.len(), 8);
        for slider in sliders {
            assert_eq!(slider["kind"], json!("number"));
            let stops = slider["rail"]["gradient"]["stops"]
                .as_array()
                .unwrap_or_else(|| panic!("{slider} declares no gradient rail"));
            assert!((2..=8).contains(&stops.len()));
        }
    }
    let actions = mixer["actions"].as_array().unwrap();
    assert!(
        actions
            .iter()
            .any(|action| action["id"] == json!("set-mixer"))
    );
    assert!(
        actions
            .iter()
            .any(|action| action["id"] == json!("reset-mixer"))
    );
    let set = actions
        .iter()
        .find(|action| action["id"] == json!("set-mixer"))
        .unwrap();
    assert_eq!(set["parameters"].as_array().unwrap().len(), 24);

    let schema = call("schema.list", json!({}));
    assert!(schema["methods"]["edit.set-mixer"].is_object());
    assert!(schema["methods"]["edit.reset-mixer"].is_object());
    assert_eq!(
        schema["methods"]["edit.reset-mixer"]["parameters"]
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
/// label a client sees for one field, a group reset, the module reset and a multi-field patch.
#[test]
fn plan_labels_and_values_through_the_editor_service() {
    let path = catalog("plan-labels");
    let mut service = EditorService::open(&path).expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;

    let one_field = service
        .apply_action(
            &asset,
            mutation(0, "one"),
            "set-mixer",
            json!({"red-hue": 20.0}),
        )
        .expect("a single-field set");
    assert_eq!(
        service
            .entry(
                &asset,
                &one_field.created_entry_id.clone().expect("an entry")
            )
            .expect("the entry")
            .label,
        "Red hue +20"
    );

    let second_field = service
        .apply_action(
            &asset,
            mutation(1, "two"),
            "set-mixer",
            json!({"aqua-luminance": -15.0}),
        )
        .expect("a second single-field set updates the same layer");
    assert_eq!(
        service
            .entry(
                &asset,
                &second_field.created_entry_id.clone().expect("an entry")
            )
            .expect("the entry")
            .label,
        "Aqua luminance -15"
    );

    let described = service.describe_entry(&asset, None).expect("a description");
    let row = described
        .layers
        .iter()
        .find(|layer| layer.effect == MIXER_EFFECT)
        .expect("the mixer row");
    assert_eq!(row.values["red-hue"], json!(20.0));
    assert_eq!(row.values["aqua-luminance"], json!(-15.0));
    assert_eq!(row.values.len(), 24);

    // Returning the whole Hue group to neutral labels itself as a group reset, however it is
    // sent.
    let hue_neutral: Map<String, Value> = fields()
        .iter()
        .filter(|name| name.ends_with("-hue"))
        .map(|name| (name.clone(), json!(0.0)))
        .collect();
    let hue_reset = service
        .apply_action(
            &asset,
            mutation(2, "hue-reset"),
            "set-mixer",
            Value::Object(hue_neutral),
        )
        .expect("the Hue group reset");
    assert_eq!(
        service
            .entry(&asset, &hue_reset.created_entry_id.expect("an entry"))
            .expect("the entry")
            .label,
        "Reset Hue"
    );

    // A multi-field patch that is not a declared group reset names the field count.
    let mixed = service
        .apply_action(
            &asset,
            mutation(3, "mixed"),
            "set-mixer",
            json!({"green-saturation": 10.0, "purple-luminance": -5.0}),
        )
        .expect("a mixed patch");
    assert_eq!(
        service
            .entry(&asset, &mixed.created_entry_id.expect("an entry"))
            .expect("the entry")
            .label,
        "Colour mixer (2 fields)"
    );

    // `reset-mixer` labels itself and is a no-op on an already-neutral layer.
    let module_reset = service
        .apply_action(
            &asset,
            mutation(4, "module-reset"),
            "reset-mixer",
            json!({}),
        )
        .expect("the module reset");
    assert_eq!(
        service
            .entry(&asset, &module_reset.created_entry_id.expect("an entry"))
            .expect("the entry")
            .label,
        "Reset Colour mixer"
    );
    let no_op = service
        .apply_action(
            &asset,
            mutation(5, "already-neutral"),
            "reset-mixer",
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
// Two layers, ordering
// -------------------------------------------------------------------------------------------

/// Planning or rendering against a stack holding two Colour mixer layers fails explicitly and
/// rewrites nothing.
#[test]
fn two_mixer_layers_are_refused_without_being_rewritten() {
    let registry = ModuleRegistry::builtin();
    let stack = recipe(vec![
        mixer_layer(json!({"red-hue": 10.0})),
        mixer_layer(json!({"aqua-saturation": 20.0})),
    ]);
    // `validate_recipe` checks each payload structurally but not single-layer ambiguity, which is
    // a whole-stack property; compiling the stack (through `render` or `sample`) is where the
    // host refuses two layers of a `single_layer` effect.
    let source = source_of(1, 1, &[[10, 20, 30]]);
    let render_error =
        render(&registry, &source, SnapshotId::new(), &stack).expect_err("rendering refuses two");
    assert_eq!(render_error.kind, ErrorKind::Validation);
    assert_eq!(render_error.detail, "ambiguous Colour mixer layers");

    let sample_error = sample(&registry, &source, &stack, 0, 0).expect_err("sampling refuses too");
    assert_eq!(sample_error.kind, ErrorKind::Validation);
    assert_eq!(sample_error.detail, "ambiguous Colour mixer layers");
    assert_eq!(
        stack.layers.len(),
        2,
        "the refused stack is kept as it stands"
    );
}

/// A mixer layer always follows the Basic layer in the colour run, whichever action was applied
/// first, matching `ModuleRegistry::insertion_index_for`'s declared order.
#[test]
fn a_mixer_layer_always_follows_basic_whichever_was_touched_first() {
    for (case, first, second) in [
        ("Basic then mixer", "set-basic", "set-mixer"),
        ("mixer then Basic", "set-mixer", "set-basic"),
    ] {
        let path = catalog(&format!("order-{first}-{second}"));
        let mut service = EditorService::open(&path).expect("a catalog");
        let asset = service.import(&jpeg()).expect("an import").asset.id;
        let payload = |action: &str| -> Value {
            if action == "set-basic" {
                json!({"exposure": 0.3})
            } else {
                json!({"red-hue": 10.0})
            }
        };
        service
            .apply_action(&asset, mutation(0, "first"), first, payload(first))
            .expect("the first edit");
        service
            .apply_action(&asset, mutation(1, "second"), second, payload(second))
            .expect("the second edit");
        let described = service.describe_entry(&asset, None).expect("a description");
        let effects: Vec<&str> = described
            .layers
            .iter()
            .map(|layer| layer.effect.as_str())
            .collect();
        assert_eq!(effects, vec![BASIC_EFFECT, MIXER_EFFECT], "{case}");
        drop(service);
        fs::remove_file(path).expect("the catalog is removed");
    }
}

// -------------------------------------------------------------------------------------------
// Real-buffer proofs: neutral sharing, grey invariance, fixtures, the linear path
// -------------------------------------------------------------------------------------------

/// A neutral mixer payload (`{}` or every field explicitly zero) keeps the identity byte path and
/// shares the source allocation, exactly as Basic's neutral layer does.
#[test]
fn a_neutral_mixer_layer_shares_the_source_buffer() {
    let registry = ModuleRegistry::builtin();
    let pixels: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, 255 - code, 128]).collect();
    let source = source_of(16, 16, &pixels);
    let identity = render(&registry, &source, SnapshotId::new(), &recipe(Vec::new()))
        .expect("the identity render");
    let explicit_neutral: Map<String, Value> = fields()
        .iter()
        .map(|name| (name.clone(), json!(0.0)))
        .collect();
    for payload in [json!({}), Value::Object(explicit_neutral)] {
        let rendered = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![mixer_layer(payload.clone())]),
        )
        .expect("a neutral render");
        assert_eq!(rendered.rgba, identity.rgba, "{payload} changed a byte");
        assert!(
            Arc::ptr_eq(&rendered.rgba, &source.rgba),
            "{payload} did not share the source allocation"
        );
    }
    let coloured = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![mixer_layer(json!({"red-hue": 20.0}))]),
    )
    .expect("a coloured render");
    assert!(!Arc::ptr_eq(&coloured.rgba, &source.rgba));
}

/// Every grey code on a rendered ramp is byte-invariant under every one of the 24 sliders at
/// `±100`, through the real render path and quantizer.
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
                assert!(
                    pixel[0] == pixel[1] && pixel[1] == pixel[2],
                    "{field} at {value}: grey {x} drifted to {pixel:?}"
                );
                assert_eq!(pixel[3], 255, "alpha is never touched");
            }
        }
    }
}

/// Production versus every one of the 414 frozen fixture cases, through the real render path
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
    assert_eq!(cases.len(), 414, "every frozen case is checked");

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
