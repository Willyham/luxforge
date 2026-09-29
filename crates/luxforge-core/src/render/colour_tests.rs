//! The pointwise colour stage: quantization, the colour runs over rows and pixels, and their
//! scratch budget.

use super::byte::source_pixel;
use super::colour_runs::{COLOR_CHUNK_ROWS, COLOR_CHUNK_SCRATCH_BYTES, NON_FINITE_COLOR};
use super::testing::{frame_in, linear, render, sample, sample_in};
use super::tests::*;
use super::*;
use crate::{
    EFFECT_FORMAT, ErrorKind, Layer, LayerId, MAX_COLOR_UNITS, Orientation, Recipe, SnapshotId,
    SourceImage, Transform,
    colour::srgb::{CODE_BINS, decode_table, quantize_pixel, quantizer},
    modules::Processing,
};
use luxforge_reference::srgb;
use serde_json::{Value, json};
use std::sync::Arc;

fn quantizer_reference_thresholds() -> [f64; 255] {
    std::array::from_fn(|index| decode_reference((index as f64 + 0.5) / 255.0))
}

/// The canonical search, independent of the production index and its bin selection.
fn quantizer_search_reference(thresholds: &[f64; 255], value: f64) -> u8 {
    let value = value.clamp(0.0, 1.0);
    thresholds.partition_point(|threshold| *threshold <= value) as u8
}

#[test]
fn indexed_quantizer_matches_search_at_every_threshold_and_bin_boundary() {
    let thresholds = quantizer_reference_thresholds();
    let check = |value| {
        assert_eq!(
            quantizer().channel(value),
            quantizer_search_reference(&thresholds, value),
            "value {value:?}, bits {:#018x}",
            value.to_bits()
        );
    };
    for threshold in thresholds {
        for bits in threshold.to_bits() - 128..=threshold.to_bits() + 128 {
            check(f64::from_bits(bits));
        }
        let rounded = threshold as f32;
        for value in [rounded.next_down(), rounded, rounded.next_up()] {
            check(f64::from(value));
        }
    }
    for bin in 0..=CODE_BINS {
        let boundary = bin as f64 / CODE_BINS as f64;
        for value in [boundary.next_down(), boundary, boundary.next_up()] {
            check(value);
        }
        let rounded = boundary as f32;
        for value in [rounded.next_down(), rounded, rounded.next_up()] {
            check(f64::from(value));
        }
    }
}

#[test]
fn indexed_quantizer_matches_search_across_dense_random_and_extended_values() {
    let thresholds = quantizer_reference_thresholds();
    let check = |value| {
        assert_eq!(
            quantizer().channel(value),
            quantizer_search_reference(&thresholds, value),
            "value {value:?}, bits {:#018x}",
            value.to_bits()
        );
    };
    for value in [
        f64::NEG_INFINITY,
        f64::MIN,
        -1.0,
        -f64::MIN_POSITIVE,
        -f64::from_bits(1),
        -0.0,
        0.0,
        f64::from_bits(1),
        f64::MIN_POSITIVE,
        1.0,
        32.0,
        f64::MAX,
        f64::INFINITY,
        f64::NAN,
        -f64::NAN,
        f64::from_bits(0x7ff0_0000_0000_0001),
    ] {
        check(value);
    }
    for step in 0..=100_000 {
        check(f64::from(step) / 100_000.0);
    }
    let mut bits = 0x1234_5678_9abc_def0_u64;
    for _ in 0..100_000 {
        bits = bits.wrapping_mul(6364136223846793005).wrapping_add(1);
        check(f64::from_bits(bits));
        check(f64::from(f32::from_bits(bits as u32)));
    }
}

#[test]
fn indexed_quantizer_preserves_complete_serial_and_parallel_colour_buffers() {
    let thresholds = quantizer_reference_thresholds();
    let registry = colour_registry();
    let evs = [0.7_f64, -0.2];
    let gains = evs.map(|ev| ev.exp2() as f32);
    let recipe = colour_recipe(vec![exposure_layer(&evs)]);
    let (width, height) = (257, 129);
    for pooled in [false, true] {
        parallel::force(Some(pooled));
        let source = source(width, height);
        let mut expected = Vec::with_capacity(source.rgba.len());
        for pixel in source.rgba.chunks_exact(4) {
            for channel in &pixel[..3] {
                let mut linear = decode_reference(f64::from(*channel) / 255.0) as f32;
                for gain in gains {
                    linear *= gain;
                }
                expected.push(quantizer_search_reference(&thresholds, f64::from(linear)));
            }
            // Every frame is opaque.
            expected.push(255);
        }
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        assert_eq!(raster.rgba.as_slice(), expected, "pooled {pooled}");
    }
    parallel::force(None);
}

