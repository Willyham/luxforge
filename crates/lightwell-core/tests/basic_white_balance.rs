//! Temperature, Tint and the neutral picker end to end: the compiled colour unit against the
//! independent f64 reference, the internal order inside the one Basic layer, and the picker as a
//! module query and a canvas interaction driven from an independent JSON client.
//!
//! Numerical rule, from the design's numerical contract: the design freezes no per-algorithm
//! tolerance for white balance, so the pointwise contract's default applies — production must match
//! the f64 reference within `1e-6 + 1e-6 · |reference|` in linear light, and a rendered code must
//! equal the reference's code except where the reference's linear value sits within that same band
//! of the exact threshold between two codes. Identity stacks, byte sharing, the picker's own
//! integer answers and history behaviour are exact with no tolerance at all.

mod reference;

use lightwell_core::{
    ApiRequest, BASIC_EFFECT, EFFECT_FORMAT, EditorService, Layer, LayerId, ModuleRegistry,
    Mutation, OwnerHandle, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, render, sample,
};
use reference::white_balance::{self, RejectReason};
use reference::{code_threshold, srgb_to_linear};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
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
        fingerprint: "sha256:basic-white-balance-fixture".into(),
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
        actor: "basic-white-balance-test".into(),
    }
}

fn temp(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-wb-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_file(&path);
    path
}

/// The declared tolerance: an exact code, unless the reference's linear value sits within
/// `1e-6 + 1e-6 · |threshold|` of the threshold between the two codes.
fn assert_code(actual: u8, expected: u8, linear: f64, case: &str) {
    if actual == expected {
        return;
    }
    let difference = i32::from(actual) - i32::from(expected);
    assert!(
        difference.abs() <= 1,
        "{case}: rendered {actual}, reference {expected}"
    );
    let crossed = actual.max(expected);
    assert!(crossed >= 1, "{case}: code 0 has no lower threshold");
    let threshold = code_threshold(crossed);
    let tolerance = 1e-6 + 1e-6 * threshold.abs();
    assert!(
        (linear.clamp(0.0, 1.0) - threshold).abs() <= tolerance,
        "{case}: rendered {actual} against {expected}, but the reference value {linear} is not \
         within {tolerance} of the code threshold {threshold}"
    );
}

/// The committed solver corpus, so the integration tests use the same frozen patches the unit does.
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct SolverCase {
    description: String,
    patch_u8: Vec<[u8; 3]>,
    expected_solution: Option<[i64; 2]>,
}

fn solver_cases() -> Vec<SolverCase> {
    #[derive(serde::Deserialize)]
    struct Cases {
        solver_cases: Vec<SolverCase>,
    }
    let raw = fs::read_to_string(fixture("basic/white-balance-cases.json"))
        .expect("the committed corpus");
    serde_json::from_str::<Cases>(&raw)
        .expect("white-balance cases")
        .solver_cases
}

/// One grid case's patch, as a uniform colour and as its 25 dithered samples.
fn grid_case(temperature: i64, tint: i64) -> SolverCase {
    solver_cases()
        .into_iter()
        .find(|case| {
            case.expected_solution == Some([temperature, tint]) && case.patch_u8.len() == 25
        })
        .unwrap_or_else(|| panic!("a grid case for ({temperature}, {tint})"))
}

// ---------------------------------------------------------------------------------------------
// The compiled unit through real layers
// ---------------------------------------------------------------------------------------------

