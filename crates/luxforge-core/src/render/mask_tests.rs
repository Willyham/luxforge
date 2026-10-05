//! The masked colour primitive: its blend, its bounds, the pixels it skips and its cost.

use super::colour_runs::{NON_FINITE_COLOR, UnitSkip, set_unit_skip};
use super::testing::{frame_in, linear, point_evaluated, render, sample, sample_in};
use super::tests::*;
use super::*;
use crate::{
    Error, Layer, Recipe, SnapshotId, SourceImage, Transform,
    colour::srgb::decode_pixel,
    modules::{ModuleRegistry, Stage},
};
use serde_json::json;
use std::sync::Arc;

/// The pixel value a geometric component is handed and ignores (proposal P12 of
/// `docs/design/range-study.md`): the masks here hold gradients and their coverage is a
/// function of position alone, so the value is arbitrary and the same at every call.
const ANY_PIXEL: [f64; 3] = [0.25, 0.5, 0.75];
const ANY_PIXEL_F32: [f32; 3] = [0.25, 0.5, 0.75];

/// A colour registry whose test module counts the pixels its `counting` unit was handed.
fn counting_registry(counter: Arc<std::sync::atomic::AtomicUsize>) -> ModuleRegistry {
    let mut registry = geometry_registry();
    registry
        .register(ColorTestModule::with_counter(Some(counter)))
        .unwrap();
    registry
}

/// One rendered frame used as the source of another render: the reference for "the mask travelled
/// with the picture" turns an already masked frame with the delivered exact pass.
fn rendered_from(raster: &Raster, layers: Vec<Layer>) -> Raster {
    let source = SourceImage {
        width: raster.width,
        height: raster.height,
        rgba: raster.rgba.clone(),
        fingerprint: "sha256:rendered-again".into(),
        orientation: 1,
        capture: Default::default(),
    };
    render(
        &colour_registry(),
        &source,
        SnapshotId::new(),
        &colour_recipe(layers),
    )
    .unwrap()
}

/// One mask of one `add` linear gradient, at full amount: `p0` at coverage 0 and `p1` at 1, both
/// as normalized content-stage positions.
fn linear_mask(x0: f64, y0: f64, x1: f64, y1: f64) -> crate::Mask {
    let mut mask = crate::Mask::new("Mask 1");
    mask.components = vec![crate::Component::new(
        "Linear 1",
        crate::ComponentMode::Add,
        "linear",
        json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1}),
    )];
    mask
}

fn masked(layer: Layer, mask: &crate::Mask) -> Layer {
    Layer {
        mask: Some(mask.id.clone()),
        ..layer
    }
}

fn masked_recipe(layers: Vec<Layer>, masks: Vec<crate::Mask>) -> Recipe {
    Recipe {
        format: crate::RECIPE_FORMAT,
        layers,
        masks,
        ..Recipe::default()
    }
}

/// The two endpoints of the blend, byte for byte and not approximately: coverage of exactly zero
/// everywhere renders the unmasked input, and coverage of exactly one everywhere renders the
/// unmasked effect. Two spellings of zero are checked, because they take different paths: an
/// `amount` of zero empties the bounds rectangle so nothing is evaluated at all, while a gradient
/// whose frame lies entirely behind `p0` is evaluated and blended with `M = 0`.
#[test]
fn the_endpoints_of_the_mask_are_byte_identical_to_the_unmasked_frames() {
    let registry = colour_registry();
    let source = gradient(37, 23);
    let identity = render(
        &registry,
        &source,
        SnapshotId::new(),
        &colour_recipe(vec![]),
    )
    .unwrap();
    let exposed = render(
        &registry,
        &source,
        SnapshotId::new(),
        &colour_recipe(vec![exposure_layer(&[1.5])]),
    )
    .unwrap();
    // M = 0 by amount: the rectangle is empty, so no unit runs anywhere.
    let mut silent = linear_mask(0.5, 0.0, 0.5, 1.0);
    silent.amount = 0.0;
    // M = 0 by geometry: the whole frame sits behind p0, which is below the frame.
    let behind = linear_mask(0.5, 1.5, 0.5, 2.0);
    // M = 1 everywhere: the whole frame sits beyond p1, which is above it.
    let ahead = linear_mask(0.5, -1.0, 0.5, -0.5);
    for (case, mask, expected) in [
        ("amount zero", silent, &identity),
        ("behind p0", behind, &identity),
        ("beyond p1", ahead, &exposed),
    ] {
        let recipe = masked_recipe(
            vec![masked(exposure_layer(&[1.5]), &mask)],
            vec![mask.clone()],
        );
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        assert_eq!(
            raster.rgba.as_ref(),
            expected.rgba.as_ref(),
            "{case}: the masked frame is not byte-identical"
        );
        // And the sampled byte is the rendered byte at every pixel of both endpoints.
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(
                    sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                    raster.pixel(x, y),
                    "{case}: sample disagrees at ({x}, {y})"
                );
            }
        }
    }
}

/// A half-covered frame against an independent stepwise evaluation, and the property that makes
/// the blend a *masked operation* rather than a masked run: the operations before and after the
/// masked one apply everywhere, nothing is clamped or quantized between them, and the masked one
/// is blended against the value it was handed.
#[test]
fn a_masked_operation_blends_against_its_own_input_inside_the_run() {
    let registry = colour_registry();
    // Every byte once at exactly half coverage — the 256×1 strip's one row sits at `v = 0.5` of a
    // gradient from `v = 0` to `v = 1`, where `smooth(0.5)` is exactly `0.5` — and then a frame
    // whose coverage varies row by row through the whole feather band.
    for (case, source) in [
        ("every byte at M = 0.5", greys()),
        ("a feather band", gradient(64, 48)),
    ] {
        let mask = linear_mask(0.5, 0.0, 0.5, 1.0);
        let recipe = masked_recipe(
            vec![
                exposure_layer(&[0.5]),
                masked(exposure_layer(&[2.0]), &mask),
                exposure_layer(&[-0.25]),
            ],
            vec![mask.clone()],
        );
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        let compiled = crate::mask::CompiledMask::new(
            &mask,
            Stage {
                width: source.width,
                height: source.height,
            },
            &crate::path::StrokeTable::default(),
        )
        .unwrap();
        let mut partial = 0;
        for y in 0..raster.height {
            let coverage = f64::from(compiled.evaluate(0, y, ANY_PIXEL_F32));
            if coverage > 0.0 && coverage < 1.0 {
                partial += 1;
            }
            for x in 0..raster.width {
                let byte = source.rgba[((y * source.width + x) * 4) as usize];
                // The stepwise f64 reference: decode, the first operation everywhere, the masked
                // one blended against its own input, the last one everywhere, one quantization.
                let input = decode_reference(f64::from(byte) / 255.0) * 2.0_f64.powf(0.5);
                let effect = input * 2.0_f64.powf(2.0);
                let coverage = f64::from(compiled.evaluate(x, y, ANY_PIXEL_F32));
                let blended = (1.0 - coverage) * input + coverage * effect;
                let clamped = (blended * 2.0_f64.powf(-0.25)).clamp(0.0, 1.0);
                let expected = (255.0 * encode_reference(clamped) + 0.5).floor() as u8;
                let actual = raster.pixel(x, y).unwrap()[0];
                assert_code_within_tolerance(
                    actual,
                    expected,
                    clamped,
                    &format!("{case} at ({x}, {y})"),
                );
                assert_eq!(
                    sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                    raster.pixel(x, y),
                    "{case}: sample disagrees at ({x}, {y})"
                );
            }
        }
        // The coverage the fixture exercised is genuinely partial, not one of the endpoints.
        assert_eq!(
            partial, raster.height as usize,
            "{case}: every row of this fixture must be partially covered"
        );
        if case == "every byte at M = 0.5" {
            assert_eq!(
                compiled.coverage(0, 0, ANY_PIXEL),
                0.5,
                "the strip's own coverage"
            );
        }
    }
}