/// An independent stepwise f64 evaluation of the colour contract: decode the byte, multiply by
/// `2^EV` once per unit in order, clamp, encode and round with `floor(255·e + 0.5)`. It shares
/// no code with the renderer and returns the clamped linear value as well as the code, so a
/// disagreement can be tested against the threshold it sits on.
fn colour_reference(byte: u8, evs: &[f64]) -> (u8, f64) {
    let mut linear = decode_reference(f64::from(byte) / 255.0);
    for ev in evs {
        linear *= 2.0_f64.powf(*ev);
    }
    let clamped = linear.clamp(0.0, 1.0);
    let code = (255.0 * encode_reference(clamped) + 0.5).floor();
    (code as u8, clamped)
}

#[test]
fn the_output_boundary_quantizes_exactly_like_rounding_the_encoded_value() {
    let reference = |value: f32| -> u8 {
        let clamped = f64::from(value).clamp(0.0, 1.0);
        (255.0 * encode_reference(clamped) + 0.5).floor() as u8
    };
    // A dense sweep of the whole range, plus values outside it that the boundary clamps.
    for step in 0..=40_000 {
        let value = (f64::from(step) / 40_000.0) as f32;
        assert_eq!(quantize_pixel([value; 3])[0], reference(value), "{value}");
    }
    for value in [-1.0_f32, -0.0, 0.0, 1.0, 1.5, 1e20] {
        assert_eq!(quantize_pixel([value; 3])[0], reference(value), "{value}");
    }
    // And on both sides of every one of the 255 thresholds, in the f32 neighbourhood of each.
    for code in 1..=255_u32 {
        let threshold = decode_reference((f64::from(code) - 0.5) / 255.0) as f32;
        let bits = threshold.to_bits();
        for value in [
            f32::from_bits(bits - 1),
            threshold,
            f32::from_bits(bits + 1),
        ] {
            assert_eq!(
                quantize_pixel([value; 3])[0],
                reference(value),
                "code {code} at {value}"
            );
        }
    }
}

/// The byte resample's contract: a clamped value's forward rounding, `round(255 · encode(v))`,
/// from the shared reference. This is what the sample computed with a power function per
/// channel before it quantized through the guarded threshold search.
fn forward_rounding_reference(linear: f64) -> u8 {
    (srgb::encode_clamped(linear) * 255.0).round() as u8
}

/// One channel's linear value as `bilinear` blends it over a 2 × 2 frame whose corners, in
/// row order, hold `codes`: each tap's weight times its decoded code, summed in tap order.
fn bilinear_value(codes: [u8; 4], u: f64, v: f64) -> f64 {
    let table = decode_table();
    let taps = Taps::new(u, v, 2, 2);
    taps.corners
        .iter()
        .zip(taps.weights)
        .map(|(&(x, y), weight)| {
            weight * f64::from(table[usize::from(codes[(y * 2 + x) as usize])])
        })
        .sum()
}

/// `bilinear` over a 2 × 2 frame whose three channels hold `codes`, checked channel by channel
/// against the forward rounding of the value it blends. Returns channel 0's blended value.
fn check_bilinear(codes: [[u8; 4]; 3], u: f64, v: f64) -> f64 {
    let fetch = |x: u32, y: u32| {
        let at = (y * 2 + x) as usize;
        Ok([codes[0][at], codes[1][at], codes[2][at], 255])
    };
    let pixel = bilinear(decode_table(), quantizer(), u, v, 2, 2, fetch).unwrap();
    for channel in 0..3 {
        let linear = bilinear_value(codes[channel], u, v);
        assert_eq!(
            pixel[channel],
            forward_rounding_reference(linear),
            "codes {codes:?} at ({u:?}, {v:?}), channel {channel}, value {linear:?}"
        );
    }
    bilinear_value(codes[0], u, v)
}

