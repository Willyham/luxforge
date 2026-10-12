//! An export's reference output: the Lanczos-3 taps, the resample against a stepwise reference
//! that sums each output pixel on its own, the 16-bit terminal, and the reference output's shapes.

use super::*;
use crate::{
    AssetId, BASIC_EFFECT, EntryId, Evaluation, HistoryEntry, Layer, LinearSettings,
    ModuleRegistry, PRESENCE_EFFECT, PreviewSource, Recipe, RenderContext, Snapshot,
    colour::srgb::encode,
    render::tests::{gradient, varied},
    tiles::{PixelRows, reference_output},
};
use serde_json::json;
use std::sync::Arc;

/// `width` × `height` codes from a fixed sequence, spread over the whole 16-bit range.
fn codes(width: u32, height: u32, seed: u64) -> Vec<u16> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..width as usize * height as usize * 3)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 48) as u16
        })
        .collect()
}

/// The taps of one output pixel along a side, found on their own: every stage pixel whose centre
/// lies within the kernel's support of the output pixel's centre, weighted and normalized.
fn stepwise_taps(from: u32, to: u32, index: u32) -> Vec<(usize, f64)> {
    if from == to {
        return vec![(index as usize, 1.0)];
    }
    let scale = f64::from(from) / f64::from(to);
    let centre = (f64::from(index) + 0.5) * scale;
    let taps: Vec<(usize, f64)> = (0..from)
        .map(|k| (k as usize, lanczos((f64::from(k) + 0.5 - centre) / scale)))
        .filter(|&(_, weight)| weight != 0.0)
        .collect();
    let sum: f64 = taps.iter().map(|&(_, weight)| weight).sum();
    taps.into_iter()
        .map(|(k, weight)| (k, weight / sum))
        .collect()
}

/// Each output pixel of `terminal` resampled to `width` × `height` on its own, in linear light and
/// unclamped: `Σ_y w_y · (Σ_x w_x · v)`, both in ascending order.
fn stepwise_sums(terminal: &WideTerminal, width: u32, height: u32) -> Vec<[f64; 3]> {
    let table = decode16_table();
    let mut sums = Vec::new();
    for y in 0..height {
        let rows = stepwise_taps(terminal.height, height, y);
        for x in 0..width {
            let columns = stepwise_taps(terminal.width, width, x);
            let mut sum = [0.0f64; 3];
            for &(row, row_weight) in &rows {
                let mut across = [0.0f64; 3];
                for &(column, weight) in &columns {
                    let codes = terminal.codes(row * terminal.width as usize + column);
                    for channel in 0..3 {
                        across[channel] += weight * f64::from(table[usize::from(codes[channel])]);
                    }
                }
                for channel in 0..3 {
                    sum[channel] += row_weight * across[channel];
                }
            }
            sums.push(sum);
        }
    }
    sums
}

/// The 8-bit code of a linear value: `round(255 · encode(v))`, clamped first.
fn code8(value: f64) -> u8 {
    (255.0 * encode(value.clamp(0.0, 1.0)) + 0.5).floor() as u8
}

/// The 16-bit code of a linear value narrowed to `f32`: `round(65535 · encode(v))`, clamped first.
fn code16(value: f64) -> u16 {
    let value = f64::from(value as f32);
    (65535.0 * encode(value.clamp(0.0, 1.0)) + 0.5).floor() as u16
}

/// `terminal` resampled to `width` × `height` at both depths matches the stepwise reference's
/// sums encoded once, every pixel opaque.
fn assert_resample_matches(terminal: &WideTerminal, width: u32, height: u32) {
    let sums = stepwise_sums(terminal, width, height);
    let eight = OutputSpec {
        width,
        height,
        depth: Depth::Eight,
    };
    let Pixels::Eight(rgba) = resample(terminal, eight, &Cancel::never()).unwrap() else {
        panic!("an 8-bit output");
    };
    let expected: Vec<u8> = sums
        .iter()
        .flat_map(|sum| [code8(sum[0]), code8(sum[1]), code8(sum[2]), 255])
        .collect();
    assert_eq!(rgba, expected, "8 bits, {width}x{height}");
    let sixteen = OutputSpec {
        depth: Depth::Sixteen,
        ..eight
    };
    let Pixels::Sixteen(rgba) = resample(terminal, sixteen, &Cancel::never()).unwrap() else {
        panic!("a 16-bit output");
    };
    let expected: Vec<u16> = sums
        .iter()
        .flat_map(|sum| [code16(sum[0]), code16(sum[1]), code16(sum[2]), u16::MAX])
        .collect();
    assert_eq!(rgba, expected, "16 bits, {width}x{height}");
}