/// What `evaluate` returns under `skip` on this thread, with every pass this thread asks for forced
/// serial so the rule reaches every row it runs; both are returned to what they were afterwards.
fn with_skip<T>(skip: UnitSkip, evaluate: impl FnOnce() -> T) -> T {
    let skips = set_unit_skip(skip);
    let forced = parallel::force(Some(false));
    let result = evaluate();
    parallel::force(forced);
    set_unit_skip(skips);
    result
}

/// Outside the bounds rectangle a masked operation evaluates **no unit at all**, and inside it none
/// at a pixel whose coverage is zero, both counted. The same test states the rectangle's own
/// conservatism: it is at most one pixel larger on each side than the rows whose coverage is
/// non-zero, which is what the rule before the skip still ran the unit over.
#[test]
fn a_masked_operation_evaluates_no_unit_outside_its_bounds() {
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = counting_registry(counter.clone());
    let source = gradient(200, 100);
    let stage = Stage {
        width: source.width,
        height: source.height,
    };
    // A gradient over the bottom tenth of the frame: p0 at v = 0.9, p1 at v = 1.0.
    let mask = linear_mask(0.5, 0.9, 0.5, 1.0);
    let recipe = masked_recipe(
        vec![masked(colour_layer(json!({"counting": true})), &mask)],
        vec![mask.clone()],
    );
    let count = |skip: UnitSkip| {
        with_skip(skip, || {
            counter.store(0, std::sync::atomic::Ordering::Relaxed);
            let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
            (raster, counter.load(std::sync::atomic::Ordering::Relaxed))
        })
    };
    let (raster, counted) = count(UnitSkip::Proved);
    let (everywhere, before) = count(UnitSkip::Never);
    assert_eq!(
        raster.rgba, everywhere.rgba,
        "skipping uncovered pixels changed the frame"
    );
    let compiled =
        crate::mask::CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default()).unwrap();
    let bounds = compiled.bounds();
    assert_eq!(
        before as u64,
        bounds.pixels(),
        "without the skip the unit ran over {before} pixels against a {}x{} rectangle",
        bounds.width,
        bounds.height
    );
    let covered = (0..source.height)
        .flat_map(|y| (0..source.width).map(move |x| (x, y)))
        .filter(|&(x, y)| compiled.evaluate(x, y, ANY_PIXEL_F32) != 0.0)
        .count();
    assert_eq!(
        counted, covered,
        "the unit ran over {counted} pixels, and {covered} have coverage"
    );
    // What the skip saves here is the rectangle's conservatism: at most a row on each side.
    assert!(
        (covered as u64) < bounds.pixels() && bounds.pixels() - covered as u64 <= 2 * 200,
        "{covered} covered pixels in a rectangle of {}",
        bounds.pixels()
    );
    // The frame is 20000 pixels and the rectangle is a small part of it: the saving is the point.
    assert!(
        (counted as u64) * 8 < u64::from(source.width) * u64::from(source.height),
        "{counted} pixels is not a small part of the frame"
    );
    // Every pixel the unit did not touch kept its input byte exactly, and the rows it did touch
    // are the rows with coverage.
    for y in 0..raster.height {
        let touched = y >= bounds.y0 && y < bounds.y1();
        let green = raster.pixel(0, y).unwrap()[1];
        let input = source.rgba[((y * source.width) * 4 + 1) as usize];
        if !touched {
            assert_eq!(green, input, "row {y} was outside the rectangle");
            assert_eq!(
                compiled.evaluate(0, y, ANY_PIXEL_F32),
                0.0,
                "row {y} has coverage"
            );
        }
    }
    // And a point sample agrees with the frame inside the feather band and at both bounds edges.
    for y in [
        0,
        bounds.y0.saturating_sub(1),
        bounds.y0,
        bounds.y0 + 1,
        95,
        raster.height - 1,
    ] {
        for x in [0, 1, raster.width / 2, raster.width - 1] {
            assert_eq!(
                sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                raster.pixel(x, y),
                "sample disagrees at ({x}, {y})"
            );
        }
    }
}

/// A mask is stored in content-stage coordinates, so it travels through the geometry tail with
/// the picture: masking a colour layer and then turning the frame renders the turn of the masked
/// frame, exactly. This is what the suffix mapping inside the blend exists for — without it the
/// mask would be read at the turned frame's coordinates.
#[test]
fn a_mask_lands_on_the_same_content_pixels_through_the_geometry_tail() {
    let registry = colour_registry();
    let source = gradient(24, 16);
    let mask = linear_mask(0.25, 0.25, 0.75, 0.75);
    let masked_layer = masked(exposure_layer(&[1.0]), &mask);
    let flat = render(
        &registry,
        &source,
        SnapshotId::new(),
        &masked_recipe(vec![masked_layer.clone()], vec![mask.clone()]),
    )
    .unwrap();
    for transform in [
        Transform::RotateRight,
        Transform::RotateLeft,
        Transform::MirrorHorizontal,
        Transform::FlipVertical,
    ] {
        // The masked colour layer first, then the turn: the host places a colour layer before the
        // geometry tail, so this is the order a commit produces.
        let turned = render(
            &registry,
            &source,
            SnapshotId::new(),
            &masked_recipe(
                vec![masked_layer.clone(), turn(transform)],
                vec![mask.clone()],
            ),
        )
        .unwrap();
        // The reference: turn the masked frame with the delivered exact pass.
        let expected = rendered_from(&flat, vec![turn(transform)]);
        assert_eq!(turned.width, expected.width, "{transform:?}");
        assert_eq!(
            turned.rgba.as_ref(),
            expected.rgba.as_ref(),
            "{transform:?}: the mask did not travel with the picture"
        );
        let recipe = masked_recipe(
            vec![masked_layer.clone(), turn(transform)],
            vec![mask.clone()],
        );
        for y in 0..turned.height {
            for x in 0..turned.width {
                assert_eq!(
                    sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                    turned.pixel(x, y),
                    "{transform:?}: sample disagrees at ({x}, {y})"
                );
            }
        }
    }
}