#[test]
fn bilinear_samples_round_forward_at_every_code_boundary() {
    let table = decode_table();
    // Values this close to a threshold take the forward evaluation; the next band out is
    // answered by the threshold search alone. Both must be reached for the test to mean
    // anything. Some blends here take the other code through the search alone, so the
    // quantizer without its guard fails this test.
    let (mut guarded, mut searched) = (0_u32, 0_u32);
    for code in 1..=255_u8 {
        let threshold = srgb::decode_encoded((f64::from(code) - 0.5) / 255.0);
        for (low, high) in [
            (code - 1, code),
            (0, 255),
            (code.saturating_sub(2), code.saturating_add(1)),
        ] {
            let (a, b) = (
                f64::from(table[usize::from(low)]),
                f64::from(table[usize::from(high)]),
            );
            assert!(
                a < threshold && threshold < b,
                "{low}..{high} around {code}"
            );
            // Horizontal blends at a row's centre, and the same blend spread over both rows,
            // whose four products round differently.
            for v in [0.5, 0.875] {
                for delta in [0.0, -2e-12, -1e-12, -5e-13, 5e-13, 1e-12, 2e-12] {
                    let u = 0.5 + (threshold + delta - a) / (b - a);
                    let bits = u.to_bits();
                    for bits in bits - 32..=bits + 32 {
                        let u = f64::from_bits(bits);
                        let blended = check_bilinear(
                            [[low, high, low, high], [high, low, high, low], [code; 4]],
                            u,
                            v,
                        );
                        let distance = (blended - threshold).abs();
                        if distance <= 1e-12 {
                            guarded += 1;
                        } else if distance <= 1e-9 {
                            searched += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        guarded > 0 && searched > 0,
        "{guarded} guarded, {searched} searched"
    );
}

#[test]
fn bilinear_samples_round_forward_across_random_frames_and_taps() {
    let mut state = 0x0f1e_2d3c_4b5a_6978_u64;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        state
    };
    for _ in 0..200_000 {
        let codes = std::array::from_fn(|_| {
            let bits = next();
            std::array::from_fn(|corner| (bits >> (16 * corner)) as u8)
        });
        // Inside the frame and past its edges, where the taps clamp.
        let u = (next() >> 11) as f64 / (1_u64 << 53) as f64 * 4.0 - 1.0;
        let v = (next() >> 11) as f64 / (1_u64 << 53) as f64 * 4.0 - 1.0;
        check_bilinear(codes, u, v);
    }
}

#[test]
fn zero_exposure_round_trips_every_grey_and_a_neutral_layer_shares_the_source() {
    let registry = colour_registry();
    let source = greys();
    let raster = render(
        &registry,
        &source,
        SnapshotId::new(),
        &colour_recipe(vec![exposure_layer(&[0.0])]),
    )
    .unwrap();
    assert_eq!(
        raster.rgba.as_ref(),
        source.rgba.as_ref(),
        "0 EV decodes and quantizes every byte back to itself"
    );
    for value in 0..=255_u8 {
        assert_eq!(colour_reference(value, &[0.0]).0, value);
    }
    // A colour layer with units materializes the frame; a neutral one compiles to nothing and
    // keeps the identity byte path with the source allocation itself.
    assert!(!Arc::ptr_eq(&raster.rgba, &source.rgba));
    let neutral = render(
        &registry,
        &source,
        SnapshotId::new(),
        &colour_recipe(vec![exposure_layer(&[])]),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&neutral.rgba, &source.rgba));
    let compiled = registry
        .compile(
            source.width,
            source.height,
            &colour_recipe(vec![exposure_layer(&[])]),
        )
        .unwrap();
    assert!(compiled.segments[0].operations.is_empty());
    assert!(!compiled.segments[0].has_color);
}

/// Every frame a pass writes is allocated as the raster's own `Arc<Vec<u8>>`, so whatever pass
/// wrote last is what the render returns, with no copy after it, on each kind of stack and on
/// both pixel domains: a colour pass (the one frame it writes), an exact transform, a resample and
/// a spatial boundary. The bytes are the ones a point sample reads. A byte stack that writes
/// nothing still returns the source allocation itself; a linear one writes its one terminal frame.
#[test]
fn a_render_returns_the_frame_its_last_pass_wrote() {
    let registry = registry();
    let (width, height) = (48, 36);
    let source = gradient(width, height);
    let planes = varied(width, height);
    let layer = |effect: &str, payload: Value| Layer {
        id: LayerId::new(),
        effect_id: effect.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        mask: None,
        artifacts: Vec::new(),
    };
    let colour = layer(
        crate::BASIC_EFFECT,
        json!({"exposure": 0.4, "contrast": 20.0}),
    );
    let turn = Layer::orientation(Orientation {
        mirror: true,
        turns: 1,
    });
    let crop = Layer::crop(fitted_crop(width, height, 6.0, [0.1, 0.1, 0.8, 0.8]));
    let presence = layer(crate::PRESENCE_EFFECT, json!({"clarity": 40.0}));
    let recipe = |layers: Vec<Layer>| Recipe {
        format: crate::RECIPE_FORMAT,
        layers,
        masks: Vec::new(),
        ..Recipe::default()
    };
    let frame = |input: RenderSource<'_>, recipe: &Recipe| {
        frame_writes::record(|| {
            frame_in(
                &RenderContext::new(),
                &registry,
                input,
                SnapshotId::new(),
                recipe,
                RenderOptions::default(),
            )
            .unwrap()
        })
    };
    for (domain, input) in [
        ("byte", RenderSource::Byte(&source)),
        ("linear", linear(&planes, LinearSettings::default())),
    ] {
        for (case, layers) in [
            ("a colour pass", vec![colour.clone()]),
            ("an exact transform", vec![turn.clone(), colour.clone()]),
            (
                "a resample",
                vec![
                    colour.clone(),
                    crop.clone(),
                    Layer::pixel(3, 4, [250, 1, 2]),
                ],
            ),
            ("a spatial boundary", vec![presence.clone(), turn.clone()]),
        ] {
            let recipe = recipe(layers);
            let (raster, written) = frame(input, &recipe);
            assert_eq!(
                written.last(),
                Some(&(raster.rgba.as_ptr() as usize)),
                "{domain}, {case}: the raster is the frame written last, not a copy of it"
            );
            if case == "a colour pass" {
                assert_eq!(written.len(), 1, "{domain}: one pass writes one frame");
            }
            let grid = super::render(
                &registry,
                input,
                &recipe,
                RenderOptions::default(),
                &RenderContext::new(),
            )
            .unwrap()
            .grid(6, &|| Ok(()))
            .unwrap();
            for ((x, y), sampled) in
                super::entry::grid_centres(6, raster.width, raster.height).zip(grid)
            {
                assert_eq!(
                    raster.pixel(x, y),
                    Some(sampled),
                    "{domain}, {case} at ({x}, {y})"
                );
            }
        }
    }
    let identity = recipe(Vec::new());
    let (raster, written) = frame(RenderSource::Byte(&source), &identity);
    assert!(written.is_empty(), "an identity byte stack writes no frame");
    assert!(Arc::ptr_eq(&raster.rgba, &source.rgba));
    let (raster, written) = frame(linear(&planes, LinearSettings::default()), &identity);
    assert_eq!(
        written,
        [raster.rgba.as_ptr() as usize],
        "an identity linear stack writes its one terminal frame"
    );
}

/// A pass over the shared source loads the source's rows into its own frame inside the pass,
/// and a geometry that only translates copies each output row as one run of its input row. The
/// source is never written, and every pixel of each frame is the point sample of the same
/// stack: a colour pass through the identity, a straight crop, and a straight crop around a
/// colour pass and a replacement.
#[test]
fn a_pass_over_the_source_loads_its_rows_and_leaves_the_source_alone() {
    let registry = registry();
    let (width, height) = (67, 41);
    let source = gradient(width, height);
    let original = source.rgba.as_slice().to_vec();
    let colour = Layer {
        id: LayerId::new(),
        effect_id: crate::BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"exposure": 0.4, "contrast": 20.0}),
        mask: None,
        artifacts: Vec::new(),
    };
    let crop = Layer::crop(fitted_crop(width, height, 0.0, [0.1, 0.2, 0.7, 0.6]));
    for (case, layers) in [
        ("a colour pass through the identity", vec![colour.clone()]),
        ("a straight crop", vec![crop.clone()]),
        (
            "a straight crop around a colour pass and a replacement",
            vec![colour, Layer::pixel(3, 4, [250, 1, 2]), crop],
        ),
    ] {
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        };
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        assert!(
            !Arc::ptr_eq(&raster.rgba, &source.rgba),
            "{case}: the pass writes its own frame"
        );
        assert_eq!(
            source.rgba.as_slice(),
            original,
            "{case}: the source is untouched"
        );
        for y in 0..raster.height {
            for x in 0..raster.width {
                let sampled = sample(&registry, &source, &recipe, x, y).unwrap();
                assert_eq!(raster.pixel(x, y), sampled.rgba, "{case} at ({x}, {y})");
            }
        }
    }
}