/// The kernel is `sinc(x) · sinc(x / 3)` inside three output pixels and zero outside, one at zero
/// and zero at every other whole number.
#[test]
fn resample_kernel_is_lanczos_three() {
    let sinc = |x: f64| {
        let pi_x = std::f64::consts::PI * x;
        pi_x.sin() / pi_x
    };
    assert_eq!(lanczos(0.0), 1.0);
    for whole in [-3.0, -2.0, -1.0, 1.0, 2.0, 3.0, 4.5, -7.0] {
        assert_eq!(lanczos(whole), 0.0, "{whole}");
    }
    for step in 1..600 {
        let x = f64::from(step) * 0.005 - 1.5;
        if x == 0.0 || x.fract() == 0.0 {
            continue;
        }
        let expected = sinc(x) * sinc(x / 3.0);
        assert!((lanczos(x) - expected).abs() < 1e-12, "{x}");
        assert_eq!(lanczos(x), lanczos(-x), "symmetric at {x}");
    }
    assert!(lanczos(1.5) < 0.0, "the first negative lobe");
}

/// Each output pixel's taps sum to one and lie within the stage, its support scaled with the
/// reduction; a side kept at its size is its own pixels with the weight one; an empty side or one
/// larger than the stage is refused.
#[test]
fn resample_taps_scale_their_support_and_sum_to_one() {
    for (from, to) in [(7, 3), (3000, 30), (9, 1), (2, 1), (101, 100), (5, 4)] {
        let taps = lanczos_taps(from, to).unwrap();
        assert_eq!(taps.len(), to as usize);
        let scale = f64::from(from) / f64::from(to);
        for (index, tap) in taps.iter().enumerate() {
            let sum: f64 = tap.weights.iter().sum();
            assert!((sum - 1.0).abs() < 1e-12, "{from}->{to} at {index}: {sum}");
            assert!(tap.first + tap.weights.len() as u32 <= from);
            assert!(
                tap.weights.len() as f64 <= 6.0 * scale + 2.0,
                "{from}->{to}: {} taps at a reduction of {scale}",
                tap.weights.len()
            );
            let expected = stepwise_taps(from, to, index as u32);
            let nonzero: Vec<(usize, f64)> = tap
                .weights
                .iter()
                .enumerate()
                .map(|(at, &weight)| (tap.first as usize + at, weight))
                .filter(|&(_, weight)| weight != 0.0)
                .collect();
            assert_eq!(nonzero.len(), expected.len(), "{from}->{to} at {index}");
            for ((k, weight), (expected_k, expected_weight)) in nonzero.iter().zip(&expected) {
                assert_eq!(k, expected_k);
                assert!((weight - expected_weight).abs() < 1e-15);
            }
        }
    }
    // An interior output pixel of a reduction by a hundred reads six hundred stage pixels.
    let wide = &lanczos_taps(3000, 30).unwrap()[15];
    assert_eq!(wide.weights.len(), 600);
    assert_eq!(wide.first, 1250);

    let kept = lanczos_taps(5, 5).unwrap();
    for (index, tap) in kept.iter().enumerate() {
        assert_eq!(
            tap,
            &Taps {
                first: index as u32,
                weights: vec![1.0]
            }
        );
    }
    for (from, to) in [(0, 0), (5, 0), (5, 6), (0, 1)] {
        assert_eq!(
            lanczos_taps(from, to).unwrap_err().kind.code(),
            "validation",
            "{from}->{to}"
        );
    }
}

