//! TASK-005: the masked colour primitive through the public pipeline, on the JPEG byte path and on
//! the RAW linear path, against the independent `f64` reference.
//!
//! The oracle is `crates/lightwell-reference/src/mask.rs` — the frozen coverage mathematics, which shares no code
//! with `lightwell-core` — composed with `lightwell-reference`'s sRGB and exposure reference and
//! that module's own `blend`, `out = (1 − M)·in + M·effect(in)`. Production reaches the same numbers
//! through `CompiledMask` and the host's colour run; the two meet only in the assertions here.
//!
//! Numerical rule, the one the delivered colour studies state: a rendered code equals the reference's
//! code exactly, except where the reference's linear value sits within `1e-6 + 1e-6·|threshold|` of a
//! code threshold, where one code of difference is permitted because production decodes, multiplies
//! and blends in `f32`. The two **endpoints** of the blend carry no tolerance at all: `M = 0` is the
//! unmasked input frame byte for byte and `M = 1` is the unmasked effect's frame byte for byte.

use super::*;
use lightwell_core::{Layer, LinearSettings, ModuleRegistry, SnapshotId};
use lightwell_reference::mask::{Algebra, Mask as RefMask, Stage as RefStage, blend, coverage};
use lightwell_reference::{linear_to_srgb_code, srgb_to_linear};
use lightwell_testkit::fixtures::{render, render_linear, sample, sample_linear};
use serde_json::json;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

fn exposure_layer(ev: f64) -> Layer {
    basic_layer(json!({ "exposure": ev }))
}

/// The reference's coverage at one content-stage pixel centre, through the frozen algebra.
fn reference_coverage(oracle: &RefMask, x: u32, y: u32) -> f64 {
    let stage = RefStage::new(WIDTH, HEIGHT);
    let (u, v) = stage.pixel_uv(x, y);
    coverage(oracle, Algebra::Zadeh, &stage, u, v)
}

// ---------------------------------------------------------------------------------------------
// The two endpoints, byte for byte, on both paths
// ---------------------------------------------------------------------------------------------

/// `M = 0` everywhere renders the unmasked **input**, and `M = 1` everywhere renders the unmasked
/// **effect** — byte for byte, on the JPEG byte path and on the RAW linear path. Three spellings of
/// the endpoints are checked because they take different routes through the primitive: an `amount` of
/// zero empties the bounds rectangle so no unit is evaluated at all, a gradient whose frame lies
/// entirely behind `p0` is evaluated and blended with `M = 0`, and one entirely beyond `p1` is
/// blended with `M = 1`.
#[test]
fn the_mask_endpoints_are_byte_identical_to_the_unmasked_frames_on_both_paths() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let linear = decoded(&byte_source());
    let identity = recipe(Vec::new(), Vec::new());
    let unmasked = recipe(vec![exposure_layer(1.5)], Vec::new());

    let (silent, _) = gradient_mask(0.5, 0.0, 0.5, 1.0, 0.0);
    let (behind, oracle_behind) = gradient_mask(0.5, 1.5, 0.5, 2.0, 100.0);
    let (ahead, oracle_ahead) = gradient_mask(0.5, -1.0, 0.5, -0.5, 100.0);
    // The oracle agrees that these are the endpoints, so the byte comparison below is a claim about
    // the blend and not about the geometry.
    for (x, y) in [(0, 0), (WIDTH / 2, HEIGHT / 2), (WIDTH - 1, HEIGHT - 1)] {
        assert_eq!(reference_coverage(&oracle_behind, x, y), 0.0);
        assert_eq!(reference_coverage(&oracle_ahead, x, y), 1.0);
    }

    for (case, mask, expected) in [
        ("amount zero", silent, &identity),
        ("behind p0", behind, &identity),
        ("beyond p1", ahead, &unmasked),
    ] {
        let stack = recipe(vec![masked(exposure_layer(1.5), &mask)], vec![mask.clone()]);
        let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
        let reference_frame = render(&registry, &source, SnapshotId::new(), expected).unwrap();
        assert_eq!(
            rendered.rgba.as_ref(),
            reference_frame.rgba.as_ref(),
            "{case}: the JPEG path is not byte-identical"
        );
        let rendered_linear = render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            &stack,
            LinearSettings::default(),
        )
        .unwrap();
        let reference_linear = render_linear(
            &registry,
            &linear,
            SnapshotId::new(),
            expected,
            LinearSettings::default(),
        )
        .unwrap();
        assert_eq!(
            rendered_linear.rgba.as_ref(),
            reference_linear.rgba.as_ref(),
            "{case}: the linear path is not byte-identical"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// A half-covered frame against the stepwise reference
// ---------------------------------------------------------------------------------------------

/// A feathered gradient over the whole frame, with an unmasked operation before the masked one,
/// against a stepwise `f64` evaluation: decode, the global exposure everywhere, the masked exposure
/// blended against its own input at the reference's coverage, and one quantization at the end.
/// Nothing is clamped or quantized in between, on either path. That an *unmasked* operation after the
/// masked one is equally unaffected is asserted in `render.rs`, where a test module may hold three
/// colour layers of one effect.
#[test]
fn a_half_covered_frame_matches_the_stepwise_reference_on_both_paths() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let linear = decoded(&byte_source());
    let (mask, oracle) = gradient_mask(0.2, 0.15, 0.8, 0.85, 80.0);
    // One global Basic layer and one bound to the mask: two layers of one single-layer effect are
    // legal because they are two targets, and the masked one follows the global one.
    let stack = recipe(
        vec![exposure_layer(0.5), masked(exposure_layer(2.0), &mask)],
        vec![mask.clone()],
    );
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
    let rendered_linear = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &stack,
        LinearSettings::default(),
    )
    .unwrap();
    let mut partial = 0;
    let mut worst = 0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let m = reference_coverage(&oracle, x, y);
            if m > 0.0 && m < 1.0 {
                partial += 1;
            }
            let offset = ((y * WIDTH + x) * 4) as usize;
            for channel in 0..3 {
                let input = srgb_to_linear(source.rgba[offset + channel]) * 2.0_f64.powf(0.5);
                let effect = input * 2.0_f64.powf(2.0);
                let blended = blend(input, effect, m);
                let expected = linear_to_srgb_code(blended);
                for (path, pixel) in [
                    ("jpeg", rendered.pixel(x, y).unwrap()),
                    ("linear", rendered_linear.pixel(x, y).unwrap()),
                ] {
                    assert_code(
                        pixel[channel],
                        expected,
                        blended,
                        &format!("{path} ({x}, {y}) channel {channel}"),
                    );
                    worst =
                        worst.max((i32::from(pixel[channel]) - i32::from(expected)).unsigned_abs());
                }
            }
        }
    }
    // The fixture must actually be half covered: an endpoint-only frame would prove nothing here.
    let pixels = (WIDTH * HEIGHT) as usize;
    assert!(
        partial > pixels / 2,
        "only {partial} of {pixels} pixels were partially covered"
    );
    // Printed with --nocapture so a handoff can quote the measured figure.
    println!(
        "masked blend: maximum observed deviation {worst} output code(s) over {pixels} pixels"
    );
}