#[test]
fn exposure_matches_an_independent_f64_reference_within_one_code() {
    let registry = colour_registry();
    for ev in [1.0, -1.0, 2.0, -2.0, 0.5, -3.0, 5.0] {
        for source in [greys(), gradient(37, 23)] {
            let raster = render(
                &registry,
                &source,
                SnapshotId::new(),
                &colour_recipe(vec![exposure_layer(&[ev])]),
            )
            .unwrap();
            for y in 0..source.height {
                for x in 0..source.width {
                    let input = source_pixel(&source, x, y);
                    let actual = raster.pixel(x, y).expect("inside the stage");
                    for channel in 0..3 {
                        let (expected, linear) = colour_reference(input[channel], &[ev]);
                        assert_code_within_tolerance(
                            actual[channel],
                            expected,
                            linear,
                            &format!("{ev} EV at ({x}, {y}) channel {channel}"),
                        );
                    }
                    assert_eq!(actual[3], 255, "every frame is opaque");
                }
            }
        }
    }
}

/// Forced onto the pool, so the row chunks run in parallel over a frame several chunks tall.
#[test]
fn a_colour_pass_on_the_parallel_path_matches_the_reference() {
    let registry = colour_registry();
    let source = gradient(120, 90);
    parallel::force(Some(true));
    let raster = render(
        &registry,
        &source,
        SnapshotId::new(),
        &colour_recipe(vec![exposure_layer(&[1.0])]),
    )
    .unwrap();
    for y in 0..source.height {
        for x in 0..source.width {
            let input = source_pixel(&source, x, y);
            let actual = raster.pixel(x, y).expect("inside the stage");
            for channel in 0..3 {
                let (expected, linear) = colour_reference(input[channel], &[1.0]);
                assert_code_within_tolerance(
                    actual[channel],
                    expected,
                    linear,
                    &format!("({x}, {y}) channel {channel}"),
                );
            }
            assert_eq!(actual[3], input[3]);
        }
    }
    parallel::force(None);
}