/// Odd sizes, a reduction from thousands of pixels to tens on either side, a side of one pixel and
/// a side kept at its size each resample to the stepwise reference's codes exactly, at 8 and 16
/// bits.
#[test]
fn resample_matches_the_stepwise_reference_at_both_depths() {
    for (index, (from, to)) in [
        ((7, 5), (3, 2)),
        ((3000, 4), (30, 1)),
        ((3, 2000), (1, 37)),
        ((9, 1), (1, 1)),
        ((10, 6), (10, 3)),
        ((11, 13), (11, 13)),
        ((1, 1), (1, 1)),
    ]
    .into_iter()
    .enumerate()
    {
        let terminal = WideTerminal::words(from.0, from.1, codes(from.0, from.1, index as u64));
        assert_resample_matches(&terminal, to.0, to.1);
    }
}

/// An output large enough to be resampled on the shared pool matches the stepwise reference too,
/// band for band.
#[test]
fn resample_on_the_pool_matches_the_stepwise_reference() {
    let terminal = WideTerminal::words(530, 410, codes(530, 410, 99));
    assert!(super::super::parallel::pooled(
        super::super::parallel::RenderPass::Resample,
        400 * 301
    ));
    assert_resample_matches(&terminal, 400, 301);
}

/// The terminal's linear path, six native-order bytes a pixel, resamples to the same codes as the
/// same codes held as words.
#[test]
fn resample_reads_the_linear_terminal_as_its_codes() {
    let words = codes(23, 17, 5);
    let bytes: Vec<u8> = words.iter().flat_map(|code| code.to_ne_bytes()).collect();
    let output = OutputSpec {
        width: 8,
        height: 5,
        depth: Depth::Sixteen,
    };
    assert_eq!(
        resample(
            &WideTerminal::bytes(23, 17, bytes),
            output,
            &Cancel::never()
        )
        .unwrap(),
        resample(
            &WideTerminal::words(23, 17, words),
            output,
            &Cancel::never()
        )
        .unwrap()
    );
}

/// A hard edge rings below black and past white once resampled; each such value is clamped before
/// it is encoded, so it takes the code of black or of white rather than wrapping.
#[test]
fn resample_clamps_the_ringing_of_a_hard_edge() {
    let (width, height) = (40, 3);
    let rgb: Vec<u16> = (0..height)
        .flat_map(|_| (0..width).flat_map(|x| [if x < 20 { 0 } else { u16::MAX }; 3]))
        .collect();
    let terminal = WideTerminal::words(width, height, rgb);
    let sums = stepwise_sums(&terminal, 13, 1);
    assert!(sums.iter().any(|sum| sum[0] < 0.0), "rings below black");
    assert!(sums.iter().any(|sum| sum[0] > 1.0), "rings past white");
    assert_resample_matches(&terminal, 13, 1);
    let output = OutputSpec {
        width: 13,
        height: 1,
        depth: Depth::Sixteen,
    };
    let Pixels::Sixteen(rgba) = resample(&terminal, output, &Cancel::never()).unwrap() else {
        panic!("a 16-bit output");
    };
    for (pixel, sum) in rgba.chunks_exact(4).zip(&sums) {
        if sum[0] < 0.0 {
            assert_eq!(pixel[0], 0);
        }
        if sum[0] > 1.0 {
            assert_eq!(pixel[0], u16::MAX);
        }
    }
}

/// A cancelled resample answers `cancelled`.
#[test]
fn resample_stops_when_cancelled() {
    let terminal = WideTerminal::words(30, 40, codes(30, 40, 3));
    let cancel = Cancel::new();
    cancel.cancel();
    let output = OutputSpec {
        width: 10,
        height: 20,
        depth: Depth::Eight,
    };
    assert_eq!(
        resample(&terminal, output, &cancel)
            .unwrap_err()
            .kind
            .code(),
        "cancelled"
    );
}