/// The snapshot block a masked operation is processed in is scratch, not arithmetic: the units are
/// pointwise, so the same row blended in blocks of 1, 3 and the whole row is the same row.
#[test]
fn the_masked_blend_does_not_depend_on_the_snapshot_block_size() {
    let registry = colour_registry();
    let source = gradient(64, 5);
    let mask = linear_mask(0.1, 0.2, 0.9, 0.8);
    let recipe = masked_recipe(
        vec![
            exposure_layer(&[0.75]),
            masked(exposure_layer(&[-1.5]), &mask),
        ],
        vec![mask],
    );
    let compiled = registry
        .compile(source.width, source.height, &recipe)
        .unwrap();
    let segment = compiled.segments.last().unwrap();
    let row = |block: usize| -> Vec<[f32; 3]> {
        let mut pixels: Vec<[f32; 3]> = (0..source.width)
            .map(|x| decode_pixel([x as u8, (x as u8).wrapping_add(20), 0]))
            .collect();
        let mut scratch = vec![MaskedInput::default(); block];
        for run in color_runs(segment) {
            apply_units(&run, 2, 0, &mut pixels, &mut scratch).unwrap();
        }
        pixels
    };
    let whole = row(source.width as usize);
    for block in [1, 3, 7, 64] {
        assert_eq!(row(block), whole, "block of {block}");
    }
}

/// A neutral payload compiles to no units whether it carries a mask or not, so the segment keeps
/// the identity byte path and the shared source buffer: masking nothing is nothing.
#[test]
fn a_neutral_masked_layer_keeps_the_identity_byte_path() {
    let registry = colour_registry();
    let source = gradient(16, 9);
    let mask = linear_mask(0.5, 0.0, 0.5, 1.0);
    let recipe = masked_recipe(vec![masked(exposure_layer(&[]), &mask)], vec![mask.clone()]);
    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
    assert!(
        Arc::ptr_eq(&raster.rgba, &source.rgba),
        "a neutral masked layer allocated a frame"
    );
    let compiled = registry
        .compile(source.width, source.height, &recipe)
        .unwrap();
    assert!(compiled.segments.last().unwrap().operations.is_empty());
}

/// Masked scratch is bounded by the existing float budget and is taken only when a run needs it:
/// an unmasked pass reserves what it always reserved.
#[test]
fn a_masked_run_reserves_its_row_snapshot_from_the_existing_budget() {
    let registry = colour_registry();
    let source = gradient(64, 48);
    let mask = linear_mask(0.5, 0.0, 0.5, 1.0);
    let recipe = masked_recipe(
        vec![masked(exposure_layer(&[1.0]), &mask)],
        vec![mask.clone()],
    );
    // One row chunk plus one row of snapshot, each pixel's input and its coverage. The budget is a
    // **target and not a limit**, so a target that fits the chunk but not the snapshot renders
    // anyway rather than refusing: what the snapshot costs is visible in the high-water mark, never
    // in an error. Below the parallel threshold the chunks run one after another, so each
    // context's high-water mark is exactly what one chunk of its render carried.
    let chunk = (color_chunk_rows(source.width) * source.width as usize * 12) as u64;
    let snapshot = source.width as u64 * 16;
    assert_eq!(std::mem::size_of::<MaskedInput>(), 16);
    let render_at = |context: &RenderContext, recipe: &Recipe| {
        frame_in(
            context,
            &registry,
            &source,
            SnapshotId::new(),
            recipe,
            RenderOptions::default(),
        )
    };
    let masked = RenderContext::with_scratch_target(chunk);
    let rendered = render_at(&masked, &recipe);
    assert!(
        rendered.is_ok(),
        "a masked run past the target still renders: {:?}",
        rendered.err()
    );
    assert_eq!(
        masked.scratch().peak(),
        chunk + snapshot,
        "a masked pass carries one chunk and one row of snapshot at once"
    );
    assert_eq!(masked.scratch().in_use(), 0, "and releases both");
    // The unmasked stack renders inside a target that holds only the chunk without ever
    // overshooting it, so the snapshot is charged to masked runs alone.
    let unmasked = colour_recipe(vec![exposure_layer(&[1.0])]);
    let plain = RenderContext::with_scratch_target(chunk);
    let rendered = render_at(&plain, &unmasked);
    assert!(rendered.is_ok(), "{:?}", rendered.err());
    assert_eq!(
        plain.scratch().peak(),
        chunk,
        "an unmasked pass takes no snapshot"
    );
    // A point sample streams nothing and allocates no snapshot, so it answers at any target.
    let empty = RenderContext::with_scratch_target(0);
    let sampled = sample_in(
        &empty,
        &registry,
        &source,
        &recipe,
        RenderOptions::default(),
        3,
        3,
    )
    .unwrap();
    assert!(sampled.rgba.is_some());
    assert_eq!(empty.scratch().peak(), 0, "a sample reserves no scratch");
}

/// One mask of one `add` luminance band, at full amount, on the histogram's `0..100` axis. Its
/// rectangle is the whole stage, whatever it selects.
fn band_mask(low: f64, low_feather: f64, high: f64, high_feather: f64) -> crate::Mask {
    let mut mask = crate::Mask::new("Mask 1");
    mask.components = vec![crate::Component::new(
        "Luminance range 1",
        crate::ComponentMode::Add,
        "luminance-range",
        json!({"low": low, "low_feather": low_feather, "high": high, "high_feather": high_feather}),
    )];
    mask
}

/// Basic with every field non-neutral: the most per-pixel work one colour layer asks for.
fn full_basic() -> Layer {
    Layer::new(
        crate::BASIC_EFFECT,
        json!({
            "exposure": 0.5, "contrast": 25.0, "highlights": -30.0, "shadows": 30.0,
            "whites": -15.0, "blacks": 15.0, "temperature": 20.0, "tint": -10.0,
            "vibrance": 30.0, "saturation": 15.0,
        }),
    )
}