// ---------------------------------------------------------------------------------------------
// Sample equals render
// ---------------------------------------------------------------------------------------------

/// `render.sample` equals the rendered byte at **every** pixel of a masked fixture, on both paths —
/// inside the feather band, at the bounds edge and outside it — because both reach the coverage
/// through the one shared call inside the colour run.
#[test]
fn a_masked_sample_equals_the_rendered_byte_at_every_pixel_on_both_paths() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let linear = decoded(&byte_source());
    // A gradient over part of the frame, so the frame holds a feather band, a fully covered region
    // and a region outside the bounds rectangle.
    let (mask, oracle) = gradient_mask(0.5, 0.35, 0.5, 0.65, 100.0);
    let stack = recipe(
        vec![masked(
            basic_layer(json!({"exposure": 1.25, "vibrance": 30.0})),
            &mask,
        )],
        vec![mask.clone()],
    );
    let rendered = render(&registry, &source, SnapshotId::new(), &stack).unwrap();
    let rendered_linear = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &stack,
        LinearSettings::default(),
    )
    .unwrap();
    let mut bands = [0usize; 3];
    for y in 0..HEIGHT {
        let m = reference_coverage(&oracle, 0, y);
        bands[if m == 0.0 {
            0
        } else if m == 1.0 {
            1
        } else {
            2
        }] += 1;
        for x in 0..WIDTH {
            assert_eq!(
                sample(&registry, &source, &stack, x, y).unwrap().rgba,
                rendered.pixel(x, y),
                "jpeg: sample disagreed at ({x}, {y})"
            );
            assert_eq!(
                sample_linear(&registry, &linear, &stack, LinearSettings::default(), x, y)
                    .unwrap()
                    .rgba,
                rendered_linear.pixel(x, y),
                "linear: sample disagreed at ({x}, {y})"
            );
        }
    }
    // The fixture covered all three cases the claim names.
    assert!(bands[0] > 0, "no row was outside the mask");
    assert!(bands[1] > 0, "no row was fully covered");
    assert!(bands[2] > 0, "no row was inside the feather band");
}