/// Every parameter pair of the corpus, rendered through a real Basic layer at the exact input codes
/// the corpus names, against the independent f64 reference.
#[test]
fn every_transform_case_renders_through_a_real_basic_layer() {
    #[derive(serde::Deserialize)]
    struct TransformCase {
        input_rgb_u8: [u8; 3],
        temperature: f64,
        tint: f64,
        expected_linear_f64: [f64; 3],
    }
    #[derive(serde::Deserialize)]
    struct Cases {
        transform_cases: Vec<TransformCase>,
    }
    let raw = fs::read_to_string(fixture("basic/white-balance-cases.json"))
        .expect("the committed corpus");
    let cases = serde_json::from_str::<Cases>(&raw)
        .expect("cases")
        .transform_cases;
    assert_eq!(cases.len(), 45);
    let registry = ModuleRegistry::builtin();
    // One render per parameter pair, with every input colour of that pair as a pixel of one row.
    let mut pairs: std::collections::BTreeMap<(u64, u64), Vec<&TransformCase>> =
        std::collections::BTreeMap::new();
    for case in &cases {
        pairs
            .entry((case.temperature.to_bits(), case.tint.to_bits()))
            .or_default()
            .push(case);
    }
    assert_eq!(pairs.len(), 9, "nine parameter pairs");
    for group in pairs.values() {
        let (temperature, tint) = (group[0].temperature, group[0].tint);
        let inputs: Vec<[u8; 3]> = group.iter().map(|case| case.input_rgb_u8).collect();
        let source = source_of(inputs.len() as u32, 1, &inputs);
        let stack = recipe(vec![basic_layer(
            json!({"temperature": temperature, "tint": tint}),
        )]);
        let rendered =
            render(&registry, &source, SnapshotId::new(), &stack).expect("a rendered correction");
        for (index, case) in group.iter().enumerate() {
            let pixel = rendered.pixel(index as u32, 0).expect("a rendered pixel");
            assert_eq!(pixel[3], 255, "alpha is never touched");
            for (channel, linear) in case.expected_linear_f64.into_iter().enumerate() {
                let expected = reference::linear_to_code(linear);
                assert_code(
                    pixel[channel],
                    expected,
                    linear,
                    &format!(
                        "{:?} channel {channel} at ({temperature}, {tint})",
                        case.input_rgb_u8
                    ),
                );
            }
        }
    }
}

/// A sample and a rendered byte are the same evaluation, over every grey code and several
/// parameter pairs.
#[test]
fn a_sample_equals_the_rendered_byte_for_every_code() {
    let registry = ModuleRegistry::builtin();
    let codes: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, 255 - code, 128]).collect();
    let source = source_of(256, 1, &codes);
    for (temperature, tint) in [
        (-100.0, 0.0),
        (-30.0, 45.0),
        (0.0, -100.0),
        (20.0, -20.0),
        (100.0, 100.0),
    ] {
        let stack = recipe(vec![basic_layer(
            json!({"temperature": temperature, "tint": tint}),
        )]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack).expect("a frame");
        for x in 0u32..256 {
            let sampled = sample(&registry, &source, &stack, x, 0)
                .expect("a sample")
                .rgba
                .expect("an opaque pixel");
            assert_eq!(
                sampled,
                rendered.pixel(x, 0).expect("a rendered pixel"),
                "code {x} at ({temperature}, {tint})"
            );
        }
    }
}

/// `0/0` keeps the identity byte path: a Basic layer holding only neutral white balance renders the
/// source's own allocation, not a copy of it.
#[test]
fn a_neutral_white_balance_renders_the_source_buffer_itself() {
    let registry = ModuleRegistry::builtin();
    let codes: Vec<[u8; 3]> = (0u8..=255).map(|code| [code, 255 - code, 64]).collect();
    let source = source_of(16, 16, &codes);
    let identity =
        render(&registry, &source, SnapshotId::new(), &recipe(Vec::new())).expect("the identity");
    for payload in [
        json!({"temperature": 0.0, "tint": 0.0}),
        json!({"temperature": 0.0}),
        json!({"tint": -0.0}),
        json!({"temperature": -0.0, "tint": 0.0, "exposure": 0.0}),
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
            std::sync::Arc::ptr_eq(&rendered.rgba, &source.rgba),
            "{payload} did not share the source allocation"
        );
    }
    // One axis alone is enough to leave the identity path, which is what makes the sharing above a
    // real property rather than a render that never happened.
    for payload in [json!({"temperature": 1.0}), json!({"tint": -1.0})] {
        let rendered = render(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe(vec![basic_layer(payload.clone())]),
        )
        .expect("a corrected render");
        assert!(
            !std::sync::Arc::ptr_eq(&rendered.rgba, &source.rgba),
            "{payload}"
        );
        assert_ne!(rendered.rgba, identity.rgba, "{payload}");
    }
}