/// The masks under which a pixel the bounds rectangle admits can still have coverage of exactly
/// zero, each with the stroke table its brush reads: a luminance band and a colour range, whose
/// rectangles are always the whole stage; an inverted radial, which its inversion gives the whole
/// stage too; an inverted radial subtracted from a gradient over the whole frame; a diagonal
/// gradient, a hard-edged radial and a brush stroke, whose rectangles hold corners their fields
/// leave at zero; a gradient intersected with a band; a band at a partial amount; and a gradient
/// whose coverage is exactly one over the whole frame, which leaves nothing to skip. Positions are
/// fractions of the stage and a radius a fraction of its height. Every radial draws a hard edge, a
/// feature no pixel grid resolves, so the proxy phase supersamples the masks that hold one.
fn uncovered_masks() -> Vec<(&'static str, crate::Mask, crate::path::StrokeTable)> {
    use crate::{Component, ComponentMode, Mask};
    let radial = |x: f64, y: f64, radius: f64| {
        json!({"x": x, "y": y, "radius_x": radius, "radius_y": radius, "angle": 0.0,
               "feather": 0.0})
    };
    let whole = json!({"x0": -1.0, "y0": 0.5, "x1": -0.5, "y1": 0.5});
    let band = band_mask(40.0, 10.0, 70.0, 0.0);
    let mut colours = Mask::new("Colours");
    colours.components.push(Component::new(
        "Colour range 1",
        ComponentMode::Add,
        "colour-range",
        json!({"samples": [[0.6, 0.25, 0.1]], "refine": 40.0}),
    ));
    let mut inverted = Mask::new("Inverted radial");
    inverted.components.push(Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        radial(0.6, 0.5, 0.3),
    ));
    inverted.invert = true;
    let mut subtracted = Mask::new("Subtracted radial");
    subtracted.components.push(Component::new(
        "Linear 1",
        ComponentMode::Add,
        "linear",
        whole.clone(),
    ));
    let mut hole = Component::new(
        "Radial 1",
        ComponentMode::Subtract,
        "radial",
        radial(0.4, 0.5, 0.2),
    );
    // Inverted, the radial covers everything but its core; subtracting that leaves the core.
    hole.invert = true;
    subtracted.components.push(hole);
    subtracted.amount = 60.0;
    let diagonal = linear_mask(0.45, 0.45, 0.15, 0.15);
    let mut hard = Mask::new("Hard radial");
    hard.components.push(Component::new(
        "Radial 1",
        ComponentMode::Add,
        "radial",
        radial(0.5, 0.5, 0.3),
    ));
    let stroke = crate::mask::Stroke::capture(
        &[[0.2, 0.3], [0.5, 0.65], [0.8, 0.4]],
        0.15,
        50.0,
        100.0,
        false,
    )
    .expect("a legal stroke");
    let (brush, strokes) = brush_mask(std::slice::from_ref(&stroke));
    let mut intersected = Mask::new("Gradient and band");
    intersected.components.push(Component::new(
        "Linear 1",
        ComponentMode::Add,
        "linear",
        whole,
    ));
    intersected.components.push(Component::new(
        "Luminance range 1",
        ComponentMode::Intersect,
        "luminance-range",
        json!({"low": 40.0, "low_feather": 10.0, "high": 70.0, "high_feather": 0.0}),
    ));
    let mut partial = band.clone();
    partial.amount = 60.0;
    let none = crate::path::StrokeTable::default;
    vec![
        ("band", band, none()),
        ("colour range", colours, none()),
        ("inverted radial", inverted, none()),
        ("inverted radial subtracted", subtracted, none()),
        ("diagonal gradient", diagonal, none()),
        ("hard-edged radial", hard, none()),
        ("brush", brush, strokes),
        ("gradient intersected with a band", intersected, none()),
        ("band at a partial amount", partial, none()),
        ("covering", linear_mask(0.5, -1.0, 0.5, -0.5), none()),
    ]
}

/// One recipe of `layers` and `mask`, with the stroke table its brush reads.
fn recipe_with(
    layers: Vec<Layer>,
    mask: &crate::Mask,
    strokes: &crate::path::StrokeTable,
) -> Recipe {
    Recipe {
        format: crate::RECIPE_FORMAT,
        layers,
        masks: vec![mask.clone()],
        strokes: strokes.clone(),
        ..Recipe::default()
    }
}

/// Whether a pixel of this input keeps it where its coverage is zero: every channel finite and
/// none `−0.0`. The independent statement of the rule the tests count against.
fn clean(input: [f32; 3]) -> bool {
    input
        .iter()
        .all(|value| value.is_finite() && value.to_bits() != (-0.0_f32).to_bits())
}

/// A pixel the bounds rectangle admits whose coverage is exactly zero runs no unit, and that changes
/// no bit. [`apply_units`] is compared with itself, the skip on and off, over every row of a small
/// stage, under every mask that leaves such pixels, through three stacks: the masked operation
/// between two unmasked ones in one run, a position-dependent unit inside it, and a whole Basic
/// layer. The rows hold what a linear source can — negative values, values past white and both
/// zeros — and one row holds `−0.0`, the value the skip excludes, while another holds a value that
/// is not a number, whose verdict must come out unchanged too. Each row is evaluated whole and as
/// a tail starting at a later column, in snapshot blocks of 1, 3 and the whole row, so a stretch
/// that crosses a block or starts past the slice's first column still hands its units and its mask
/// the coordinates the whole row does. A counting unit shows the skip is not vacuous: with it on,
/// the units run at exactly the pixels the rectangle admits that do not keep their input; with it
/// off, at every pixel the rectangle admits.
#[test]
fn a_pixel_whose_coverage_is_zero_runs_no_unit_and_keeps_its_bits() {
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = counting_registry(counter.clone());
    let (width, height) = (53_u32, 23_u32);
    let (negative_zeros, not_a_number) = (5_u32, 11_u32);
    let row = |y: u32| -> Vec<[f32; 3]> {
        (0..width)
            .map(|x| {
                let mut pixel = [0_u32, 1, 2].map(|channel| {
                    ((x * 37 + y * 101 + channel * 53) % 97) as f32 / 97.0 * 1.6 - 0.2
                });
                if (x + y).is_multiple_of(9) {
                    pixel = [0.0; 3];
                }
                if y == negative_zeros && x.is_multiple_of(2) {
                    pixel[(x / 2 % 3) as usize] = -0.0;
                }
                if y == not_a_number && x == 31 {
                    pixel[2] = f32::NAN;
                }
                pixel
            })
            .collect()
    };
    for (name, mask, strokes) in uncovered_masks() {
        for (stack, layers) in [
            (
                "inside a run",
                vec![
                    exposure_layer(&[0.5]),
                    masked(exposure_layer(&[1.5]), &mask),
                    exposure_layer(&[-0.25]),
                ],
            ),
            (
                "positional",
                vec![masked(
                    colour_layer(json!({"exposure": [0.7], "positional": true})),
                    &mask,
                )],
            ),
            ("Basic", vec![masked(full_basic(), &mask)]),
            (
                "counting",
                vec![masked(colour_layer(json!({"counting": true})), &mask)],
            ),
        ] {
            let what = format!("{name}, {stack}");
            let compiled = registry
                .compile(width, height, &recipe_with(layers, &mask, &strokes))
                .unwrap();
            let segment = compiled.segments.last().unwrap();
            // Row `y` from column `x0` on, through every run of the segment, under `skip`, in
            // snapshot blocks of `block` pixels: every value's bits, or the refusal.
            let evaluate = |skip: UnitSkip, y: u32, x0: u32, block: usize| {
                let mut pixels = row(y)[x0 as usize..].to_vec();
                let mut scratch = vec![MaskedInput::default(); block];
                let skips = set_unit_skip(skip);
                let result = color_runs(segment)
                    .try_for_each(|run| apply_units(&run, y, x0, &mut pixels, &mut scratch));
                set_unit_skip(skips);
                result
                    .map(|()| {
                        pixels
                            .iter()
                            .flatten()
                            .map(|value| value.to_bits())
                            .collect::<Vec<_>>()
                    })
                    .map_err(|error| (error.kind, error.detail))
            };
            for y in 0..height {
                let whole = evaluate(UnitSkip::Never, y, 0, width as usize);
                assert!(
                    whole.is_ok() || y == not_a_number,
                    "{what}: row {y} failed without the skip: {whole:?}"
                );
                for block in [1, 3, width as usize] {
                    assert_eq!(
                        evaluate(UnitSkip::Proved, y, 0, block),
                        whole,
                        "{what}: row {y} in blocks of {block}"
                    );
                    let tail = evaluate(UnitSkip::Proved, y, 7, block);
                    assert_eq!(
                        tail,
                        evaluate(UnitSkip::Never, y, 7, width as usize),
                        "{what}: the tail of row {y} from column 7 in blocks of {block}"
                    );
                    if let (Ok(tail), Ok(whole)) = (&tail, &whole) {
                        assert_eq!(
                            tail[..],
                            whole[7 * 3..],
                            "{what}: the tail of row {y} is not the row's own"
                        );
                    }
                }
            }
            if stack != "counting" {
                continue;
            }
            let field = segment
                .operations
                .iter()
                .find_map(|operation| match operation {
                    crate::modules::Processing::Color(operation) => operation.mask(),
                    _ => None,
                })
                .expect("the counting layer is masked");
            let bounds = field.bounds();
            // Every row but the one whose stretch fails part way through it.
            let rows = || (0..height).filter(|y| *y != not_a_number);
            let (mut admitted, mut kept) = (0, 0);
            for y in rows() {
                for (x, input) in (0..width).zip(row(y)) {
                    if bounds.contains(x, y) {
                        admitted += 1;
                        if field.evaluate(x, y, input) == 0.0 && clean(input) {
                            kept += 1;
                        }
                    }
                }
            }
            let ran = |skip: UnitSkip| {
                counter.store(0, std::sync::atomic::Ordering::Relaxed);
                for y in rows() {
                    evaluate(skip, y, 0, width as usize).unwrap();
                }
                counter.load(std::sync::atomic::Ordering::Relaxed)
            };
            assert_eq!(ran(UnitSkip::Never), admitted, "{what}");
            assert_eq!(ran(UnitSkip::Proved), admitted - kept, "{what}");
            assert!(
                (kept > 0) == (name != "covering"),
                "{what}: {kept} of {admitted} pixels the rectangle admits kept their input"
            );
        }
    }
}