/// The wide terminal's 16-bit output is its codes with an opaque alpha, from either layout.
#[test]
fn wide_terminal_widens_to_opaque_rgba16() {
    let words = codes(5, 3, 11);
    let expected: Vec<u16> = words
        .chunks_exact(3)
        .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], u16::MAX])
        .collect();
    let bytes: Vec<u8> = words.iter().flat_map(|code| code.to_ne_bytes()).collect();
    assert_eq!(
        WideTerminal::words(5, 3, words)
            .into_rgba16(&Cancel::never())
            .unwrap(),
        expected
    );
    assert_eq!(
        WideTerminal::bytes(5, 3, bytes)
            .into_rgba16(&Cancel::never())
            .unwrap(),
        expected
    );
}

/// `recipe` over `source`, bound for evaluation as the catalog owner binds a saved entry's stack.
fn evaluation(source: PreviewSource, layers: Vec<Layer>) -> Evaluation {
    let asset_id = AssetId::new();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset_id.clone(),
        sequence: 1,
        action_id: "test".into(),
        label: "Test".into(),
        parameters: json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot::original(asset_id),
        undo_parent: None,
        restore_target: None,
    };
    let recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        layers,
        ..Recipe::default()
    };
    Evaluation::new(
        Arc::new(ModuleRegistry::builtin()),
        RenderContext::new(),
        source,
        entry,
        recipe,
        None,
    )
}

fn jpeg(width: u32, height: u32) -> PreviewSource {
    PreviewSource::Jpeg(gradient(width, height))
}

fn raw(width: u32, height: u32) -> PreviewSource {
    PreviewSource::Raw {
        image: varied(width, height),
        settings: LinearSettings::default(),
    }
}

fn basic() -> Layer {
    Layer::new(
        BASIC_EFFECT,
        json!({"exposure": 0.4, "contrast": 20.0, "vibrance": 15.0}),
    )
}

fn presence() -> Layer {
    Layer::new(PRESENCE_EFFECT, json!({"clarity": 40.0, "texture": 25.0}))
}

fn output(evaluation: &Evaluation, spec: OutputSpec) -> Result<crate::tiles::OutputFrame, Error> {
    reference_output(
        evaluation,
        spec,
        evaluation.entry().snapshot.id.clone(),
        &Cancel::never(),
    )
}

/// The exact 8-bit frame of `evaluation`.
fn frame(evaluation: &Evaluation) -> Arc<Vec<u8>> {
    evaluation
        .exact(&Cancel::never())
        .unwrap()
        .frame(evaluation.entry().snapshot.id.clone())
        .unwrap()
        .rgba
}

/// The full-size 8-bit output is the reference's own exact frame, shared rather than copied, for a
/// JPEG and a RAW, edited or not.
#[test]
fn tiles_reference_output_at_full_size_and_8_bits_is_the_frame() {
    for (name, source) in [("JPEG", jpeg(37, 23)), ("RAW", raw(37, 23))] {
        for layers in [vec![], vec![basic(), presence()]] {
            let evaluation = evaluation(source.clone(), layers);
            let full = output(&evaluation, OutputSpec::full(37, 23)).unwrap();
            assert_eq!(
                (full.width, full.height, full.depth()),
                (37, 23, Depth::Eight)
            );
            assert!(full.shared().is_some(), "{name}: the frame is not copied");
            assert_eq!(full.rows(), PixelRows::Eight(&frame(&evaluation)), "{name}");
        }
    }
}