/// The internal order inside the one layer is white balance first, then exposure.
///
/// The order is proved by the compiled unit list, which is what the host evaluates. It cannot also
/// be proved numerically here: a scalar exposure gain commutes exactly with white balance's linear
/// map, and nothing clamps between units of one operation, so both orders produce the same linear
/// result by construction. The rendered bytes are still checked against the reference composed in
/// the declared order, so a unit that silently did something else would fail.
#[test]
fn white_balance_runs_before_exposure_inside_the_one_layer() {
    use lightwell_core::{Processing, Stage};
    let registry = ModuleRegistry::builtin();
    let (module, _) = registry.effect(BASIC_EFFECT).expect("the Basic module");
    let payload = json!({"exposure": 2.0, "temperature": 60.0, "tint": -25.0});
    let Processing::Color(operation) = module
        .compile(
            BASIC_EFFECT,
            EFFECT_FORMAT,
            &payload,
            Stage {
                width: 4,
                height: 1,
            },
        )
        .expect("a compiled layer")
    else {
        panic!("expected a colour operation");
    };
    assert_eq!(operation.len(), 2);
    assert_eq!(
        operation.units()[0].describe(),
        "white-balance(+60, -25)",
        "white balance is the first unit of the run"
    );
    assert_eq!(operation.units()[1].describe(), "exposure(+2.00)");

    // The rendered bytes against the reference composed in that order, including inputs the
    // exposure drives above 1.0 so the output boundary's clamp is exercised.
    let inputs: Vec<[u8; 3]> = vec![
        [10, 10, 10],
        [64, 96, 128],
        [200, 180, 160],
        [250, 250, 250],
        [255, 128, 0],
    ];
    let source = source_of(inputs.len() as u32, 1, &inputs);
    let rendered = render(
        &registry,
        &source,
        SnapshotId::new(),
        &recipe(vec![basic_layer(payload)]),
    )
    .expect("a rendered mixed layer");
    for (index, input) in inputs.iter().enumerate() {
        let linear = input.map(srgb_to_linear);
        let balanced = white_balance::apply(60.0, -25.0, linear);
        let exposed = balanced.map(|value| value * 4.0);
        for (channel, value) in exposed.into_iter().enumerate() {
            assert_code(
                rendered.pixel(index as u32, 0).expect("a pixel")[channel],
                reference::linear_to_code(value),
                value,
                &format!("{input:?} channel {channel}"),
            );
        }
    }
}

/// Two Basic layers are ambiguous for rendering, sampling and planning alike; nothing is rewritten.
#[test]
fn two_basic_layers_are_ambiguous_for_every_path() {
    let registry = ModuleRegistry::builtin();
    let source = source_of(2, 1, &[[10, 20, 30], [40, 50, 60]]);
    let stack = recipe(vec![
        basic_layer(json!({"temperature": 10.0})),
        basic_layer(json!({"tint": -10.0})),
    ]);
    for error in [
        render(&registry, &source, SnapshotId::new(), &stack).unwrap_err(),
        sample(&registry, &source, &stack, 0, 0).unwrap_err(),
    ] {
        assert_eq!(error.detail, "ambiguous Basic layers");
    }
    assert_eq!(stack.layers.len(), 2, "the refused stack is kept");
}

// ---------------------------------------------------------------------------------------------
// The neutral picker through the JSON API
// ---------------------------------------------------------------------------------------------

/// The synthetic cast photograph the picker tests run on, written as a JPEG so it goes through the
/// real import, verification and decode path an independent client uses.
///
/// Regions, all well inside their own 16-pixel block so a block's centre is far from any JPEG
/// block boundary: a warm-cast grey covering most of the frame, a blown-white block, a near-black
/// block and a saturated red block.
struct CastImage {
    path: PathBuf,
    width: u32,
    height: u32,
    /// The centre of the warm-cast region.
    neutral: (u32, u32),
    clipped: (u32, u32),
    dark: (u32, u32),
    saturated: (u32, u32),
}

