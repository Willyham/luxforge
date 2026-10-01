//! The Tone curve module (`luxforge.curve`) end to end: the frozen fixture through the real render
//! path on the byte and RAW linear paths, the identity map keeping the source, the layer's place
//! after Basic and before the colour mixer, the unit-slope tail past white, collapsed knots, and
//! the kind's refusals. What the curve shares with every field-patch module is proved once, for
//! every such module, by the conformance suite (`field_patch`).
//!
//! Numerical rule, from `docs/design/tone-curve.md`'s frozen tolerance: for a point set whose peak
//! slope is at most 64, a rendered code equals the `f64` reference's code, except where the
//! reference's linear value sits within `1e-5 + 1e-5 * |threshold|` of the threshold between two
//! codes, where one code of difference is permitted. For a steeper set the rendered code lies
//! within the codes of the reference evaluated with its encoded input perturbed by `±2^-20`,
//! widened by the forward tolerance. Identity stacks and byte sharing are exact.

use luxforge_core::{
    BASIC_EFFECT, CURVE_EFFECT, Layer, LinearSettings, MIXER_EFFECT, ModuleRegistry, OwnerHandle,
    Processing, SnapshotId, Stage,
};
use luxforge_reference::{
    self as reference,
    curve::{CurvePoints, curve_pixel, curve_pixel_with_input_error},
};
use luxforge_testbase::paths;
use luxforge_testkit::client::{self, call, import, refused};
use luxforge_testkit::fixtures::{
    self, linear_source_of, recipe, render, render_linear, source_of,
};
use serde_json::{Value, json};
use std::{fs, sync::Arc};

/// The forward tolerance's relative band.
const BAND: f64 = 1e-5;
/// The peak slope above which a set takes the backward tolerance.
const FORWARD_SLOPE: f64 = 64.0;
/// The backward tolerance's relative error in the encoded input.
const INPUT_ERROR: f64 = 1.0 / 1_048_576.0;

/// A global curve layer holding `points`.
fn layer(points: &[[f64; 2]]) -> Layer {
    fixtures::layer(CURVE_EFFECT, json!({"luminance": points}))
}

fn tolerance(value: f64) -> f64 {
    BAND + BAND * value.abs()
}

fn fixture() -> Value {
    let path = paths::fixture("curve/curve-cases.json");
    serde_json::from_str(&fs::read_to_string(&path).expect("the curve fixtures"))
        .expect("valid fixture JSON")
}

fn points_of(value: &Value) -> Vec<[f64; 2]> {
    value
        .as_array()
        .expect("a point list")
        .iter()
        .map(|point| [point[0].as_f64().unwrap(), point[1].as_f64().unwrap()])
        .collect()
}

fn triple(value: &Value) -> [f64; 3] {
    std::array::from_fn(|channel| value[channel].as_f64().expect("a value"))
}

/// A rendered code against the reference: the forward rule, or, for a steep set, the codes of the
/// backward band. Answers the code distance from the unperturbed reference's code.
fn check_code(
    actual: u8,
    points: &CurvePoints,
    steep: bool,
    input: [f64; 3],
    expected: [f64; 3],
    channel: usize,
    case: &str,
) -> u32 {
    let code = reference::srgb::code(expected[channel]);
    if steep {
        let band = [-INPUT_ERROR, INPUT_ERROR]
            .map(|relative| curve_pixel_with_input_error(points, input, relative)[channel]);
        let low = expected[channel].min(band[0]).min(band[1]);
        let high = expected[channel].max(band[0]).max(band[1]);
        let low = reference::srgb::code(low - tolerance(low));
        let high = reference::srgb::code(high + tolerance(high));
        assert!(
            (low..=high).contains(&actual),
            "{case}: rendered {actual} outside the backward band's codes {low}..={high}"
        );
    } else {
        fixtures::assert_code_near_threshold(actual, code, expected[channel], BAND, case);
    }
    u32::from(actual.abs_diff(code))
}

// -------------------------------------------------------------------------------------------
// The fixture through render
// -------------------------------------------------------------------------------------------

