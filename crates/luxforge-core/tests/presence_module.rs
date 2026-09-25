//! The Presence module (`luxforge.presence`) end to end, following `mixer_module.rs`'s patterns:
//! the independent `f64` reference through the real render path on the byte and the RAW linear
//! paths, and achromatic invariance. What Presence shares with every field-patch module —
//! discovery, neutral payloads, drafts, history, one layer per target, sample equal to render
//! through a crop, an unavailable provider and reopen — is proved once, for every such module, by
//! `field_patch_conformance.rs`, and its place after the colour run by the Presence, mixer and
//! vignette chapter of `editor-acceptance`.
//!
//! Numerical rule, from `docs/design/presence-study.md`'s frozen tolerance: production matches the
//! reference within `2e-4 + 2e-4 * |reference|` in linear light and at most one output code. This
//! file sees the quantized frame, so it asserts the code: an exact code everywhere except where the
//! reference's linear value sits within that tolerance of the threshold between two codes. The
//! linear-light half of the tolerance is asserted against the committed oracle in the crate's own
//! `modules::presence::oracle`, which can read the units' `f32` output before the host quantizes it.
//! Identity stacks and byte sharing are exact with no tolerance at all.

mod reference;

use luxforge_core::{
    EFFECT_FORMAT, Layer, LayerId, LinearImage, LinearSettings, ModuleRegistry, PRESENCE_EFFECT,
    RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, render, render_linear, sample, sample_linear,
};
use reference::presence::{PresenceParams, Rgb, apply_presence};
use serde_json::{Value, json};
use std::hash::{DefaultHasher, Hash, Hasher};

// -------------------------------------------------------------------------------------------
// Shared helpers.
// -------------------------------------------------------------------------------------------

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

/// A fingerprint that names these exact contents, as a real source's does. The host keys what it
/// caches for a source — Dehaze's stored atmospheric light among it — by the fingerprint, so two
/// different pictures of one size must never share one.
fn content_fingerprint(kind: &str, contents: impl Hash) -> String {
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);
    format!("sha256:presence-module-{kind}-{:016x}", hasher.finish())
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
    let fingerprint = content_fingerprint("fixture", (width, height, &rgba));
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
    let bits: Vec<u32> = planes.iter().map(|value| value.to_bits()).collect();
    let fingerprint = content_fingerprint("linear-fixture", (width, height, bits));
    (
        LinearImage::with_fingerprint(width as u32, height as u32, planes, fingerprint).unwrap(),
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
// Real-buffer proofs
// -------------------------------------------------------------------------------------------

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