fn write_cast_image(name: &str) -> CastImage {
    let (width, height) = (64u32, 64u32);
    // The colour of a corpus patch that solves to (20, -20): a plausible warm cast.
    let cast = grid_case(20, -20).patch_u8[12];
    let mut image = image::RgbImage::from_pixel(width, height, image::Rgb(cast));
    let block = |image: &mut image::RgbImage, x0: u32, y0: u32, colour: [u8; 3]| {
        for y in y0..y0 + 16 {
            for x in x0..x0 + 16 {
                image.put_pixel(x, y, image::Rgb(colour));
            }
        }
    };
    block(&mut image, 0, 0, [255, 255, 255]);
    block(&mut image, 48, 0, [12, 12, 12]);
    block(&mut image, 0, 48, [230, 30, 30]);
    let path = temp(&format!("{name}.jpg"));
    let mut file = std::io::BufWriter::new(fs::File::create(&path).expect("the JPEG file"));
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 100)
        .encode_image(&image)
        .expect("a written JPEG");
    drop(file);
    CastImage {
        path,
        width,
        height,
        neutral: (32, 32),
        clipped: (8, 8),
        dark: (56, 8),
        saturated: (8, 56),
    }
}

fn call(
    owner: &OwnerHandle,
    client: lightwell_core::ClientId,
    method: &str,
    params: Value,
) -> Value {
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
        .expect("the owner answered");
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.expect("a result")
}

/// Import an original and adopt it: the owner acknowledges with a bounded source job, the worker
/// prepares the file, and `job.adopt` hands the asset state back to this client.
fn import_asset(
    owner: &OwnerHandle,
    client: lightwell_core::ClientId,
    path: &std::path::Path,
) -> Value {
    let queued = call(owner, client, "catalog.import", json!({"path": path}));
    let job_id = queued["job_id"]
        .as_str()
        .expect("an import job id")
        .to_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = call(owner, client, "job.status", json!({"job_id": job_id}));
        match status["state"].as_str() {
            Some("ready") => break,
            Some("queued" | "preparing") => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the import never became ready: {status}"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            other => panic!("unexpected import job {other:?}: {status}"),
        }
    }
    call(owner, client, "job.adopt", json!({"job_id": job_id}))["asset"].clone()
}

fn call_error(
    owner: &OwnerHandle,
    client: lightwell_core::ClientId,
    method: &str,
    params: Value,
) -> lightwell_core::ApiFailure {
    owner
        .call(
            client,
            ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
        )
        .expect("the owner answered")
        .error
        .expect("an error")
}

/// The 8-bit codes a query answered with, and the settings the independent f64 reference solves
/// from exactly those codes. The picker's own answer must equal the reference's, whatever the JPEG
/// decoder produced.
fn reference_settings(result: &Value) -> Result<(i32, i32), RejectReason> {
    let pixels: Vec<[u8; 3]> =
        serde_json::from_value(result["patch"]["pixels"].clone()).expect("the sampled codes");
    white_balance::solve_from_patch(&pixels)
}