#[test]
fn an_inverse_pair_in_one_operation_returns_the_exact_input_bytes() {
    let registry = colour_registry();
    for source in [greys(), gradient(29, 17)] {
        let raster = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![exposure_layer(&[1.0, -1.0])]),
        )
        .unwrap();
        assert_eq!(
            raster.rgba.as_ref(),
            source.rgba.as_ref(),
            "nothing is clamped or quantized between the units of one operation"
        );
    }
}

#[test]
fn two_consecutive_colour_operations_keep_values_outside_the_range_between_them() {
    let registry = colour_registry();
    let source = gradient(29, 17);
    let recipe = colour_recipe(vec![exposure_layer(&[3.0]), exposure_layer(&[-3.0])]);
    let compiled = registry
        .compile(source.width, source.height, &recipe)
        .unwrap();
    assert_eq!(
        compiled.segments[0]
            .operations
            .iter()
            .filter(|operation| matches!(operation, Processing::Color(_)))
            .count(),
        2,
        "two layers are two operations"
    );
    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
    assert_eq!(
        raster.rgba.as_ref(),
        source.rgba.as_ref(),
        "consecutive operations fuse into one run, so +3 EV does not clip before −3 EV"
    );
    // Separating them with a point replacement does clip, because a replacement is a boundary.
    let separated = colour_recipe(vec![
        exposure_layer(&[3.0]),
        Layer::pixel(0, 0, [1, 2, 3]),
        exposure_layer(&[-3.0]),
    ]);
    let clipped = render(&registry, &source, SnapshotId::new(), &separated).unwrap();
    assert_ne!(clipped.rgba.as_ref(), source.rgba.as_ref());
}