/// Every fixture point set through the real render path on both pixel domains. The RAW linear path
/// takes every one of the 1,204 cases exactly as the fixture writes them (each input is
/// `f32`-representable, negative and over-white values included). The byte path can only carry
/// 8-bit codes, so it renders, under each fixture set, every grey code, a colour cube in steps of
/// 51 (the fixture's full-intensity corners among them, checked against the fixture's own expected
/// values) against the independent reference evaluated on exactly the decoded `f32` input.
#[test]
fn the_curve_matches_its_fixture_through_render_on_both_paths() {
    let file = fixture();
    let sets = file["point_sets"].as_object().expect("the point sets");
    let cases = file["cases"].as_array().expect("the cases");
    assert_eq!(cases.len(), 1204, "every frozen case is checked");
    let registry = ModuleRegistry::builtin();

    let mut codes: Vec<[u8; 3]> = (0..=255u8).map(|code| [code; 3]).collect();
    for r in (0..=255u8).step_by(51) {
        for g in (0..=255u8).step_by(51) {
            for b in (0..=255u8).step_by(51) {
                codes.push([r, g, b]);
            }
        }
    }
    let byte_source = source_of(codes.len() as u32, 1, &codes);
    let decoded: Vec<[f64; 3]> = codes
        .iter()
        .map(|pixel| pixel.map(|code| f64::from(reference::srgb::decode(code) as f32)))
        .collect();

    let (mut checked, mut worst) = ([0usize; 2], [(0u32, String::new()), (0u32, String::new())]);
    for (set_name, set) in sets {
        let points = points_of(&set["points"]);
        let reference_points = CurvePoints::new(&points);
        let steep = set["peak_slope"]
            .as_f64()
            .is_none_or(|slope| slope > FORWARD_SLOPE);
        let stack = recipe(vec![layer(&points)]);

        // The RAW linear path, over the fixture's own inputs and expected outputs.
        let own: Vec<&Value> = cases
            .iter()
            .filter(|case| case["set"] == *set_name)
            .collect();
        assert_eq!(own.len(), 86, "{set_name}: every input");
        let inputs: Vec<[f64; 3]> = own
            .iter()
            .map(|case| triple(&case["input_linear_rgb"]))
            .collect();
        let linear = linear_source_of(inputs.len() as u32, 1, &inputs);
        let rendered = render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap_or_else(|error| panic!("{set_name} (linear): {error}"));
        for (index, case) in own.iter().enumerate() {
            let name = case["name"].as_str().unwrap();
            let expected = triple(&case["expected_linear_rgb"]);
            let pixel = rendered.pixel(index as u32, 0).unwrap();
            assert_eq!(pixel[3], 255, "{name}: alpha is never touched");
            for (channel, &actual) in pixel[..3].iter().enumerate() {
                let off = check_code(
                    actual,
                    &reference_points,
                    steep,
                    inputs[index],
                    expected,
                    channel,
                    &format!("{name} channel {channel} (linear)"),
                );
                if off > worst[0].0 {
                    worst[0] = (off, format!("{name} channel {channel}"));
                }
                checked[0] += 1;
            }
        }

        // The byte path, over every grey code and a colour cube.
        let rendered = render(&registry, &byte_source, SnapshotId::new(), &stack)
            .unwrap_or_else(|error| panic!("{set_name} (byte): {error}"));
        for (index, input) in decoded.iter().enumerate() {
            let expected = curve_pixel(&reference_points, *input);
            let pixel = rendered.pixel(index as u32, 0).unwrap();
            assert_eq!(pixel[3], 255, "{set_name}: alpha is never touched");
            for (channel, &actual) in pixel[..3].iter().enumerate() {
                let off = check_code(
                    actual,
                    &reference_points,
                    steep,
                    *input,
                    expected,
                    channel,
                    &format!("{set_name}/{:?} channel {channel} (byte)", codes[index]),
                );
                if off > worst[1].0 {
                    worst[1] = (
                        off,
                        format!("{set_name}/{:?} channel {channel}", codes[index]),
                    );
                }
                checked[1] += 1;
            }
        }
        // The fixture's full-intensity corners are exact codes: the byte path's reference agrees
        // with the fixture's expected values for them.
        for case in own.iter().filter(|case| {
            triple(&case["input_linear_rgb"])
                .iter()
                .all(|channel| *channel == 0.0 || *channel == 1.0)
        }) {
            let input = triple(&case["input_linear_rgb"]);
            let expected = triple(&case["expected_linear_rgb"]);
            let recomputed = curve_pixel(&reference_points, input);
            for channel in 0..3 {
                assert!(
                    (recomputed[channel] - expected[channel]).abs()
                        <= 1e-12 + 1e-12 * expected[channel].abs(),
                    "{}: the byte path's reference is the fixture's",
                    case["name"]
                );
            }
        }
    }
    // Printed with --nocapture so the handoff can quote measured figures.
    println!(
        "linear path: {} channels, largest code distance from the reference {} at {}",
        checked[0], worst[0].0, worst[0].1
    );
    println!(
        "byte path: {} channels, largest code distance from the reference {} at {}",
        checked[1], worst[1].0, worst[1].1
    );
}