/// An independent JSON client discovers the query and the picker mode, runs the pick and applies
/// what it returns; the corrected patch is neutral to the code.
#[test]
fn a_client_discovers_the_picker_runs_it_and_applies_what_it_returns() {
    let image = write_cast_image("apply");
    let catalog = temp("apply.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog).expect("the owner loop");
    let client = owner.register();

    // Discovery: the query, its generated method and the canvas mode it puts on the strip.
    let modules = call(&owner, client, "module.list", json!({}));
    let basic = modules["modules"]
        .as_array()
        .expect("the modules")
        .iter()
        .find(|module| module["id"] == json!("lightwell.basic"))
        .expect("the Basic module")
        .clone();
    assert_eq!(
        basic["queries"][0]["id"],
        json!("neutral-sample"),
        "the query is discoverable: {basic}"
    );
    assert_eq!(
        basic["canvas"],
        json!({
            "kind": "sample-apply",
            "query": "neutral-sample",
            "x": "x",
            "y": "y",
            "action": "set-basic",
            "title": "Neutral picker",
            "shortcut": "W",
        })
    );
    let schema = call(&owner, client, "schema.list", json!({}));
    assert_eq!(
        schema["methods"]["query.neutral-sample"]["mutates"],
        json!(false)
    );
    // The picker's mode is accepted by workspace.set because the accepted modes are derived from
    // the canvas declarations themselves.
    assert_eq!(
        call(
            &owner,
            client,
            "workspace.set",
            json!({"mode": "lightwell.basic"})
        )["workspace"]["mode"],
        json!("lightwell.basic")
    );

    let asset = import_asset(&owner, client, &image.path)["asset"]["id"].clone();
    let (x, y) = image.neutral;
    let picked = call(
        &owner,
        client,
        "query.neutral-sample",
        json!({"asset_id": asset, "x": x, "y": y}),
    );

    // The patch it read, and the settings it solved from exactly those codes.
    assert_eq!(
        picked["patch"],
        json!({
            "x": x - 2, "y": y - 2, "width": 5, "height": 5,
            "pixels": picked["patch"]["pixels"],
            "mean_linear": picked["patch"]["mean_linear"],
        })
    );
    assert_eq!(
        picked["patch"]["pixels"]
            .as_array()
            .expect("the codes")
            .len(),
        25
    );
    let (temperature, tint) = (
        picked["temperature"].as_i64().expect("a temperature"),
        picked["tint"].as_i64().expect("a tint"),
    );
    assert_eq!(
        reference_settings(&picked).expect("a solved patch"),
        (temperature as i32, tint as i32),
        "the picker's answer is the independent reference's answer for the codes it read"
    );
    // The cast was built from the corpus patch that solves to (20, -20); a JPEG round trip and a
    // flatter patch move it a little, never far.
    assert!(
        (temperature - 20).abs() <= 3 && (tint + 20).abs() <= 6,
        "the recovered correction ({temperature}, {tint}) is not near the cast it was built from"
    );

    // Applying it is an ordinary set-basic: one entry, and the patch becomes neutral to the code.
    let applied = call(
        &owner,
        client,
        "edit.set-basic",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 0, "request_id": "pick", "actor": "test"},
            "temperature": temperature,
            "tint": tint,
        }),
    );
    assert_eq!(applied["outcome"], json!("applied"));
    let sampled = call(
        &owner,
        client,
        "render.sample",
        json!({"asset_id": asset, "x": x, "y": y}),
    );
    let rgba: Vec<i64> = serde_json::from_value(sampled["rgba"].clone()).expect("a pixel");
    let spread = rgba[..3].iter().max().unwrap() - rgba[..3].iter().min().unwrap();
    assert!(
        spread <= 1,
        "the corrected patch is not neutral to the code: {rgba:?}"
    );

    owner.disconnect(client);
    owner.stop();
    join.join().expect("the owner loop ends");
    let _ = fs::remove_file(catalog);
    let _ = fs::remove_file(image.path);
}

/// The picker evaluates before the Basic layer, so a strong correction already in the stack does
/// not change what the next pick reads or returns.
#[test]
fn the_picker_reads_the_stage_before_the_basic_layer() {
    let image = write_cast_image("before");
    let catalog = temp("before.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog).expect("the owner loop");
    let client = owner.register();
    let asset = import_asset(&owner, client, &image.path)["asset"]["id"].clone();
    let (x, y) = image.neutral;
    let params = json!({"asset_id": asset, "x": x, "y": y});
    let before = call(&owner, client, "query.neutral-sample", params.clone());

    // A strong white balance, then a pixel replacement and a quarter turn on top of it.
    call(
        &owner,
        client,
        "edit.set-basic",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 0, "request_id": "strong", "actor": "test"},
            "temperature": 90, "tint": 70,
        }),
    );
    call(
        &owner,
        client,
        "edit.transform",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 1, "request_id": "turn", "actor": "test"},
            "transform": "rotate-right",
        }),
    );
    let after = call(&owner, client, "query.neutral-sample", params);
    assert_eq!(
        after, before,
        "the pick sees the stage the Basic layer receives, not its own correction"
    );

    owner.disconnect(client);
    owner.stop();
    join.join().expect("the owner loop ends");
    let _ = fs::remove_file(catalog);
    let _ = fs::remove_file(image.path);
}

