//! The masked colour primitive: its blend, its bounds and its cost.

use super::testing::{frame_in, render, sample, sample_in};
use super::tests::*;
use super::*;
use crate::{
    Layer, Recipe, SnapshotId, SourceImage, Transform,
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

/// Outside the bounds rectangle a masked operation evaluates **no unit at all**, counted. The
/// same test states the rectangle's own conservatism: it is at most one pixel larger on each side
/// than the rows whose coverage is non-zero.
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
    counter.store(0, std::sync::atomic::Ordering::Relaxed);
    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
    let counted = counter.load(std::sync::atomic::Ordering::Relaxed);
    let compiled =
        crate::mask::CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default()).unwrap();
    let bounds = compiled.bounds();
    assert_eq!(
        counted as u64,
        bounds.pixels(),
        "the unit ran over {counted} pixels against a {}x{} rectangle",
        bounds.width,
        bounds.height
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
        let mut scratch = vec![[0.0f32; 3]; block];
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
    // One row chunk plus one row of snapshot. The budget is a **target and not a limit**, so a
    // target that fits the chunk but not the snapshot renders anyway rather than refusing: what
    // the snapshot costs is visible in the high-water mark, never in an error. Below the
    // parallel threshold the chunks run one after another, so each context's high-water mark
    // is exactly what one chunk of its render carried.
    let chunk = (color_chunk_rows(source.width) * source.width as usize * 12) as u64;
    let snapshot = source.width as u64 * 12;
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

/// What a masked colour layer costs on a photo-sized frame, and what the bounds rectangle saves.
/// Ignored by default because it is a recorded measurement rather than a pass/fail property:
///
/// ```sh
/// cargo test --release --package luxforge-core --lib \
///     render::mask_tests::masked_colour_cost_on_a_24_megapixel_frame -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a recorded measurement, not an assertion"]
fn masked_colour_cost_on_a_24_megapixel_frame() {
    let registry = colour_registry();
    let source = gradient(6000, 4000);
    let stage = Stage {
        width: source.width,
        height: source.height,
    };
    // A gradient over the bottom twentieth of the frame against one over the whole frame: the same
    // mask mathematics and the same units, differing only in how much of the frame the bounds
    // rectangle admits.
    let small = linear_mask(0.5, 0.95, 0.5, 1.0);
    let whole = linear_mask(0.5, 0.0, 0.5, 1.0);
    let unmasked = colour_recipe(vec![exposure_layer(&[1.0])]);
    let identity = colour_recipe(vec![]);
    let measure = |name: &str, recipe: &Recipe| {
        // One warm pass, then three measured ones: the frame allocation dominates a single run.
        render(&registry, &source, SnapshotId::new(), recipe).unwrap();
        let started = std::time::Instant::now();
        for _ in 0..3 {
            std::hint::black_box(render(&registry, &source, SnapshotId::new(), recipe).unwrap());
        }
        println!(
            "{name}: {:.1} ms per 24 MP render",
            started.elapsed().as_secs_f64() * 1000.0 / 3.0
        );
    };
    measure("identity", &identity);
    measure("unmasked exposure", &unmasked);
    for (name, mask) in [("small bounds", small), ("whole frame", whole)] {
        let compiled =
            crate::mask::CompiledMask::new(&mask, stage, &crate::path::StrokeTable::default())
                .unwrap();
        let bounds = compiled.bounds();
        println!(
            "{name}: the rectangle admits {:.2}% of the frame",
            100.0 * bounds.pixels() as f64 / (6000.0 * 4000.0)
        );
        measure(
            name,
            &masked_recipe(
                vec![masked(exposure_layer(&[1.0]), &mask)],
                vec![mask.clone()],
            ),
        );
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

            // The point query, which is the rule-4 claim: a sample through a masked layer costs
            // `O(layers)` and never rasterizes, so its cost must not grow with the stroke count.
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