// -------------------------------------------------------------------------------------------
// Identity
// -------------------------------------------------------------------------------------------

/// The neutral payload and every exact identity map compile to no units, keep the identity byte
/// path and share the source allocation, on the byte path and bit for bit on the linear one; an
/// on-diagonal curve with more points is still a stored, described, non-neutral layer.
#[test]
fn on_diagonal_points_compile_to_no_units_and_share_the_source() {
    let registry = ModuleRegistry::builtin();
    let module = registry.module("luxforge.curve").expect("the curve");
    let codes: Vec<[u8; 3]> = (0..=255u8)
        .map(|code| [code, code / 2, 255 - code])
        .collect();
    let source = source_of(256, 1, &codes);
    let decoded: Vec<[f64; 3]> = codes
        .iter()
        .map(|pixel| pixel.map(reference::srgb::decode))
        .collect();
    let linear = linear_source_of(256, 1, &decoded);
    let untouched = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &recipe(Vec::new()),
        LinearSettings::default(),
    )
    .unwrap();
    for payload in [
        json!({}),
        json!({"luminance": [[0, 0], [1, 1]]}),
        json!({"luminance": [[0, 0], [0.5, 0.5], [1, 1]]}),
        json!({"luminance": [[0, 0], [0.25, 0.25], [0.3, 0.3], [0.75, 0.75], [1, 1]]}),
    ] {
        let Processing::Color(operation) = module
            .compile(
                CURVE_EFFECT,
                1,
                &payload,
                Stage {
                    width: 256,
                    height: 1,
                },
            )
            .unwrap()
        else {
            panic!("{payload}: a colour operation");
        };
        assert!(operation.is_empty(), "{payload} compiled to units");
        let stack = recipe(vec![fixtures::layer(CURVE_EFFECT, payload.clone())]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        assert!(
            Arc::ptr_eq(&rendered.rgba, &source.rgba),
            "{payload} did not share the source allocation"
        );
        let rendered = render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap();
        assert_eq!(
            rendered.rgba, untouched.rgba,
            "{payload} on the linear path"
        );
    }
    let report = registry
        .layer_report(&fixtures::layer(
            CURVE_EFFECT,
            json!({"luminance": [[0, 0], [0.5, 0.5], [1, 1]]}),
        ))
        .unwrap();
    assert_eq!(report.summary, "Tone curve 3 points");
    assert!(!report.neutral);
}

// -------------------------------------------------------------------------------------------
// Placement
// -------------------------------------------------------------------------------------------