/// A linear source holding negative values, values past white and `−0.0` in one channel of every
/// fifth pixel, the one input value a pixel whose coverage is zero still runs the units for.
fn signed_zeros(width: u32, height: u32) -> crate::LinearImage {
    let pixels: Vec<[f32; 3]> = (0..width * height)
        .map(|index| {
            let value = index as f32;
            let mut pixel = [
                (value * 0.037) % 1.3 - 0.1,
                (value * 0.051) % 1.1,
                (value * 0.023) % 1.6 - 0.2,
            ];
            if index.is_multiple_of(5) {
                pixel[(index / 5 % 3) as usize] = -0.0;
            }
            pixel
        })
        .collect();
    image(width, height, &pixels)
}

/// The input of layer `layer` of `recipe` over `source`, as a GPU preview's boundary holds it: the
/// value the CPU's run hands that layer, written by the rows a frame is written by, as half floats
/// on the byte path and as `f32` on the linear path.
fn boundary_texels(
    registry: &ModuleRegistry,
    source: RenderSource<'_>,
    recipe: &Recipe,
    layer: usize,
    options: RenderOptions,
) -> Vec<u8> {
    let context = RenderContext::new();
    let rendered = crate::render::render(registry, source, recipe, options, &context).unwrap();
    let (width, height) = source.dimensions();
    let whole = Stage { width, height };
    let format = match source {
        RenderSource::Byte(_) => BoundaryFormat::Half,
        RenderSource::Linear { .. } => BoundaryFormat::Float,
    };
    rendered
        .boundary(
            &rendered.compiled,
            whole,
            crate::modules::Region::whole(whole),
            rendered.compiled.layers[layer],
            format,
        )
        .unwrap()
        .texels
        .to_vec()
}

/// The same claim through the one render entry point, on the byte path and the RAW linear path, in
/// the exact phase and in the proxy phase, where the hard-edged radials are supersampled: with the
/// skip on, serially and on the pool, the frame is the bytes it is with the skip off, and so is the
/// value a colour layer after the masked one receives inside the run, read by rows as a GPU
/// boundary (half floats and `f32`) and by points as a mask's input (`f32`), bit for bit. A masked
/// layer before a spatial one hands it the byte path's 16-bit frame. A sample equals the rendered
/// byte at every pixel of the exact phase and on a grid of the proxy phase.
#[test]
fn a_render_that_skips_uncovered_pixels_is_identical_and_samples_equal_it() {
    let registry = colour_registry();
    let (width, height) = (47, 31);
    let byte = gradient(width, height);
    let planes = signed_zeros(width, height);
    let tail = exposure_layer(&[-0.5]);
    let presence = Layer::new(crate::PRESENCE_EFFECT, json!({"clarity": 40.0}));
    for (name, mask, strokes) in uncovered_masks() {
        for (stack, layers, colour_tail) in [
            (
                "Basic",
                vec![masked(full_basic(), &mask), tail.clone()],
                true,
            ),
            (
                "a position-dependent unit inside a run",
                vec![
                    exposure_layer(&[0.25]),
                    masked(
                        colour_layer(json!({"exposure": [0.7], "positional": true})),
                        &mask,
                    ),
                    tail.clone(),
                ],
                true,
            ),
            (
                "Basic before a spatial layer",
                vec![masked(full_basic(), &mask), presence.clone()],
                false,
            ),
        ] {
            let recipe = recipe_with(layers, &mask, &strokes);
            let last = recipe.layers.len() - 1;
            for (domain, source) in [
                ("byte", RenderSource::Byte(&byte)),
                ("linear", linear(&planes, LinearSettings::default())),
            ] {
                for (phase, options) in [
                    ("exact", RenderOptions::default()),
                    ("proxy", RenderOptions::proxy(&crate::Cancel::never())),
                ] {
                    let what = format!("{name}, {stack}, {domain}, {phase}");
                    if phase == "proxy" && name.contains("radial") {
                        assert!(
                            registry.proxy_approximation(&recipe, width, height).mask,
                            "{what}: the hard edge is supersampled"
                        );
                    }
                    let context = RenderContext::new();
                    let frame = || {
                        frame_in(
                            &context,
                            &registry,
                            source,
                            SnapshotId::new(),
                            &recipe,
                            options.clone(),
                        )
                        .unwrap_or_else(|error| panic!("{what}: {error}"))
                    };
                    let everywhere = with_skip(UnitSkip::Never, frame);
                    for pooled in [false, true] {
                        parallel::force(Some(pooled));
                        let skipped = frame();
                        parallel::force(None);
                        assert!(
                            skipped.rgba == everywhere.rgba,
                            "{what}, pooled {pooled}: skipping uncovered pixels changed the frame"
                        );
                    }
                    if phase == "exact" {
                        let (_, _, sampled) = point_evaluated(
                            &context,
                            &registry,
                            source,
                            &recipe,
                            SpatialMode::Frames,
                        )
                        .unwrap();
                        assert!(
                            sampled == everywhere.rgba.as_slice(),
                            "{what}: a sample is not the rendered byte"
                        );
                    } else {
                        for y in (0..height).step_by(3) {
                            for x in (0..width).step_by(4) {
                                let sample = sample_in(
                                    &context,
                                    &registry,
                                    source,
                                    &recipe,
                                    options.clone(),
                                    x,
                                    y,
                                )
                                .unwrap();
                                assert_eq!(
                                    sample.rgba,
                                    everywhere.pixel(x, y),
                                    "{what}: the sample at ({x}, {y})"
                                );
                            }
                        }
                    }
                    if !colour_tail {
                        continue;
                    }
                    let rows = with_skip(UnitSkip::Never, || {
                        boundary_texels(&registry, source, &recipe, last, options.clone())
                    });
                    parallel::force(Some(true));
                    let skipped =
                        boundary_texels(&registry, source, &recipe, last, options.clone());
                    parallel::force(None);
                    assert!(
                        skipped == rows,
                        "{what}: the run's value at its last layer changed"
                    );
                    if phase == "exact" {
                        let points = |skip: UnitSkip| {
                            let previous = set_unit_skip(skip);
                            let context = RenderContext::new();
                            let (_, input) =
                                layer_input(&registry, source, &recipe, last, &context).unwrap();
                            let bits: Vec<u64> = (0..height)
                                .flat_map(|y| (0..width).map(move |x| (x, y)))
                                .flat_map(|(x, y)| input.linear(x, y).unwrap().unwrap())
                                .map(f64::to_bits)
                                .collect();
                            set_unit_skip(previous);
                            bits
                        };
                        assert!(
                            points(UnitSkip::Proved) == points(UnitSkip::Never),
                            "{what}: a point's value at the run's last layer changed"
                        );
                    }
                }
            }
        }
    }
}