#[test]
fn a_replacement_before_a_colour_operation_is_exposed_and_one_after_it_is_not() {
    let registry = colour_registry();
    let source = gradient(8, 6);
    let rgb = [10, 120, 200];
    let recipe = colour_recipe(vec![
        Layer::pixel(1, 1, rgb),
        exposure_layer(&[1.0]),
        Layer::pixel(2, 1, rgb),
    ]);
    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
    let mut exposed = [0; 3];
    for (channel, slot) in exposed.iter_mut().enumerate() {
        *slot = colour_reference(rgb[channel], &[1.0]).0;
    }
    assert_eq!(
        raster.pixel(1, 1).map(|p| [p[0], p[1], p[2]]),
        Some(exposed),
        "the replacement before the colour operation is processed by it"
    );
    assert_eq!(
        raster.pixel(2, 1).map(|p| [p[0], p[1], p[2]]),
        Some(rgb),
        "the replacement after it keeps its exact bytes"
    );
    for y in 0..raster.height {
        for x in 0..raster.width {
            assert_eq!(
                sample(&registry, &source, &recipe, x, y).unwrap().rgba,
                raster.pixel(x, y),
                "({x}, {y})"
            );
        }
    }
}

#[test]
fn exact_geometry_commutes_with_pointwise_colour() {
    let registry = colour_registry();
    let source = gradient(13, 9);
    for transform in [
        Transform::MirrorHorizontal,
        Transform::RotateRight,
        Transform::FlipVertical,
    ] {
        let before = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![turn(transform), exposure_layer(&[1.5])]),
        )
        .unwrap();
        let after = render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![exposure_layer(&[1.5]), turn(transform)]),
        )
        .unwrap();
        assert_eq!((before.width, before.height), (after.width, after.height));
        assert_eq!(before.rgba, after.rgba, "{transform:?}");
    }
}

#[test]
fn a_colour_operation_before_a_rotated_crop_quantizes_then_resamples() {
    let registry = colour_registry();
    let (width, height) = (40_u32, 24_u32);
    let source = gradient(width, height);
    let crop = fitted_crop(width, height, 10.0, [0.15, 0.15, 0.7, 0.7]);
    let exposed_bytes = render(
        &registry,
        &source,
        SnapshotId::new(),
        &colour_recipe(vec![exposure_layer(&[1.0])]),
    )
    .unwrap()
    .rgba;
    let exposed = SourceImage {
        rgba: exposed_bytes,
        fingerprint: "sha256:exposed".into(),
        ..source.clone()
    };
    let recipe = colour_recipe(vec![exposure_layer(&[1.0]), crop_layer(crop)]);
    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
    // The colour run ends at the resample, so the crop interpolates the quantized frame: the
    // same bytes as cropping an already exposed source.
    let separately = render(
        &registry,
        &exposed,
        SnapshotId::new(),
        &colour_recipe(vec![crop_layer(crop)]),
    )
    .unwrap();
    assert_eq!(raster.rgba, separately.rgba);
    // And that frame is what the independent f64 crop reference samples, within its one code.
    let reference = CropReference::new(&exposed, crop);
    assert_eq!(
        (raster.width, raster.height),
        (reference.width, reference.height)
    );
    for j in 0..raster.height {
        for i in 0..raster.width {
            let expected = reference.pixel(&exposed, i, j);
            let actual = raster.pixel(i, j).expect("inside the output stage");
            for channel in 0..4 {
                assert!(
                    (i32::from(actual[channel]) - i32::from(expected[channel])).abs() <= 1,
                    "({i}, {j}) channel {channel}: {actual:?} against {expected:?}"
                );
            }
        }
    }
}