/// Through the JSON methods, committing Basic, the curve and the mixer in each of the six touch
/// orders always stacks Basic, then the curve, then the mixer, and every order renders the same
/// bytes.
#[test]
fn the_curve_lands_after_basic_and_before_the_mixer_in_every_touch_order() {
    let catalog = paths::temp_catalog("curve-placement");
    let (owner, join) = OwnerHandle::start(&catalog).expect("an owner");
    let editor = owner.register();
    let imported = import(&owner, editor, &paths::jpeg(), "curve-placement").unwrap();
    let asset = imported["asset"]["id"].clone();
    let original = imported["current_entry"]["id"].clone();
    let payload_for = |action: &str| match action {
        "set-basic" => json!({"exposure": 0.3}),
        "set-curve" => json!({"luminance": [[0, 0], [0.5, 0.6], [1, 1]]}),
        _ => json!({"red-hue": 10.0}),
    };
    let registry = ModuleRegistry::builtin();
    let source = source_of(
        3,
        2,
        &[
            [200, 40, 30],
            [30, 180, 60],
            [20, 50, 210],
            [128, 128, 128],
            [240, 230, 10],
            [5, 6, 7],
        ],
    );
    let mut bytes: Option<Vec<u8>> = None;
    let actions = ["set-basic", "set-curve", "set-mixer"];
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for order in orders {
        let tag = order.map(|index| actions[index]).join("-");
        let revision = client::revision(&owner, editor, &asset).unwrap();
        call(
            &owner,
            editor,
            "history.restore",
            json!({"asset_id": asset, "mutation": client::mutation(revision, &client::request_id(&format!("restore-{tag}")), "curve-placement"), "entry_id": original}),
        )
        .unwrap();
        for index in order {
            let action = actions[index];
            let revision = client::revision(&owner, editor, &asset).unwrap();
            let mut params = payload_for(action);
            params["asset_id"] = asset.clone();
            params["mutation"] = client::mutation(
                revision,
                &client::request_id(&format!("{tag}-{action}")),
                "curve-placement",
            );
            let result = call(&owner, editor, &format!("edit.{action}"), params).unwrap();
            assert_eq!(result["outcome"], "applied", "{tag}: {action}");
        }
        let described = call(
            &owner,
            editor,
            "recipe.describe",
            json!({"asset_id": asset}),
        )
        .unwrap();
        let effects: Vec<Value> = described["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| layer["effect"].clone())
            .collect();
        assert_eq!(
            effects,
            [
                json!(BASIC_EFFECT),
                json!(CURVE_EFFECT),
                json!(MIXER_EFFECT)
            ],
            "touch order {tag}"
        );
        let stack = client::recipe(&owner, editor, &asset).unwrap();
        let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        match &bytes {
            None => bytes = Some(rendered.rgba.to_vec()),
            Some(first) => assert_eq!(first, &rendered.rgba.to_vec(), "touch order {tag}"),
        }
    }
    owner.stop();
    join.join().expect("the owner joined");
    let _ = fs::remove_file(catalog);
}

// -------------------------------------------------------------------------------------------
// Tails, collapsed knots and refusals
// -------------------------------------------------------------------------------------------

/// A linear pixel above white takes the unit-slope tail: under a lowered white point, over-white
/// greys and the fixture's over-white colour render the reference's codes, and over-white greys
/// stay apart in order rather than collapsing onto the white point's output.
#[test]
fn an_over_white_pixel_takes_the_unit_slope_tail() {
    let registry = ModuleRegistry::builtin();
    let points = [[0.0, 0.0], [1.0, 0.9]];
    let reference_points = CurvePoints::new(&points);
    let encoded = [1.02, 1.05, 1.08];
    let mut inputs: Vec<[f64; 3]> = encoded
        .iter()
        .map(|&x| [f64::from(reference::srgb::decode_encoded(x) as f32); 3])
        .collect();
    inputs.push([1.5, 1.2, 0.9]);
    let linear = linear_source_of(inputs.len() as u32, 1, &inputs);
    let rendered = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &recipe(vec![layer(&points)]),
        LinearSettings::default(),
    )
    .unwrap();
    let mut previous = 0u8;
    for (index, input) in inputs.iter().enumerate() {
        let expected = curve_pixel(&reference_points, *input);
        let pixel = rendered.pixel(index as u32, 0).unwrap();
        for (channel, &actual) in pixel[..3].iter().enumerate() {
            check_code(
                actual,
                &reference_points,
                false,
                *input,
                expected,
                channel,
                &format!("{input:?} channel {channel}"),
            );
        }
        if index < encoded.len() {
            assert!(pixel[0] < 255, "{input:?} is lowered below white");
            assert!(
                pixel[0] > previous,
                "{input:?} stays above the grey before it"
            );
            previous = pixel[0];
        }
    }
    // The flat hold would have mapped every over-white grey to the white point's output, 0.9.
    let held = reference::srgb::code(reference::srgb::decode_encoded(0.9));
    assert!(previous > held);
}