/// A mask travels through the geometry tail on **both** paths: the same quarter turn over the same
/// masked layer changes the same output pixels on the byte path and on the linear path, and each path's
/// sample still equals its own rendered byte. Both reach the mask through the same suffix mapping
/// inside the colour run, and this is what would fail if either read the mask at the turned frame's
/// coordinates instead of the content stage's.
#[test]
fn a_mask_travels_through_a_quarter_turn_on_both_paths() {
    let registry = ModuleRegistry::builtin();
    let source = byte_source();
    let linear = decoded(&byte_source());
    let (mask, _) = gradient_mask(0.5, 0.3, 0.5, 0.7, 100.0);
    let layer = masked(exposure_layer(1.5), &mask);
    let turn = Layer::orientation(lightwell_core::Orientation::of(
        lightwell_core::Transform::RotateRight,
    ));
    let masked_stack = recipe(vec![layer, turn.clone()], vec![mask.clone()]);
    let plain_stack = recipe(vec![turn], Vec::new());
    // Which output pixels the mask changed, per path, against the same stack without the layer.
    let changed = |masked_frame: &lightwell_core::Raster, plain: &lightwell_core::Raster| {
        assert_eq!(masked_frame.width, plain.width);
        let mut set = Vec::new();
        for y in 0..masked_frame.height {
            for x in 0..masked_frame.width {
                if masked_frame.pixel(x, y) != plain.pixel(x, y) {
                    set.push((x, y));
                }
            }
        }
        set
    };
    let byte_masked = render(&registry, &source, SnapshotId::new(), &masked_stack).unwrap();
    let byte_plain = render(&registry, &source, SnapshotId::new(), &plain_stack).unwrap();
    let linear_masked = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &masked_stack,
        LinearSettings::default(),
    )
    .unwrap();
    let linear_plain = render_linear(
        &registry,
        &linear,
        SnapshotId::new(),
        &plain_stack,
        LinearSettings::default(),
    )
    .unwrap();
    let byte_changed = changed(&byte_masked, &byte_plain);
    let linear_changed = changed(&linear_masked, &linear_plain);
    assert!(
        !byte_changed.is_empty(),
        "the masked layer changed nothing at all"
    );
    assert_eq!(
        byte_changed, linear_changed,
        "the two paths placed the mask on different output pixels"
    );
    // The gradient runs down the content stage, so its band lies across the content's rows; the turn
    // swaps the stage's sides, so in the turned frame that band must lie across its *columns*. A mask
    // read at the turned frame's own coordinates would produce the other band, which is what the two
    // spans below distinguish. The spans are of the pixels whose byte actually changed, which is a
    // subset of the covered ones — a small exposure change rounds to the same code in places — so they
    // are compared with each other rather than with the frame's sides.
    let span = |values: Vec<u32>| {
        values.iter().copied().max().unwrap() - values.iter().copied().min().unwrap() + 1
    };
    let rows = span(byte_changed.iter().map(|(_, y)| *y).collect());
    let columns = span(byte_changed.iter().map(|(x, _)| *x).collect());
    assert_eq!(byte_masked.width, HEIGHT, "the turn swapped the sides");
    assert!(
        columns < byte_masked.width,
        "the band spans the turned frame's whole width, so it was read before the turn"
    );
    assert!(
        rows > columns,
        "the band ({rows} rows by {columns} columns) does not run along the turned frame's rows"
    );
    for y in 0..byte_masked.height {
        for x in 0..byte_masked.width {
            assert_eq!(
                sample(&registry, &source, &masked_stack, x, y)
                    .unwrap()
                    .rgba,
                byte_masked.pixel(x, y),
                "jpeg: sample disagreed at ({x}, {y})"
            );
            assert_eq!(
                sample_linear(
                    &registry,
                    &linear,
                    &masked_stack,
                    LinearSettings::default(),
                    x,
                    y
                )
                .unwrap()
                .rgba,
                linear_masked.pixel(x, y),
                "linear: sample disagreed at ({x}, {y})"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------------------------

/// A client discovers the target field and the effects it applies to without a hand-maintained list:
/// `schema.list` lists `mask` among the optional fields of every action of a maskable effect and of no
/// other action, and `modules` marks the maskable effects.
#[test]
fn the_schema_lists_the_mask_field_on_exactly_the_maskable_actions() {
    let schema = lightwell_core::schemas(&ModuleRegistry::builtin());
    let methods = schema["methods"].as_object().unwrap();
    for method in [
        "edit.set-basic",
        "edit.reset-basic",
        "edit.set-mixer",
        "edit.reset-mixer",
        "edit.set-presence",
        "edit.reset-presence",
    ] {
        let optional = methods[method]["optional"].as_object().unwrap();
        assert!(
            optional.contains_key("mask"),
            "{method} does not list the mask field"
        );
    }
    for method in [
        "edit.set-pixel",
        "edit.crop",
        "edit.transform",
        "edit.set-vignette",
        "history.undo",
    ] {
        let optional = methods[method]["optional"].as_object().unwrap();
        assert!(
            !optional.contains_key("mask"),
            "{method} lists a mask field it does not accept"
        );
    }
    // And the effects say which of them a mask may reach.
    let maskable: Vec<String> = schema["modules"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|module| module["effects"].as_array().unwrap())
        .filter(|effect| effect["maskable"] == json!(true))
        .map(|effect| effect["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        maskable,
        vec![
            "lightwell.basic.adjust".to_owned(),
            "lightwell.presence.adjust".to_owned(),
            "lightwell.mixer.hsl".to_owned(),
        ],
        "exactly the three effects the design names, in registration order"
    );
}