/// What the skip changes, and the one thing it changes: a unit whose value is not finite at a pixel
/// the mask leaves uncovered fails neither the render nor a sample there, on either path, where the
/// rule before the skip failed both; a sample equals the rendered byte at every pixel; and a pixel
/// the mask covers still fails both, the same way. The masked layer's two `overflow` units turn
/// every non-zero channel into an infinity and leave a zero one zero, and a hard-edged band
/// selects exactly the black pixels, so only a pixel the mask leaves uncovered produces one.
#[test]
fn a_non_finite_unit_value_where_coverage_is_zero_fails_neither_the_render_nor_a_sample() {
    let registry = colour_registry();
    let (width, height) = (24_u32, 16_u32);
    // Black in the top-left quarter and a bright grey elsewhere, with one dim pixel in the black
    // quarter that only the second band's shoulder reaches.
    let dim = (3_u32, 2_u32);
    let code = |x: u32, y: u32| -> u8 {
        match (x, y) {
            _ if (x, y) == dim => 10,
            _ if x < width / 2 && y < height / 2 => 0,
            _ => 150,
        }
    };
    let byte = SourceImage {
        width,
        height,
        rgba: (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let value = code(x, y);
                [value, value, value, 255]
            })
            .collect::<Vec<_>>()
            .into(),
        fingerprint: "sha256:black-quarter".into(),
        orientation: 1,
        capture: Default::default(),
    };
    let pixels: Vec<[f32; 3]> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| [decode_pixel([code(x, y); 3])[0]; 3])
        .collect();
    let planes = image(width, height, &pixels);
    let overflow = colour_layer(json!({"overflow": 2}));
    // Only black: a hard band from 0 to 1 on the histogram's axis, where the dim pixel sits at 3.9.
    let black = band_mask(0.0, 0.0, 1.0, 0.0);
    // Black and the dim pixel, through a shoulder that ends at 6.
    let dark = band_mask(0.0, 0.0, 1.0, 5.0);
    let identity = colour_recipe(vec![]);
    for (domain, source) in [
        ("byte", RenderSource::Byte(&byte)),
        ("linear", linear(&planes, LinearSettings::default())),
    ] {
        let context = RenderContext::new();
        let frame = |recipe: &Recipe| {
            frame_in(
                &context,
                &registry,
                source,
                SnapshotId::new(),
                recipe,
                RenderOptions::default(),
            )
        };
        let sampled = |recipe: &Recipe, x: u32, y: u32| {
            sample_in(
                &context,
                &registry,
                source,
                recipe,
                RenderOptions::default(),
                x,
                y,
            )
        };
        let refused = |error: Error, what: &str| {
            assert_eq!(
                (error.kind, error.detail.as_str()),
                (crate::ErrorKind::ResourceLimit, NON_FINITE_COLOR),
                "{domain}: {what}"
            );
        };
        let recipe = masked_recipe(vec![masked(overflow.clone(), &black)], vec![black.clone()]);
        // The rule before the skip ran the units at every pixel and refused the bright ones.
        with_skip(UnitSkip::Never, || {
            refused(frame(&recipe).unwrap_err(), "the render before the skip");
            refused(
                sampled(&recipe, width - 1, height - 1).unwrap_err(),
                "a bright sample before the skip",
            );
        });
        // Now the bright pixels keep their input, and the black ones blend black at full coverage.
        let expected = frame(&identity).unwrap();
        for pooled in [false, true] {
            parallel::force(Some(pooled));
            let rendered = frame(&recipe);
            parallel::force(None);
            let rendered = rendered.unwrap_or_else(|error| panic!("{domain}: {error}"));
            assert_eq!(
                rendered.rgba, expected.rgba,
                "{domain}, pooled {pooled}: the frame is not its input"
            );
        }
        for y in 0..height {
            for x in 0..width {
                assert_eq!(
                    sampled(&recipe, x, y).unwrap().rgba,
                    expected.pixel(x, y),
                    "{domain}: the sample at ({x}, {y})"
                );
            }
        }
        // A pixel the mask covers still runs the units, and its infinity fails the render and the
        // sample there alike, while a sample of any other pixel answers its byte.
        let recipe = masked_recipe(vec![masked(overflow.clone(), &dark)], vec![dark.clone()]);
        for pooled in [false, true] {
            parallel::force(Some(pooled));
            let rendered = frame(&recipe);
            parallel::force(None);
            refused(rendered.unwrap_err(), "a covered pixel's infinity");
        }
        refused(
            sampled(&recipe, dim.0, dim.1).unwrap_err(),
            "the covered pixel's sample",
        );
        for (x, y) in [(0, 0), (dim.0 + 1, dim.1), (width - 1, height - 1)] {
            assert_eq!(
                sampled(&recipe, x, y).unwrap().rgba,
                expected.pixel(x, y),
                "{domain}: the sample at ({x}, {y})"
            );
        }
    }
}