/// The desktop's drag clamp can leave two points `4 · f64::EPSILON` apart: the curve is accepted,
/// renders on both paths, keeps greys grey and never inverts the grey ramp.
#[test]
fn a_drag_clamped_against_a_neighbour_renders() {
    let registry = ModuleRegistry::builtin();
    let points = [
        [0.0, 0.0],
        [0.5, 0.4],
        [0.5 + 4.0 * f64::EPSILON, 0.6],
        [1.0, 1.0],
    ];
    let stack = recipe(vec![layer(&points)]);
    assert!(registry.layer_report(&stack.layers[0]).is_ok());
    let greys: Vec<[u8; 3]> = (0..=255u8).map(|code| [code; 3]).collect();
    let source = source_of(256, 1, &greys);
    let linear = linear_source_of(
        256,
        1,
        &greys
            .iter()
            .map(|pixel| pixel.map(reference::srgb::decode))
            .collect::<Vec<_>>(),
    );
    let byte = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
    let raw = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &stack,
        LinearSettings::default(),
    )
    .unwrap();
    for (path, rendered) in [("byte", &byte), ("linear", &raw)] {
        let mut previous = 0u8;
        for x in 0..256u32 {
            let [r, g, b, a] = rendered.pixel(x, 0).unwrap();
            assert!(
                r == g && g == b && a == 255,
                "{path}: grey {x} -> {r},{g},{b},{a}"
            );
            assert!(
                r >= previous,
                "{path}: grey {x} inverts ({r} after {previous})"
            );
            previous = r;
        }
        // The step sits at encoded 0.5: code 127 (0.498) is below it, code 128 (0.502) above.
        assert!(rendered.pixel(128, 0).unwrap()[0] > rendered.pixel(127, 0).unwrap()[0]);
    }
}

/// The curve kind's limits refuse, through `edit.set-curve`, a seventeenth point, a decreasing
/// output, a repeated input and a coordinate outside `0..=1`, each with the kind's text naming the
/// luminance field; the sample query refuses them the same way.
#[test]
fn a_seventeenth_point_a_decreasing_output_a_repeated_input_and_a_coordinate_outside_the_unit_are_refused()
 {
    let catalog = paths::temp_catalog("curve-refusals");
    let (owner, join) = OwnerHandle::start(&catalog).expect("an owner");
    let editor = owner.register();
    let asset =
        import(&owner, editor, &paths::jpeg(), "curve-refusals").unwrap()["asset"]["id"].clone();
    let seventeen: Vec<[f64; 2]> = (0..17)
        .map(|k| [f64::from(k) / 16.0, f64::from(k) / 16.0])
        .collect();
    for (points, refusal) in [
        (
            json!(seventeen),
            "parameter luminance has an invalid curve point count",
        ),
        (
            json!([[0, 0], [0.5, 0.6], [0.7, 0.5], [1, 1]]),
            "parameter luminance has an invalid curve point 2",
        ),
        (
            json!([[0, 0], [0.5, 0.4], [0.5, 0.6], [1, 1]]),
            "parameter luminance has an invalid curve point 2",
        ),
        (
            json!([[0, 0], [0.5, 1.2], [1, 1]]),
            "parameter luminance has an invalid curve point 1",
        ),
        (
            json!([[-0.1, 0], [1, 1]]),
            "parameter luminance has an invalid curve point 0",
        ),
    ] {
        let revision = client::revision(&owner, editor, &asset).unwrap();
        let (code, message) = refused(
            &owner,
            editor,
            "edit.set-curve",
            json!({
                "asset_id": asset,
                "mutation": client::mutation(revision, &client::request_id("refused"), "curve-refusals"),
                "luminance": points,
            }),
        )
        .unwrap();
        assert_eq!(code, "validation", "{points}");
        assert!(message.contains(refusal), "{points}: {message}");
        let (code, message) = refused(
            &owner,
            editor,
            "query.sample-curve",
            json!({"asset_id": asset, "luminance": points}),
        )
        .unwrap();
        assert_eq!(code, "validation", "{points}");
        assert!(message.contains(refusal), "{points}: {message}");
    }
    assert!(
        client::recipe(&owner, editor, &asset)
            .unwrap()
            .layers
            .is_empty()
    );
    let sampled = call(
        &owner,
        editor,
        "query.sample-curve",
        json!({"asset_id": asset, "luminance": [[0, 0], [0.5, 0.6], [1, 1]]}),
    )
    .unwrap();
    assert_eq!(sampled["points"].as_array().unwrap().len(), 257);
    owner.stop();
    join.join().expect("the owner joined");
    let _ = fs::remove_file(catalog);
}