/// The patch is clipped at the stage's edges, and every refusal names its reason first and commits
/// nothing.
#[test]
fn edges_are_clipped_and_bad_patches_are_refused_with_their_reason() {
    let image = write_cast_image("edges");
    let catalog = temp("edges.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog).expect("the owner loop");
    let client = owner.register();
    let asset = import_asset(&owner, client, &image.path)["asset"]["id"].clone();
    let pick = |x: i64, y: i64| json!({"asset_id": asset, "x": x, "y": y});

    // The four corners of the stage, and one edge: the patch shrinks and says which rectangle it
    // read. The corners of this image are the white and red blocks, so only the rectangle is
    // asserted there; the two right-hand corners are inside the cast region and also solve.
    for (case, (x, y), expected) in [
        ("the top-left corner", (0, 0), (0, 0, 3, 3)),
        (
            "the bottom-right corner",
            (image.width as i64 - 1, image.height as i64 - 1),
            (image.width as i64 - 3, image.height as i64 - 3, 3, 3),
        ),
        (
            "one in from the bottom-right corner",
            (image.width as i64 - 2, image.height as i64 - 2),
            (image.width as i64 - 4, image.height as i64 - 4, 4, 4),
        ),
        (
            "a right edge",
            (image.width as i64 - 1, 32),
            (image.width as i64 - 3, 30, 3, 5),
        ),
    ] {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: case.into(),
                    method: "query.neutral-sample".into(),
                    params: pick(x, y),
                    token: None,
                },
            )
            .expect("the owner answered");
        match response.result {
            Some(result) => {
                let patch = &result["patch"];
                assert_eq!(
                    (
                        patch["x"].as_i64().unwrap(),
                        patch["y"].as_i64().unwrap(),
                        patch["width"].as_i64().unwrap(),
                        patch["height"].as_i64().unwrap(),
                    ),
                    expected,
                    "{case}"
                );
                assert_eq!(
                    patch["pixels"].as_array().unwrap().len() as i64,
                    expected.2 * expected.3,
                    "{case}: every sampled pixel is reported"
                );
                assert_eq!(
                    reference_settings(&result).expect("a solved patch"),
                    (
                        result["temperature"].as_i64().unwrap() as i32,
                        result["tint"].as_i64().unwrap() as i32
                    ),
                    "{case}"
                );
            }
            None => {
                // A corner sitting on the blown-white block is refused, not clamped inward.
                let error = response.error.expect("a reason");
                assert_eq!(error.code, "validation", "{case}");
                assert!(error.message.starts_with("clipped:"), "{case}: {error:?}");
            }
        }
    }

    for (case, (x, y), prefix) in [
        ("the blown-white block", image.clipped, "clipped:"),
        ("the near-black block", image.dark, "near-black:"),
        ("the saturated red block", image.saturated, "out-of-range:"),
        (
            "a point past the right edge",
            (image.width, 32),
            "outside the stage:",
        ),
        (
            "a point past the bottom edge",
            (32, image.height),
            "outside the stage:",
        ),
    ] {
        let error = call_error(
            &owner,
            client,
            "query.neutral-sample",
            pick(i64::from(x), i64::from(y)),
        );
        assert_eq!(error.code, "validation", "{case}");
        assert!(error.message.starts_with(prefix), "{case}: {error:?}");
    }
    // A coordinate outside the declared parameter range is refused by the generic check.
    let error = call_error(&owner, client, "query.neutral-sample", pick(-1, 0));
    assert_eq!(error.code, "validation");
    let error = call_error(&owner, client, "query.neutral-sample", pick(0, 99_999));
    assert!(
        error.message.contains("must be an integer within"),
        "{error:?}"
    );
    // No refusal wrote anything.
    assert_eq!(
        call(&owner, client, "asset.state", json!({"asset_id": asset}))["revision"],
        json!(0)
    );

    owner.disconnect(client);
    owner.stop();
    join.join().expect("the owner loop ends");
    let _ = fs::remove_file(catalog);
    let _ = fs::remove_file(image.path);
}