/// What a masked colour layer costs on a photo-sized frame, and what the bounds rectangle saves.
/// Ignored by default because it is a recorded measurement rather than a pass/fail property:
///
/// ```sh
/// cargo test --release --package luxforge-core --lib \
///     render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame -- --ignored --nocapture
/// ```
///
/// Two layers are measured, one `+1 EV` exposure unit and a full Basic layer, each unmasked and
/// under four masks: two linear gradients, whose rectangles admit about a twentieth and all of the
/// frame, and two luminance bands, whose rectangles are always the whole stage and which select a
/// minority of it. Each masked case is measured twice, back to back, with the rule set on every
/// pool thread: the units at every pixel the rectangle admits, as before the zero-coverage skip,
/// and the skip itself. Each render prints its wall time and the process's CPU time, and each
/// masked case the least cost per pixel of its rows on one thread, which a loaded host disturbs
/// least. The source is a programmatically filled 6000 × 4000 gradient, on which the bright band
/// selects the brightest part of the frame in long runs and the middle band a diagonal band in
/// shorter ones; `LUXFORGE_MASKED_COLOUR_SOURCE` names a JPEG to measure instead. On a generated
/// photo-size fixture, four flat colour quadrants, each band selects one quadrant whole: the middle
/// band the blue one, and the bright band the yellow one through its shoulder.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn masked_colour_cost_on_a_24_megapixel_frame() {
    let registry = colour_registry();
    let source = match std::env::var("LUXFORGE_MASKED_COLOUR_SOURCE") {
        Ok(path) => crate::open_source(std::path::Path::new(&path)).expect("a JPEG source"),
        Err(_) => gradient(6000, 4000),
    };
    let stage = Stage {
        width: source.width,
        height: source.height,
    };
    let frame = f64::from(source.width) * f64::from(source.height);
    // A gradient over the bottom twentieth of the frame against one over the whole frame: the same
    // mask mathematics and the same units, differing only in how much of the frame the bounds
    // rectangle admits.
    let small = linear_mask(0.5, 0.95, 0.5, 1.0);
    let whole = linear_mask(0.5, 0.0, 0.5, 1.0);
    let bright = band_mask(80.0, 5.0, 100.0, 0.0);
    let middle = band_mask(25.0, 5.0, 38.0, 3.0);
    // The process's CPU time beside the wall time: on a host other sessions load, the work a render
    // did moves far less than how long it waited for a core.
    let mut sampler = luxforge_process::Sampler::new();
    let mut measure = |name: &str, recipe: &Recipe| {
        // One warm pass, then three measured ones: the frame allocation dominates a single run.
        render(&registry, &source, SnapshotId::new(), recipe).unwrap();
        let cpu = sampler.read().cpu_time_ns.expect("this process's CPU time");
        let started = std::time::Instant::now();
        for _ in 0..3 {
            std::hint::black_box(render(&registry, &source, SnapshotId::new(), recipe).unwrap());
        }
        let wall = started.elapsed();
        let cpu = sampler.read().cpu_time_ns.expect("this process's CPU time") - cpu;
        println!(
            "{name}: {:.1} ms per render, {:.1} ms of CPU",
            wall.as_secs_f64() * 1000.0 / 3.0,
            cpu as f64 / 1e6 / 3.0
        );
    };
    measure("identity", &colour_recipe(vec![]));
    let masks = [
        ("small bounds", small),
        ("whole frame", whole),
        ("bright band", bright),
        ("middle band", middle),
    ];
    for (name, mask) in &masks {
        // Each masked layer here is the stack's first, so the pixel its mask reads is the
        // decoded source pixel.
        let compiled =
            crate::mask::CompiledMask::new(mask, stage, &crate::path::StrokeTable::default())
                .unwrap();
        let covered = (0..source.height)
            .flat_map(|y| (0..source.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let offset = ((y * source.width + x) * 4) as usize;
                let rgb = &source.rgba[offset..offset + 3];
                compiled.evaluate(x, y, decode_pixel([rgb[0], rgb[1], rgb[2]])) != 0.0
            })
            .count();
        println!(
            "{name}: the rectangle admits {:.2}% of the frame, and coverage is not zero at {:.2}%",
            100.0 * compiled.bounds().pixels() as f64 / frame,
            100.0 * covered as f64 / frame
        );
    }
    // One thread's cost per pixel of the rows a masked colour run evaluates, the least of thirty
    // passes over 64 rows spread down the frame: a pass that waited for a core is never the least,
    // so a loaded host still reproduces it.
    let rows: Vec<(u32, Vec<[f32; 3]>)> = (0..64)
        .map(|index| {
            let y = index * source.height / 64;
            let row = (0..source.width)
                .map(|x| {
                    let offset = ((y * source.width + x) * 4) as usize;
                    let rgb = &source.rgba[offset..offset + 3];
                    decode_pixel([rgb[0], rgb[1], rgb[2]])
                })
                .collect();
            (y, row)
        })
        .collect();
    let per_pixel = |recipe: &Recipe| {
        let compiled = registry
            .compile(source.width, source.height, recipe)
            .unwrap();
        let segment = compiled.segments.last().unwrap();
        let mut scratch = vec![MaskedInput::default(); source.width as usize];
        let mut least = f64::MAX;
        for _ in 0..30 {
            let mut pass = rows.clone();
            let started = std::time::Instant::now();
            for (y, row) in &mut pass {
                for run in color_runs(segment) {
                    apply_units(&run, *y, 0, row, &mut scratch).unwrap();
                }
            }
            least = least.min(started.elapsed().as_secs_f64());
            std::hint::black_box(&pass);
        }
        least * 1e9 / (rows.len() as f64 * f64::from(source.width))
    };
    for (layer_name, layer) in [
        ("one exposure unit", exposure_layer(&[1.0])),
        ("full Basic", full_basic()),
    ] {
        measure(
            &format!("{layer_name}, unmasked"),
            &colour_recipe(vec![layer.clone()]),
        );
        for (name, mask) in &masks {
            let recipe = masked_recipe(vec![masked(layer.clone(), mask)], vec![mask.clone()]);
            for (rule, skip) in [
                (
                    ", units at every pixel the rectangle admits",
                    UnitSkip::Never,
                ),
                ("", UnitSkip::Proved),
            ] {
                set_unit_skip(skip);
                rayon::broadcast(|_| set_unit_skip(skip));
                measure(&format!("{layer_name}, {name}{rule}"), &recipe);
                println!(
                    "{layer_name}, {name}{rule}: {:.2} ns per pixel of rows on one thread",
                    per_pixel(&recipe)
                );
            }
        }
    }
}

/// One brush mask over these strokes, with the resolved table its addresses read through.
fn brush_mask(strokes: &[crate::mask::Stroke]) -> (crate::Mask, crate::path::StrokeTable) {
    let mut table = crate::path::StrokeTable::new("the brush measurement");
    let addresses: Vec<String> = strokes
        .iter()
        .map(|stroke| table.insert(stroke.clone()).to_string())
        .collect();
    let mut mask = crate::Mask::new("Mask 1");
    let name = mask.next_component_name("brush");
    mask.components.push(crate::Component::new(
        name,
        crate::ComponentMode::Add,
        "brush",
        json!({ "strokes": addresses }),
    ));
    (mask, table)
}

/// `count` strokes of a plausible retouching brush, laid out so they neither coincide nor leave
/// the frame: each is a three-position path across its own column, at a radius of 0.05 mask-space
/// units — a twentieth of the frame's height, which is 200 px on a 24 MP stage — and softly
/// feathered, which is the brush the panel starts with.
fn painted_strokes(count: usize) -> Vec<crate::mask::Stroke> {
    (0..count)
        .map(|index| {
            let t = (index as f64 + 0.5) / count as f64;
            let y = 0.1 + 0.8 * t;
            crate::mask::Stroke::capture(
                &[[0.1, y], [0.5, y + 0.02], [0.9, y]],
                0.05,
                50.0,
                100.0,
                false,
            )
            .expect("a legal stroke")
        })
        .collect()
}