#[test]
fn an_over_long_or_non_finite_colour_operation_is_refused_by_compilation() {
    let registry = colour_registry();
    let source = gradient(8, 6);
    assert!(
        render(
            &registry,
            &source,
            SnapshotId::new(),
            &colour_recipe(vec![exposure_layer(&[0.5; MAX_COLOR_UNITS])]),
        )
        .is_ok(),
        "eight units are the bound, not one too many"
    );
    for (case, layer, detail) in [
        (
            "nine units",
            exposure_layer(&[0.5; MAX_COLOR_UNITS + 1]),
            "more than the 8",
        ),
        (
            "a non-finite unit",
            colour_layer(json!({"exposure": [1.0], "infinite": true})),
            "not finite",
        ),
    ] {
        let recipe = colour_recipe(vec![layer]);
        for error in [
            render(&registry, &source, SnapshotId::new(), &recipe).unwrap_err(),
            sample(&registry, &source, &recipe, 0, 0).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Validation, "{case}");
            assert!(error.detail.contains(detail), "{case}: {error}");
        }
    }
}

#[test]
fn a_colour_unit_that_overflows_fails_the_render_and_the_sample() {
    let registry = colour_registry();
    let source = gradient(8, 6);
    let recipe = colour_recipe(vec![colour_layer(json!({"overflow": 2}))]);
    for error in [
        render(&registry, &source, SnapshotId::new(), &recipe).unwrap_err(),
        sample(&registry, &source, &recipe, 4, 3).unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(error.detail, NON_FINITE_COLOR);
    }
}

#[test]
fn a_colour_pass_reserves_its_row_chunks_from_the_scratch_budget() {
    let registry = colour_registry();
    let source = gradient(64, 48);
    let recipe = colour_recipe(vec![exposure_layer(&[1.0])]);
    // Below the parallel threshold the pass runs its chunks one after another, so the
    // high-water mark of a context only this render reserves from is exactly one chunk.
    let chunk = (color_chunk_rows(source.width) * source.width as usize * 12) as u64;
    let context = RenderContext::new();
    let budget = context.scratch();
    assert_eq!(budget.target(), 64 * 1024 * 1024, "the declared default");
    let expected = frame_in(
        &context,
        &registry,
        &source,
        SnapshotId::new(),
        &recipe,
        RenderOptions::default(),
    )
    .unwrap();
    assert_eq!(budget.in_use(), 0, "nothing is held after the render");
    assert_eq!(budget.peak(), chunk, "one row chunk at a time");
    // The target is a target: a render whose row chunk is larger than all of it still
    // completes, with the same bytes, and the high-water mark shows it went past.
    let small = RenderContext::with_scratch_target(16);
    let rendered = frame_in(
        &small,
        &registry,
        &source,
        SnapshotId::new(),
        &recipe,
        RenderOptions::default(),
    )
    .expect("a chunk past the target still runs");
    assert_eq!(rendered.rgba, expected.rgba);
    assert_eq!(
        small.scratch().peak(),
        chunk,
        "the chunk was reserved and counted"
    );
    assert_eq!(small.scratch().in_use(), 0);
    // A point sample streams nothing, so it reserves nothing whatever the target is.
    let empty = RenderContext::with_scratch_target(0);
    let sampled = sample_in(
        &empty,
        &registry,
        &source,
        &recipe,
        RenderOptions::default(),
        1,
        1,
    )
    .unwrap();
    assert!(sampled.rgba.is_some());
    assert_eq!(empty.scratch().peak(), 0, "a sample reserves no scratch");
}

#[test]
fn a_row_chunk_stays_inside_the_scratch_budget_at_every_supported_width() {
    // 16 workers, one chunk each: the byte cap decides for wide frames and the row cap for
    // narrow ones, and neither reaches the 64 MiB target.
    for width in [1_u32, 64, 6000, 10_000, 16_384] {
        let rows = color_chunk_rows(width);
        let bytes = rows * width as usize * std::mem::size_of::<[f32; 3]>();
        assert!((1..=COLOR_CHUNK_ROWS).contains(&rows), "{width}");
        assert!(bytes <= COLOR_CHUNK_SCRATCH_BYTES, "{width}: {bytes} bytes");
        assert!(
            (16 * bytes as u64) < context::DEFAULT_SCRATCH_BYTES,
            "{width}: {bytes} bytes per worker"
        );
    }
    assert_eq!(color_chunk_rows(10_000), 8);
    assert_eq!(color_chunk_rows(64), COLOR_CHUNK_ROWS);
}