/// The desktop's flow — locate the picked view pixel, then query that content pixel — gives the
/// same content pixel and the same settings as asking about the content coordinates directly, even
/// through a quarter turn and an interpolating 10° crop.
#[test]
fn locate_then_query_matches_the_direct_content_coordinates_through_geometry() {
    let image = write_cast_image("locate");
    let catalog = temp("locate.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog).expect("the owner loop");
    let client = owner.register();
    let asset = import_asset(&owner, client, &image.path)["asset"]["id"].clone();
    let (content_x, content_y) = image.neutral;
    let direct = call(
        &owner,
        client,
        "query.neutral-sample",
        json!({"asset_id": asset, "x": content_x, "y": content_y}),
    );

    call(
        &owner,
        client,
        "edit.transform",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 0, "request_id": "turn", "actor": "test"},
            "transform": "rotate-right",
        }),
    );
    call(
        &owner,
        client,
        "edit.crop",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 1, "request_id": "crop", "actor": "test"},
            "angle": 10.0, "x": 0.15, "y": 0.15, "width": 0.6, "height": 0.6,
        }),
    );
    let state = call(&owner, client, "asset.state", json!({"asset_id": asset}));
    assert_eq!(state["revision"], json!(2));

    // Find the rendered pixel whose content pixel is the one picked before the geometry existed.
    let rendered = call(
        &owner,
        client,
        "recipe.describe",
        json!({"asset_id": asset}),
    );
    assert!(
        rendered["layers"].as_array().expect("layers").len() >= 2,
        "the stack carries the turn and the crop"
    );
    let mut found = None;
    'search: for y in 0..64u32 {
        for x in 0..64u32 {
            let response = owner
                .call(
                    client,
                    ApiRequest {
                        id: "locate".into(),
                        method: "render.locate".into(),
                        params: json!({"asset_id": asset, "x": x, "y": y}),
                        token: None,
                    },
                )
                .expect("the owner answered");
            let Some(point) = response.result else {
                continue;
            };
            if point["content_x"] == json!(content_x) && point["content_y"] == json!(content_y) {
                found = Some((x, y, point));
                break 'search;
            }
        }
    }
    let (view_x, view_y, point) =
        found.expect("some rendered pixel shows the content pixel that was picked");
    assert_eq!(point["width"], json!(image.width));
    assert_eq!(point["height"], json!(image.height));

    let located = call(
        &owner,
        client,
        "query.neutral-sample",
        json!({
            "asset_id": asset,
            "x": point["content_x"],
            "y": point["content_y"],
        }),
    );
    assert_eq!(
        located, direct,
        "the pick at view ({view_x}, {view_y}) reads the same content patch as the direct \
         coordinates, through the turn and the crop"
    );

    owner.disconnect(client);
    owner.stop();
    join.join().expect("the owner loop ends");
    let _ = fs::remove_file(catalog);
    let _ = fs::remove_file(image.path);
}

/// A query is read-only: two clients asking at once get identical answers, no history entry is
/// created and no event is emitted. A client previewing a historical entry may still ask.
#[test]
fn a_query_is_read_only_and_two_clients_agree() {
    let image = write_cast_image("shared");
    let catalog = temp("shared.sqlite");
    let (owner, join) = OwnerHandle::start(&catalog).expect("the owner loop");
    let first = owner.register();
    let second = owner.register();
    let asset = import_asset(&owner, first, &image.path)["asset"]["id"].clone();
    let entries_before = call(
        &owner,
        first,
        "history.list",
        json!({"asset_id": asset, "limit": 50}),
    )["entries"]
        .as_array()
        .expect("entries")
        .len();
    let sequence_before =
        call(&owner, first, "events.since", json!({"after": 0}))["current_sequence"]
            .as_u64()
            .expect("a sequence");

    let (x, y) = image.neutral;
    let params = json!({"asset_id": asset, "x": x, "y": y});
    let one = call(&owner, first, "query.neutral-sample", params.clone());
    let two = call(&owner, second, "query.neutral-sample", params.clone());
    assert_eq!(one, two, "two clients read the same stack the same way");
    let again = call(&owner, first, "query.neutral-sample", params.clone());
    assert_eq!(again, one, "the same question answers the same way twice");

    assert_eq!(
        call(
            &owner,
            first,
            "history.list",
            json!({"asset_id": asset, "limit": 50})
        )["entries"]
            .as_array()
            .expect("entries")
            .len(),
        entries_before,
        "a query adds no history entry"
    );
    let events = call(&owner, first, "events.since", json!({"after": 0}));
    assert_eq!(
        events["current_sequence"].as_u64().expect("a sequence"),
        sequence_before,
        "a query emits no event"
    );

    // A commit by one client does not change what a query of the entry before it answers: a query
    // names the entry it asks about, and a read-only client on a historical entry may still pick.
    let state = call(&owner, first, "asset.state", json!({"asset_id": asset}));
    let original_entry = state["current_entry"]["id"].clone();
    call(
        &owner,
        first,
        "edit.set-basic",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 0, "request_id": "strong", "actor": "test"},
            "temperature": 80,
        }),
    );
    call(
        &owner,
        second,
        "preview.select",
        json!({"asset_id": asset, "entry_id": original_entry}),
    );
    assert_eq!(
        call(&owner, second, "query.neutral-sample", params.clone()),
        one,
        "a historical selection answers about that entry"
    );
    assert_eq!(
        call(
            &owner,
            first,
            "query.neutral-sample",
            json!({"asset_id": asset, "entry_id": original_entry, "x": x, "y": y})
        ),
        one,
        "naming the entry explicitly answers the same way"
    );
    // An edit from a read-only selection is still refused; a query is not.
    let refused = call_error(
        &owner,
        second,
        "edit.set-basic",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": 1, "request_id": "nope", "actor": "test"},
            "temperature": 10,
        }),
    );
    assert_eq!(refused.code, "conflict");

    owner.disconnect(first);
    owner.disconnect(second);
    owner.stop();
    join.join().expect("the owner loop ends");
    let _ = fs::remove_file(catalog);
    let _ = fs::remove_file(image.path);
}