/// What a **brush** mask costs on a photo-sized frame: the grid index it compiles to, the
/// rectangle it bounds, the render it modulates, and the point query it answers. Ignored by
/// default because it is a recorded measurement rather than a pass/fail property:
///
/// ```sh
/// cargo test --release --package luxforge-core --lib \
///     render::mask_tests::masked_brush_cost_on_photo_sized_frames -- --ignored --nocapture
/// ```
///
/// Four stroke counts are measured, up to the delivered per-component limit, against the
/// unmasked render and against a whole-frame gradient mask — the case
/// [`masked_colour_cost_on_a_24_megapixel_frame`] already records — so what a brush costs is
/// stated as a difference from a mask whose cost is known rather than on its own.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn masked_brush_cost_on_photo_sized_frames() {
    let registry = colour_registry();
    for (label, width, height) in [("24 MP", 6000u32, 4000u32), ("60 MP", 9000, 6667)] {
        let source = gradient(width, height);
        let stage = Stage { width, height };
        let pixels = f64::from(width) * f64::from(height);
        let measure = |name: &str, recipe: &Recipe| {
            // One warm pass, then three measured ones: the frame allocation dominates a single
            // run.
            render(&registry, &source, SnapshotId::new(), recipe).unwrap();
            let started = std::time::Instant::now();
            for _ in 0..3 {
                std::hint::black_box(
                    render(&registry, &source, SnapshotId::new(), recipe).unwrap(),
                );
            }
            println!(
                "{label} {name}: {:.1} ms per render",
                started.elapsed().as_secs_f64() * 1000.0 / 3.0
            );
        };
        measure("identity", &colour_recipe(vec![]));
        measure(
            "unmasked exposure",
            &colour_recipe(vec![exposure_layer(&[1.0])]),
        );
        let gradient_mask = linear_mask(0.5, 0.0, 0.5, 1.0);
        measure(
            "whole-frame gradient mask",
            &masked_recipe(
                vec![masked(exposure_layer(&[1.0]), &gradient_mask)],
                vec![gradient_mask],
            ),
        );

        for count in [1usize, 8, 32, 64] {
            let strokes = painted_strokes(count);
            let (mask, table) = brush_mask(&strokes);
            // The compile is where the grid index is built, before a pixel is read. It is charged to the gesture, not to the frame, so it is
            // measured on its own.
            let started = std::time::Instant::now();
            let rounds = 20;
            for _ in 0..rounds {
                std::hint::black_box(crate::mask::CompiledMask::new(&mask, stage, &table).unwrap());
            }
            let compile = started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds);
            let compiled = crate::mask::CompiledMask::new(&mask, stage, &table).unwrap();
            println!(
                "{label} {count} strokes: {compile:.3} ms to compile, rectangle admits \
                 {:.2}% of the frame",
                100.0 * compiled.bounds().pixels() as f64 / pixels
            );
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers: vec![masked(exposure_layer(&[1.0]), &mask)],
                masks: vec![mask.clone()],
                strokes: table.clone(),
                artifacts: Default::default(),
            };
            measure(&format!("{count}-stroke brush mask"), &recipe);

            // The point query, which is the rule-4 claim: a sample through a masked colour layer
            // costs `O(layers)` and never rasterizes, so its cost must not grow with the stroke
            // count.
            // A thousand of them, spread over the frame, because one is too fast to time.
            let queries = 1000u32;
            let started = std::time::Instant::now();
            for index in 0..queries {
                let x = (index * 7919) % width;
                let y = (index * 6271) % height;
                std::hint::black_box(sample(&registry, &source, &recipe, x, y).unwrap());
            }
            println!(
                "{label} {count} strokes: {:.4} ms per point query",
                started.elapsed().as_secs_f64() * 1000.0 / f64::from(queries)
            );
        }
    }
}

/// What one position of a painted stroke costs in path work, at each length the stroke reaches:
/// the per-point mask work a drafted stroke repeats on every `draft.set` — decimating the
/// captured path, checking the posted path, capturing and hashing the stroke, and building its
/// grid index — so a later reader can see it is not the latency. Ignored by default because it
/// is a recorded measurement:
///
/// ```sh
/// cargo test --release --package luxforge-core --lib \
///     render::mask_tests::painted_stroke_path_work_per_position -- --ignored --nocapture
/// ```
///
/// The path is `editor-latency --mode paint`'s own sine at its own brush (radius 0.06, feather
/// 50), sampled at `n` positions over the same span, so the decimated length grows with `n` as
/// a longer stroke's does.
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn painted_stroke_path_work_per_position() {
    use crate::{
        mask::Stroke,
        path::{PathCapture, decimate},
    };
    let size = 0.06;
    let sine = |n: usize| -> Vec<[f64; 2]> {
        (0..n)
            .map(|index| {
                let t = index as f64 / (n - 1) as f64;
                [
                    0.2 + 0.8 * t,
                    0.5 + 0.2 * (std::f64::consts::TAU * 2.5 * t).sin(),
                ]
            })
            .collect()
    };
    let points = crate::ParameterDescriptor::points("points", 1, 16384);
    let time = |rounds: u32, mut work: Box<dyn FnMut() + '_>| {
        work();
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            work();
        }
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds)
    };
    for n in [100usize, 400, 1600, 6400] {
        let raw = sine(n);
        let stored = decimate(&raw, size).unwrap();
        let rounds = 200;
        // The desktop, per pointer event: the whole captured path decimated again...
        let whole = time(
            rounds,
            Box::new(|| {
                std::hint::black_box(decimate(&raw, size).unwrap());
            }),
        );
        // ...or the new position pushed onto the held capture and the held grid reduced.
        let mut capture = PathCapture::default();
        for point in &raw[..n - 1] {
            capture.push(*point);
        }
        let incremental = time(
            rounds,
            Box::new(|| {
                let mut held = capture.clone();
                held.push(raw[n - 1]);
                std::hint::black_box(held.decimated(size).unwrap());
            }),
        );
        let clone_only = time(
            rounds,
            Box::new(|| {
                std::hint::black_box(capture.clone());
            }),
        );
        let posted = json!(stored);
        // The owner, per `draft.set`: the generic check of the posted path...
        let check = time(
            rounds,
            Box::new(|| {
                crate::modules::check_value(&points, &posted).unwrap();
            }),
        );
        // ...and, planning its preview, the capture and the content address...
        let capture_hash = time(
            rounds,
            Box::new(|| {
                let stroke = Stroke::capture(&stored, size, 50.0, 100.0, false).unwrap();
                std::hint::black_box(stroke.id());
            }),
        );
        // ...which, on the raw path an appending client would leave on the owner, costs this.
        let raw_capture_hash = time(
            rounds,
            Box::new(|| {
                let stroke = Stroke::capture(&raw, size, 50.0, 100.0, false).unwrap();
                std::hint::black_box(stroke.id());
            }),
        );
        // ...and the brush component's grid index, compiled at the paint workload's proxy
        // stage and at the full 24 MP stage.
        let stroke = Stroke::capture(&stored, size, 50.0, 100.0, false).unwrap();
        let (mask, table) = brush_mask(std::slice::from_ref(&stroke));
        let (mask, table) = (&mask, &table);
        let index = |stage: Stage| {
            time(
                rounds,
                Box::new(move || {
                    std::hint::black_box(
                        crate::mask::CompiledMask::new(mask, stage, table).unwrap(),
                    );
                }),
            )
        };
        let proxy = index(Stage {
            width: 1716,
            height: 1144,
        });
        let full = index(Stage {
            width: 6000,
            height: 4000,
        });
        println!(
            "{n} positions, {} stored: decimate whole {whole:.4} ms, push and reduce \
             {incremental:.4} ms (of which the benchmark's clone {clone_only:.4}), check \
             {check:.4} ms, capture and hash {capture_hash:.4} ms (raw path {raw_capture_hash:.4}), \
             index {proxy:.4} ms proxy / {full:.4} ms 24 MP",
            stored.len()
        );
    }
}