/// A JPEG left as it is holds each 8-bit code at 16 bits as its multiple of 257; an edited JPEG
/// and a RAW hold the same terminal value the 8-bit frame quantizes, at 16 bits, so each code lies
/// within half an 8-bit step of the frame's.
#[test]
fn tiles_reference_output_at_16_bits_encodes_the_frames_values_once() {
    let wide = OutputSpec {
        width: 41,
        height: 29,
        depth: Depth::Sixteen,
    };
    let identity = evaluation(jpeg(41, 29), vec![]);
    let rgba = output(&identity, wide).unwrap();
    let narrow: Vec<u16> = frame(&identity)
        .iter()
        .enumerate()
        .map(|(index, &code)| {
            if index % 4 == 3 {
                u16::MAX
            } else {
                u16::from(code) * 257
            }
        })
        .collect();
    assert_eq!(rgba.rows(), PixelRows::Sixteen(&narrow));

    for (name, source, layers) in [
        ("edited JPEG", jpeg(41, 29), vec![basic()]),
        (
            "JPEG through a spatial chain",
            jpeg(41, 29),
            vec![presence(), basic()],
        ),
        ("RAW", raw(41, 29), vec![]),
        ("edited RAW", raw(41, 29), vec![basic(), presence()]),
    ] {
        let evaluation = evaluation(source, layers);
        let full = output(&evaluation, wide).unwrap();
        assert_eq!(full.depth(), Depth::Sixteen);
        let PixelRows::Sixteen(wide_codes) = full.rows() else {
            panic!("{name}: 16-bit rows");
        };
        let narrow = frame(&evaluation);
        let mut finer = false;
        for (index, (&wide, &narrow)) in wide_codes.iter().zip(narrow.iter()).enumerate() {
            if index % 4 == 3 {
                assert_eq!(wide, u16::MAX, "{name}: opaque");
                continue;
            }
            let step = f64::from(wide) * 255.0 / 65535.0 - f64::from(narrow);
            assert!(
                step.abs() <= 0.51,
                "{name}: {wide} against {narrow} at {index}"
            );
            finer |= wide % 257 != 0;
        }
        assert!(
            finer,
            "{name}: the 16-bit codes are not the 8-bit ones widened"
        );
    }
}

/// A smaller output is the stage's 16-bit terminal resampled: for a JPEG left as it is, the frame's
/// codes widened by 257 resampled by the stepwise reference.
#[test]
fn tiles_reference_output_when_smaller_resamples_the_terminal() {
    let evaluation = evaluation(jpeg(45, 31), vec![]);
    let rgb: Vec<u16> = frame(&evaluation)
        .chunks_exact(4)
        .flat_map(|pixel| [0, 1, 2].map(|channel| u16::from(pixel[channel]) * 257))
        .collect();
    let terminal = WideTerminal::words(45, 31, rgb);
    let sums = stepwise_sums(&terminal, 16, 11);
    for depth in [Depth::Eight, Depth::Sixteen] {
        let resized = output(
            &evaluation,
            OutputSpec {
                width: 16,
                height: 11,
                depth,
            },
        )
        .unwrap();
        assert_eq!(
            (resized.width, resized.height, resized.depth()),
            (16, 11, depth)
        );
        assert!(resized.shared().is_none());
        match resized.into_pixels() {
            Pixels::Eight(rgba) => assert_eq!(
                rgba,
                sums.iter()
                    .flat_map(|sum| [code8(sum[0]), code8(sum[1]), code8(sum[2]), 255])
                    .collect::<Vec<_>>()
            ),
            Pixels::Sixteen(rgba) => assert_eq!(
                rgba,
                sums.iter()
                    .flat_map(|sum| [code16(sum[0]), code16(sum[1]), code16(sum[2]), u16::MAX])
                    .collect::<Vec<_>>()
            ),
        }
    }
}

/// An output larger than its stage on either side, or with an empty side, is refused with
/// `validation`; a cancelled render answers `cancelled`.
#[test]
fn tiles_reference_output_refuses_an_enlarged_or_empty_output() {
    let evaluation = evaluation(jpeg(12, 8), vec![]);
    for (width, height) in [(13, 8), (12, 9), (0, 8), (12, 0)] {
        for depth in [Depth::Eight, Depth::Sixteen] {
            let spec = OutputSpec {
                width,
                height,
                depth,
            };
            assert_eq!(
                output(&evaluation, spec).unwrap_err().kind.code(),
                "validation",
                "{width}x{height}"
            );
        }
    }
    let cancel = Cancel::new();
    cancel.cancel();
    let spec = OutputSpec {
        width: 6,
        height: 4,
        depth: Depth::Sixteen,
    };
    assert_eq!(
        reference_output(
            &evaluation,
            spec,
            evaluation.entry().snapshot.id.clone(),
            &cancel
        )
        .unwrap_err()
        .kind
        .code(),
        "cancelled"
    );
}