/// The stored values, the history labels and the group reset a white-balance edit produces.
#[test]
fn values_and_labels_name_the_white_balance_fields_and_their_group() {
    let catalog = temp("labels.sqlite");
    let mut service = EditorService::open(&catalog).expect("a catalog");
    let asset = service
        .import(&fixture("s0/orientation-1.jpg"))
        .expect("an import")
        .asset
        .id;

    let labelled = |service: &mut EditorService, revision: u64, request: &str, params: Value| {
        let result = service
            .apply_action(&asset, mutation(revision, request), "set-basic", params)
            .unwrap_or_else(|error| panic!("{request}: {error}"));
        result.created_entry_id.map(|entry| {
            service
                .entry(&asset, &entry)
                .expect("the entry")
                .label
                .clone()
        })
    };
    assert_eq!(
        labelled(&mut service, 0, "warm", json!({"temperature": 20})).as_deref(),
        Some("Temperature +20")
    );
    assert_eq!(
        labelled(&mut service, 1, "green", json!({"tint": -5})).as_deref(),
        Some("Tint -5")
    );
    assert_eq!(
        labelled(
            &mut service,
            2,
            "both",
            json!({"temperature": -40, "tint": 15})
        )
        .as_deref(),
        Some("Basic (2 fields)"),
        "a patch that is not one field and not one group's reset names its size"
    );
    assert_eq!(
        labelled(
            &mut service,
            3,
            "wb-reset",
            json!({"temperature": 0, "tint": 0})
        )
        .as_deref(),
        Some("Reset White balance")
    );

    // The row reports every implemented field, and the stored payload keeps only what is not
    // neutral, so a reset group leaves the canonical empty object behind.
    service
        .apply_action(
            &asset,
            mutation(4, "warm-again"),
            "set-basic",
            json!({"temperature": 35, "tint": -12}),
        )
        .expect("a correction");
    let described = service.describe_entry(&asset, None).expect("a description");
    let row = described
        .layers
        .iter()
        .find(|layer| layer.effect == BASIC_EFFECT)
        .expect("the Basic row");
    assert_eq!(row.summary, "Temperature +35, Tint -12");
    assert_eq!(row.values.get("temperature"), Some(&json!(35.0)));
    assert_eq!(row.values.get("tint"), Some(&json!(-12.0)));
    assert_eq!(row.values.get("exposure"), Some(&json!(0.0)));

    // The stored values survive a reopen and still render the same bytes.
    let before = service.render_current(&asset).expect("a frame");
    drop(service);
    let service = EditorService::open(&catalog).expect("the reopened catalog");
    let after = service.render_current(&asset).expect("a frame");
    assert_eq!(before.rgba, after.rgba, "a reopen renders the same bytes");
    let described = service.describe_entry(&asset, None).expect("a description");
    assert_eq!(
        described
            .layers
            .iter()
            .find(|layer| layer.effect == BASIC_EFFECT)
            .expect("the Basic row")
            .summary,
        "Temperature +35, Tint -12"
    );

    drop(service);
    let _ = fs::remove_file(catalog);
}
