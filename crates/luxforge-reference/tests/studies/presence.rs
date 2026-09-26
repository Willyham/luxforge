//! Property proofs, measurements and fixtures for the frozen Presence reference in
//! `crates/luxforge-reference/src/presence.rs`.
//!
//! The frozen equations, constants, halos and figures are recorded in
//! `docs/design/presence-study.md`; this file is what makes them reproducible.
//!
//! Two committed fixtures back the study:
//!
//! * `fixtures/presence/cases.json` — small deterministic images, the unit amounts applied
//!   to them and the resulting linear-sRGB output at full `f64` precision, the oracle a
//!   later production implementation is compared against.
//! * `fixtures/presence/measurements.json` — every figure quoted in the study note.
//!
//! Both are produced by the ignored generator:
//!
//! ```sh
//! cargo test --package luxforge-reference --test studies \
//!     -- --ignored generate_presence_fixtures
//! ```
//!
//! `presence_fixtures_match_reference` and `measurements_match_reference` (not ignored)
//! reload the committed files and recompute every value from the same reference, so a
//! silent drift between the files and the frozen formulas fails the build.

use luxforge_reference::presence::{
    self as presence, Atmosphere, PresenceParams, Rect, Rgb, T_FLOOR, decode_srgb_extended,
    encode_srgb_extended, luminance,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Deterministic fixture images
// ---------------------------------------------------------------------------

/// The 64-bit LCG every noisy fixture uses, so a fixture is reproducible from its spec
/// alone: `x <- x * 6364136223846793005 + 1442695040888963407`, value `(x >> 11) / 2^53`.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

/// A deterministic synthetic image. Every fixture is described by one of these, so the
/// committed JSON never carries a pixel buffer and production can rebuild the same input.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ImageSpec {
    /// A neutral step edge in linear light, along x (or y when `vertical`).
    Step {
        width: i64,
        height: i64,
        low: f64,
        high: f64,
        vertical: bool,
    },
    /// A neutral sinusoid in the *encoded* domain, so the measured band gain is read
    /// directly in the domain the units work in.
    Sine {
        width: i64,
        height: i64,
        mean: f64,
        amplitude: f64,
        period: f64,
        vertical: bool,
    },
    /// An 8-bit quantised luminance ramp: `repeat` columns per code, starting at
    /// `first_code`. Used to measure whether a unit introduces extra quantization steps.
    Ramp8 {
        width: i64,
        height: i64,
        first_code: u32,
        repeat: i64,
    },
    /// Neutral uniform noise of the given linear amplitude about a linear mean.
    Noise {
        width: i64,
        height: i64,
        mean: f64,
        amplitude: f64,
        seed: u64,
    },
    /// A multi-scale textured patch with an optional per-channel tint, for hue checks.
    Patch {
        width: i64,
        height: i64,
        mean: f64,
        seed: u64,
        tint: [f64; 3],
    },
    /// A neutral linear ramp from `low` to `high` across x.
    Grey {
        width: i64,
        height: i64,
        low: f64,
        high: f64,
    },
    /// A synthetic scene built through the forward haze model `I = t*J + (1 - t)*A`.
    ///
    /// `J` is a checkerboard whose even 8-pixel cells are exactly black, so the
    /// dark-channel prior holds in every window, plus `sky_rows` rows at the top where
    /// `J = A`, so the atmospheric-light estimator has a pure-haze region to find.
    /// `t` runs linearly from `t_min` at `x = 0` to `t_max` at `x = width - 1`.
    Haze {
        width: i64,
        height: i64,
        t_min: f64,
        t_max: f64,
        a: [f64; 3],
        sky_rows: i64,
        cell: i64,
    },
}

const HAZE_PALETTE: [[f64; 3]; 6] = [
    [0.80, 0.35, 0.20],
    [0.20, 0.70, 0.30],
    [0.15, 0.25, 0.85],
    [0.70, 0.70, 0.20],
    [0.45, 0.45, 0.45],
    [0.10, 0.55, 0.60],
];

fn code_to_linear(code: u32) -> f64 {
    let encoded = f64::from(code) / 255.0;
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_code(linear: f64) -> u32 {
    let clamped = linear.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.003_130_8 {
        12.92 * clamped
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (255.0 * encoded + 0.5).floor() as u32
}

impl ImageSpec {
    fn size(&self) -> (i64, i64) {
        match *self {
            ImageSpec::Step { width, height, .. }
            | ImageSpec::Sine { width, height, .. }
            | ImageSpec::Ramp8 { width, height, .. }
            | ImageSpec::Noise { width, height, .. }
            | ImageSpec::Patch { width, height, .. }
            | ImageSpec::Grey { width, height, .. }
            | ImageSpec::Haze { width, height, .. } => (width, height),
        }
    }

    fn build(&self) -> Rgb {
        let (width, height) = self.size();
        let mut pixels = vec![[0.0f64; 3]; (width * height) as usize];
        match *self {
            ImageSpec::Step {
                low,
                high,
                vertical,
                ..
            } => {
                for y in 0..height {
                    for x in 0..width {
                        let along = if vertical { y } else { x };
                        let extent = if vertical { height } else { width };
                        let value = if along < extent / 2 { low } else { high };
                        pixels[(y * width + x) as usize] = [value; 3];
                    }
                }
            }
            ImageSpec::Sine {
                mean,
                amplitude,
                period,
                vertical,
                ..
            } => {
                for y in 0..height {
                    for x in 0..width {
                        let along = if vertical { y } else { x } as f64;
                        let encoded =
                            mean + amplitude * (std::f64::consts::TAU * along / period).sin();
                        let value = decode_srgb_extended(encoded);
                        pixels[(y * width + x) as usize] = [value; 3];
                    }
                }
            }
            ImageSpec::Ramp8 {
                first_code, repeat, ..
            } => {
                for y in 0..height {
                    for x in 0..width {
                        let code = (first_code as i64 + x / repeat).min(255) as u32;
                        let value = code_to_linear(code);
                        pixels[(y * width + x) as usize] = [value; 3];
                    }
                }
            }
            ImageSpec::Noise {
                mean,
                amplitude,
                seed,
                ..
            } => {
                let mut rng = Lcg::new(seed);
                for pixel in pixels.iter_mut() {
                    let value = mean + amplitude * (rng.next_unit() * 2.0 - 1.0);
                    *pixel = [value; 3];
                }
            }
            ImageSpec::Patch {
                mean, seed, tint, ..
            } => {
                let mut rng = Lcg::new(seed);
                for y in 0..height {
                    for x in 0..width {
                        let fx = x as f64;
                        let fy = y as f64;
                        let value = mean
                            + 0.05
                                * (std::f64::consts::TAU * fx / 5.0).sin()
                                * (std::f64::consts::TAU * fy / 7.0).sin()
                            + 0.04 * (std::f64::consts::TAU * fx / 19.0).sin()
                            + 0.03 * (std::f64::consts::TAU * fy / 31.0).cos()
                            + 0.01 * (rng.next_unit() * 2.0 - 1.0);
                        pixels[(y * width + x) as usize] =
                            [value * tint[0], value * tint[1], value * tint[2]];
                    }
                }
            }
            ImageSpec::Grey {
                low, high, width, ..
            } => {
                for y in 0..height {
                    for x in 0..width {
                        let fraction = if width > 1 {
                            x as f64 / (width - 1) as f64
                        } else {
                            0.0
                        };
                        let value = low + (high - low) * fraction;
                        pixels[(y * width + x) as usize] = [value; 3];
                    }
                }
            }
            ImageSpec::Haze {
                t_min,
                t_max,
                a,
                sky_rows,
                cell,
                width,
                ..
            } => {
                for y in 0..height {
                    for x in 0..width {
                        let clear = if y < sky_rows {
                            a
                        } else {
                            let bx = x / cell;
                            let by = y / cell;
                            if bx % 2 == 0 && by % 2 == 0 {
                                [0.0; 3]
                            } else {
                                HAZE_PALETTE[(((bx / 2) + 3 * (by / 2)) % 6) as usize]
                            }
                        };
                        let fraction = if width > 1 {
                            x as f64 / (width - 1) as f64
                        } else {
                            0.0
                        };
                        let t = t_min + (t_max - t_min) * fraction;
                        let mut pixel = [0.0; 3];
                        for channel in 0..3 {
                            pixel[channel] = t * clear[channel] + (1.0 - t) * a[channel];
                        }
                        pixels[(y * width + x) as usize] = pixel;
                    }
                }
            }
        }
        Rgb::from_pixels(width, height, &pixels)
    }
}

// ---------------------------------------------------------------------------
// Named fixture images
// ---------------------------------------------------------------------------

/// The long side every measurement fixture is evaluated at: a 24 MP stage. The fixtures are
/// small because a tile of such a stage is small; the radii are the production ones.
const MEASURE_LONG_SIDE: i64 = 6000;

fn step_high() -> ImageSpec {
    ImageSpec::Step {
        width: 1024,
        height: 8,
        low: 0.05,
        high: 0.75,
        vertical: false,
    }
}

fn step_low() -> ImageSpec {
    ImageSpec::Step {
        width: 1024,
        height: 8,
        low: 0.35,
        high: 0.55,
        vertical: false,
    }
}

fn step_vertical() -> ImageSpec {
    ImageSpec::Step {
        width: 8,
        height: 1024,
        low: 0.05,
        high: 0.75,
        vertical: true,
    }
}

fn ramp8() -> ImageSpec {
    ImageSpec::Ramp8 {
        width: 512,
        height: 8,
        first_code: 40,
        repeat: 4,
    }
}

fn noise(amplitude: f64, seed: u64) -> ImageSpec {
    ImageSpec::Noise {
        width: 96,
        height: 96,
        mean: 0.2,
        amplitude,
        seed,
    }
}

fn patch() -> ImageSpec {
    ImageSpec::Patch {
        width: 96,
        height: 96,
        mean: 0.25,
        seed: 20_260_922,
        tint: [1.0, 0.85, 0.7],
    }
}

fn grey_ramp() -> ImageSpec {
    ImageSpec::Grey {
        width: 64,
        height: 8,
        low: 0.0,
        high: 1.0,
    }
}

const HAZE_A: [f64; 3] = [0.85, 0.88, 0.95];

fn haze_flat() -> ImageSpec {
    ImageSpec::Haze {
        width: 128,
        height: 256,
        t_min: 0.4,
        t_max: 0.4,
        a: HAZE_A,
        sky_rows: 32,
        cell: 8,
    }
}

fn haze_ramp() -> ImageSpec {
    ImageSpec::Haze {
        width: 128,
        height: 256,
        t_min: 0.25,
        t_max: 0.9,
        a: HAZE_A,
        sky_rows: 32,
        cell: 8,
    }
}

fn haze_ramp_gentle() -> ImageSpec {
    ImageSpec::Haze {
        width: 128,
        height: 256,
        t_min: 0.38,
        t_max: 0.42,
        a: HAZE_A,
        sky_rows: 32,
        cell: 8,
    }
}

fn haze_clear() -> ImageSpec {
    ImageSpec::Haze {
        width: 128,
        height: 256,
        t_min: 1.0,
        t_max: 1.0,
        a: HAZE_A,
        sky_rows: 32,
        cell: 8,
    }
}

/// The images every whole-image property test runs over.
fn property_images() -> Vec<(&'static str, ImageSpec)> {
    vec![
        ("step-high", step_high()),
        ("step-low", step_low()),
        ("step-vertical", step_vertical()),
        ("ramp8", ramp8()),
        ("noise-020", noise(0.02, 7)),
        ("patch", patch()),
        ("grey-ramp", grey_ramp()),
        ("haze-flat", haze_flat()),
    ]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn params(texture: f64, clarity: f64, dehaze: f64) -> PresenceParams {
    PresenceParams {
        texture,
        clarity,
        dehaze,
    }
}

fn apply(spec: &ImageSpec, p: PresenceParams, long_side: i64) -> (Rgb, Rgb) {
    let input = spec.build();
    let output = presence::apply_presence(&input, p, long_side);
    (input, output)
}

fn encoded_row(image: &Rgb, y: i64) -> Vec<f64> {
    (0..image.frame_width())
        .map(|x| encode_srgb_extended(luminance(image.get(x, y))))
        .collect()
}

fn encoded_column(image: &Rgb, x: i64) -> Vec<f64> {
    (0..image.frame_height())
        .map(|y| encode_srgb_extended(luminance(image.get(x, y))))
        .collect()
}

fn max_abs(values: &[f64]) -> f64 {
    values.iter().fold(0.0f64, |acc, v| acc.max(v.abs()))
}

fn crop(src: &Rgb, rect: Rect) -> Rgb {
    let mut out = Rgb::zeros(src.frame_width(), src.frame_height(), rect);
    let rect = out.rect();
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            out.set(x, y, src.get(x, y));
        }
    }
    out
}

/// Evaluate the operation the way the host will: 512-style output tiles, each reading the
/// input frame through the units' declared halos with edge clamping, and nothing else.
///
/// Every plane in the reference panics on a read outside the rectangle it holds, so this
/// function cannot quietly succeed with an insufficient halo: it either matches the
/// whole-frame evaluation bit for bit or it panics.
fn tiled_presence(src: &Rgb, p: PresenceParams, long_side: i64, tile: i64) -> Rgb {
    let frame = src.frame_rect();
    let mut out = Rgb::zeros(src.frame_width(), src.frame_height(), frame);
    let atmosphere = if p.dehaze == 0.0 {
        Atmosphere([0.0; 3])
    } else {
        presence::estimate_atmosphere(src)
    };
    let halo_dehaze = if p.dehaze == 0.0 {
        0
    } else {
        presence::dehaze_halo(long_side)
    };
    let halo_texture = if p.texture == 0.0 {
        0
    } else {
        presence::texture_halo(long_side)
    };
    let halo_clarity = if p.clarity == 0.0 {
        0
    } else {
        presence::clarity_halo(long_side)
    };

    let mut y0 = 0;
    while y0 < frame.y1 {
        let mut x0 = 0;
        while x0 < frame.x1 {
            let tile_rect = Rect::new(x0, y0, (x0 + tile).min(frame.x1), (y0 + tile).min(frame.y1));
            let texture_out = tile_rect.expand(halo_clarity).clip(frame);
            let dehaze_out = texture_out.expand(halo_texture).clip(frame);
            let input_rect = dehaze_out.expand(halo_dehaze).clip(frame);

            let region = crop(src, input_rect);
            let after_dehaze = if p.dehaze == 0.0 {
                crop(&region, dehaze_out)
            } else {
                presence::dehaze_region(&region, p.dehaze, atmosphere, long_side, dehaze_out)
            };
            let after_texture =
                presence::texture_region(&after_dehaze, p.texture, long_side, texture_out);
            let after_clarity =
                presence::clarity_region(&after_texture, p.clarity, long_side, tile_rect);

            for y in tile_rect.y0..tile_rect.y1 {
                for x in tile_rect.x0..tile_rect.x1 {
                    out.set(x, y, after_clarity.get(x, y));
                }
            }
            x0 += tile;
        }
        y0 += tile;
    }
    out
}

// ---------------------------------------------------------------------------
// Frozen properties
// ---------------------------------------------------------------------------

#[test]
fn every_unit_at_zero_is_the_exact_identity() {
    for (name, spec) in property_images() {
        let input = spec.build();
        for p in [
            params(0.0, 0.0, 0.0),
            params(0.0, 0.0, 0.0),
            params(0.0, 50.0, 0.0),
            params(0.0, 0.0, 50.0),
            params(50.0, 0.0, 0.0),
        ] {
            // Only the units at zero are checked for exactness; a nonzero unit is allowed to
            // change the image, so compare each zero unit in isolation.
            let single = [
                params(p.texture, 0.0, 0.0),
                params(0.0, p.clarity, 0.0),
                params(0.0, 0.0, p.dehaze),
            ];
            for unit in single {
                if unit.is_neutral() {
                    let output = presence::apply_presence(&input, unit, MEASURE_LONG_SIDE);
                    let rect = input.frame_rect();
                    for y in rect.y0..rect.y1 {
                        for x in rect.x0..rect.x1 {
                            assert_eq!(
                                output.get(x, y),
                                input.get(x, y),
                                "{name}: neutral {unit:?} changed ({x}, {y})"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn the_declared_halos_make_tiled_evaluation_bit_identical() {
    let cases = [
        ("step-high", step_high(), params(100.0, 100.0, 0.0)),
        ("step-high", step_high(), params(-100.0, -100.0, 0.0)),
        ("patch", patch(), params(60.0, 40.0, 0.0)),
        ("haze-flat", haze_flat(), params(0.0, 0.0, 100.0)),
        ("haze-flat", haze_flat(), params(35.0, -45.0, 80.0)),
        ("haze-ramp", haze_ramp(), params(0.0, 0.0, -100.0)),
    ];
    for (name, spec, p) in cases {
        let input = spec.build();
        let whole = presence::apply_presence(&input, p, MEASURE_LONG_SIDE);
        for tile in [16, 37, 64] {
            let tiled = tiled_presence(&input, p, MEASURE_LONG_SIDE, tile);
            let rect = input.frame_rect();
            for y in rect.y0..rect.y1 {
                for x in rect.x0..rect.x1 {
                    assert_eq!(
                        tiled.get(x, y),
                        whole.get(x, y),
                        "{name} {p:?} tile {tile}: ({x}, {y}) differs from the whole frame"
                    );
                }
            }
        }
    }
}

#[test]
fn halo_formulas_match_the_study_note_and_fit_the_host_bound() {
    let rows = [
        (480, 4, 23, 19, 46),
        (6000, 8, 199, 67, 274),
        (10000, 14, 327, 107, 448),
    ];
    for (long_side, texture, clarity, dehaze, sum) in rows {
        assert_eq!(
            presence::texture_halo(long_side),
            texture,
            "texture {long_side}"
        );
        assert_eq!(
            presence::clarity_halo(long_side),
            clarity,
            "clarity {long_side}"
        );
        assert_eq!(
            presence::dehaze_halo(long_side),
            dehaze,
            "dehaze {long_side}"
        );
        assert_eq!(presence::presence_halo(long_side), sum, "sum {long_side}");
    }
    // The host bounds the summed halo of the operation's units at 512 pixels at any stage
    // size it accepts (64 MP, at most 16384 px per side).
    for long_side in [1, 480, 2000, 6000, 10000, 16384] {
        assert!(
            presence::presence_halo(long_side) <= 512,
            "summed halo at long side {long_side} is {}",
            presence::presence_halo(long_side)
        );
    }
}

#[test]
fn texture_and_clarity_keep_achromatic_pixels_achromatic() {
    for (name, spec) in [("grey-ramp", grey_ramp()), ("step-high", step_high())] {
        for p in [
            params(100.0, 0.0, 0.0),
            params(-100.0, 0.0, 0.0),
            params(0.0, 100.0, 0.0),
            params(0.0, -100.0, 0.0),
            params(100.0, 100.0, 0.0),
        ] {
            let (_, output) = apply(&spec, p, MEASURE_LONG_SIDE);
            let rect = output.frame_rect();
            for y in rect.y0..rect.y1 {
                for x in rect.x0..rect.x1 {
                    let pixel = output.get(x, y);
                    assert_eq!(pixel[0], pixel[1], "{name} {p:?} at ({x}, {y})");
                    assert_eq!(pixel[1], pixel[2], "{name} {p:?} at ({x}, {y})");
                }
            }
        }
    }
}

#[test]
fn texture_and_clarity_preserve_channel_ratios() {
    let spec = patch();
    for p in [
        params(100.0, 0.0, 0.0),
        params(0.0, 100.0, 0.0),
        params(-100.0, -100.0, 0.0),
    ] {
        let (input, output) = apply(&spec, p, MEASURE_LONG_SIDE);
        let rect = input.frame_rect();
        let mut worst: f64 = 0.0;
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                let before = input.get(x, y);
                let after = output.get(x, y);
                // The luminance-ratio reconstruction scales all three channels by one
                // factor, so every cross ratio is preserved exactly up to rounding.
                let left = after[0] * before[1] - after[1] * before[0];
                let right = after[1] * before[2] - after[2] * before[1];
                worst = worst.max(left.abs()).max(right.abs());
            }
        }
        assert!(worst < 1e-15, "{p:?}: worst cross-ratio residual {worst}");
    }
}

#[test]
fn texture_and_clarity_keep_encoded_luminance_inside_the_unit_range() {
    for (name, spec) in [
        ("patch", patch()),
        ("step-high", step_high()),
        ("grey-ramp", grey_ramp()),
        ("noise-080", noise(0.08, 11)),
    ] {
        for p in [
            params(100.0, 100.0, 0.0),
            params(-100.0, -100.0, 0.0),
            params(100.0, -100.0, 0.0),
        ] {
            let (_, output) = apply(&spec, p, MEASURE_LONG_SIDE);
            let rect = output.frame_rect();
            for y in rect.y0..rect.y1 {
                for x in rect.x0..rect.x1 {
                    let encoded = encode_srgb_extended(luminance(output.get(x, y)));
                    assert!(
                        (-1e-12..=1.0 + 1e-12).contains(&encoded),
                        "{name} {p:?} at ({x}, {y}): encoded luminance {encoded} left [0, 1]"
                    );
                }
            }
        }
    }
}

#[test]
fn every_unit_is_monotone_in_its_amount() {
    let spec = patch();
    let input = spec.build();
    let samples = [(11, 17), (40, 40), (63, 21), (80, 90)];
    let amounts = [-100.0, -75.0, -50.0, -25.0, 0.0, 25.0, 50.0, 75.0, 100.0];

    // Texture and clarity: the encoded excursion at a pixel grows monotonically with the
    // amount, in the direction that pixel's own band or residual sign gives it. The sign is
    // read once at +100 and the sweep is then required to be nondecreasing in that
    // orientation.
    for unit in 0..2 {
        let unit_params = |amount: f64| {
            if unit == 0 {
                params(amount, 0.0, 0.0)
            } else {
                params(0.0, amount, 0.0)
            }
        };
        let extreme = presence::apply_presence(&input, unit_params(100.0), MEASURE_LONG_SIDE);
        let sign: Vec<f64> = samples
            .iter()
            .map(|&(x, y)| {
                let before = encode_srgb_extended(luminance(input.get(x, y)));
                let after = encode_srgb_extended(luminance(extreme.get(x, y)));
                if after - before >= 0.0 { 1.0 } else { -1.0 }
            })
            .collect();

        let mut previous = vec![f64::NEG_INFINITY; samples.len()];
        for amount in amounts {
            let output = presence::apply_presence(&input, unit_params(amount), MEASURE_LONG_SIDE);
            for (index, &(x, y)) in samples.iter().enumerate() {
                let before = encode_srgb_extended(luminance(input.get(x, y)));
                let after = encode_srgb_extended(luminance(output.get(x, y)));
                let oriented = (after - before) * sign[index];
                assert!(
                    oriented >= previous[index] - 1e-15,
                    "unit {unit} sample {index}: {oriented} at amount {amount} is below {} at \
                     the previous amount",
                    previous[index]
                );
                previous[index] = oriented;
            }
        }
    }

    // Dehaze: the distance from the atmospheric light grows monotonically with the amount
    // across the whole range, negative through positive.
    let hazed = haze_flat().build();
    let atmosphere = presence::estimate_atmosphere(&hazed).0;
    let mut previous = vec![f64::NEG_INFINITY; samples.len()];
    for amount in amounts {
        let output = presence::apply_presence(&hazed, params(0.0, 0.0, amount), MEASURE_LONG_SIDE);
        for (index, &(x, y)) in samples.iter().enumerate() {
            let sample_y = y + 120;
            let before = hazed.get(x, sample_y);
            let after = output.get(x, sample_y);
            let reference = before[0] - atmosphere[0];
            let ratio = if reference.abs() < 1e-9 {
                0.0
            } else {
                (after[0] - atmosphere[0]) / reference
            };
            assert!(
                ratio >= previous[index] - 1e-12,
                "dehaze sample {index}: ratio {ratio} at amount {amount} is below {}",
                previous[index]
            );
            previous[index] = ratio;
        }
    }
}

#[test]
fn every_output_is_finite() {
    for (name, spec) in property_images() {
        for p in [
            params(100.0, 100.0, 100.0),
            params(-100.0, -100.0, -100.0),
            params(100.0, -100.0, 100.0),
            params(-100.0, 100.0, -100.0),
        ] {
            let (_, output) = apply(&spec, p, MEASURE_LONG_SIDE);
            assert!(
                output.is_finite(),
                "{name} {p:?} produced a non-finite value"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Measurements
// ---------------------------------------------------------------------------

/// The maximum encoded excursion a unit adds on either side of a step edge, and the width in
/// pixels over which it is visible (`|delta| > 0.001` encoded, about a quarter of an 8-bit
/// code).
struct EdgeResponse {
    overshoot: f64,
    halo_width: f64,
}

fn edge_response(spec: &ImageSpec, p: PresenceParams, vertical: bool) -> EdgeResponse {
    let (input, output) = apply(spec, p, MEASURE_LONG_SIDE);
    let (before, after) = if vertical {
        (encoded_column(&input, 4), encoded_column(&output, 4))
    } else {
        (encoded_row(&input, 4), encoded_row(&output, 4))
    };
    let deltas: Vec<f64> = before
        .iter()
        .zip(after.iter())
        .map(|(b, a)| a - b)
        .collect();
    let edge = (before.len() / 2) as i64;
    let mut width = 0i64;
    for (index, delta) in deltas.iter().enumerate() {
        if delta.abs() > 0.001 {
            width = width.max(((index as i64) - edge).abs().max(edge - (index as i64)));
        }
    }
    // Width is measured as the furthest affected pixel from the edge on either side.
    let mut furthest = 0i64;
    for (index, delta) in deltas.iter().enumerate() {
        if delta.abs() > 0.001 {
            furthest = furthest.max(((index as i64) - edge).abs());
        }
    }
    let _ = width;
    EdgeResponse {
        overshoot: max_abs(&deltas),
        halo_width: furthest as f64,
    }
}

/// The encoded peak-to-trough gain a unit applies to a sinusoid of the given period,
/// measured away from the frame edges.
fn band_gain(period: f64, p: PresenceParams, width: i64) -> f64 {
    let spec = ImageSpec::Sine {
        width,
        height: 8,
        mean: 0.5,
        amplitude: 0.005,
        period,
        vertical: false,
    };
    let (input, output) = apply(&spec, p, MEASURE_LONG_SIDE);
    let before = encoded_row(&input, 4);
    let after = encoded_row(&output, 4);
    let margin = (width / 4) as usize;
    let range = |values: &[f64]| {
        let slice = &values[margin..values.len() - margin];
        let max = slice.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let min = slice.iter().cloned().fold(f64::INFINITY, f64::min);
        max - min
    };
    range(&after) / range(&before)
}

/// The largest jump, in 8-bit output codes, a unit introduces between adjacent columns of a
/// ramp whose input steps are exactly one code every `repeat` columns.
fn max_code_step(spec: &ImageSpec, p: PresenceParams) -> f64 {
    let (_, output) = apply(spec, p, MEASURE_LONG_SIDE);
    let codes: Vec<i64> = (0..output.frame_width())
        .map(|x| linear_to_code(luminance(output.get(x, 4))) as i64)
        .collect();
    let margin = 16usize;
    let mut worst = 0i64;
    for window in codes[margin..codes.len() - margin].windows(2) {
        worst = worst.max((window[1] - window[0]).abs());
    }
    worst as f64
}

/// The ratio of output to input standard deviation of encoded luminance.
fn noise_amplification(amplitude: f64, seed: u64, p: PresenceParams) -> f64 {
    let spec = noise(amplitude, seed);
    let (input, output) = apply(&spec, p, MEASURE_LONG_SIDE);
    let deviation = |image: &Rgb| {
        let rect = image.frame_rect();
        let mut values = Vec::new();
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                values.push(encode_srgb_extended(luminance(image.get(x, y))));
            }
        }
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        (values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / values.len() as f64).sqrt()
    };
    deviation(&output) / deviation(&input)
}

/// The largest linear-light error between a dehazed synthetic scene and the clear scene it
/// was built from, over the region the estimator is not mixing across the sky boundary in.
fn haze_recovery_error(spec: &ImageSpec, exclude_rows: i64) -> f64 {
    let hazed = spec.build();
    let clear = match *spec {
        ImageSpec::Haze {
            width,
            height,
            a,
            sky_rows,
            cell,
            ..
        } => ImageSpec::Haze {
            width,
            height,
            t_min: 1.0,
            t_max: 1.0,
            a,
            sky_rows,
            cell,
        }
        .build(),
        _ => panic!("haze_recovery_error needs a Haze spec"),
    };
    let output = presence::apply_presence(&hazed, params(0.0, 0.0, 100.0), MEASURE_LONG_SIDE);
    let rect = output.frame_rect();
    let mut worst: f64 = 0.0;
    for y in (rect.y0 + exclude_rows)..rect.y1 {
        for x in rect.x0..rect.x1 {
            let recovered = output.get(x, y);
            let truth = clear.get(x, y);
            for channel in 0..3 {
                worst = worst.max((recovered[channel] - truth[channel]).abs());
            }
        }
    }
    worst
}

// ---------------------------------------------------------------------------
// Frozen bounds, asserted directly
// ---------------------------------------------------------------------------

#[test]
fn step_edge_overshoot_stays_inside_the_frozen_limits() {
    for (name, spec, vertical) in [
        ("step-high", step_high(), false),
        ("step-low", step_low(), false),
        ("step-vertical", step_vertical(), true),
    ] {
        for amount in [100.0, -100.0] {
            let texture = edge_response(&spec, params(amount, 0.0, 0.0), vertical);
            assert!(
                texture.overshoot <= presence::LIMIT_TEXTURE + 1e-12,
                "{name} texture {amount}: overshoot {} exceeds LIMIT_TEXTURE",
                texture.overshoot
            );
            assert!(
                texture.halo_width <= presence::texture_halo(MEASURE_LONG_SIDE) as f64,
                "{name} texture {amount}: halo {} exceeds the declared halo",
                texture.halo_width
            );

            let clarity = edge_response(&spec, params(0.0, amount, 0.0), vertical);
            assert!(
                clarity.overshoot <= presence::LIMIT_CLARITY + 1e-12,
                "{name} clarity {amount}: overshoot {} exceeds LIMIT_CLARITY",
                clarity.overshoot
            );
            assert!(
                clarity.halo_width <= presence::clarity_halo(MEASURE_LONG_SIDE) as f64,
                "{name} clarity {amount}: halo {} exceeds the declared halo",
                clarity.halo_width
            );
        }
    }
}

#[test]
fn texture_is_a_medium_frequency_band() {
    let peak = band_gain(10.0, params(100.0, 0.0, 0.0), 512);
    // Periods of 2 and 3 pixels are not usable probes on an integer grid: a period-2
    // sinusoid samples to a constant and a period-3 one is annihilated exactly by the
    // three-tap fine filter, so the finest probe the study quotes is 4 pixels.
    let finest = band_gain(4.0, params(100.0, 0.0, 0.0), 512);
    let coarse = band_gain(128.0, params(100.0, 0.0, 0.0), 1024);
    assert!(
        peak > 3.0,
        "texture +100 should boost its own band strongly: {peak}"
    );
    assert!(
        finest < 0.5 * peak,
        "the finest structure should be well below the peak: {finest} vs {peak}"
    );
    assert!(
        coarse < 1.1,
        "very coarse structure should be nearly unchanged: {coarse}"
    );
    let attenuated = band_gain(10.0, params(-100.0, 0.0, 0.0), 512);
    assert!(
        attenuated < 0.35,
        "texture -100 should remove most of its band: {attenuated}"
    );
}

#[test]
fn clarity_is_a_broader_band_than_texture() {
    let clarity_mid = band_gain(64.0, params(0.0, 100.0, 0.0), 1024);
    let texture_mid = band_gain(64.0, params(100.0, 0.0, 0.0), 1024);
    assert!(
        clarity_mid > texture_mid,
        "clarity should dominate texture at 64 px: {clarity_mid} vs {texture_mid}"
    );
    let clarity_coarse = band_gain(1024.0, params(0.0, 100.0, 0.0), 4096);
    assert!(
        clarity_coarse < 1.35,
        "clarity should leave structure far above its base nearly unchanged: {clarity_coarse}"
    );
}

#[test]
fn clarity_does_not_band_an_eight_bit_ramp() {
    let spec = ramp8();
    let step = max_code_step(&spec, params(0.0, 100.0, 0.0));
    assert!(
        step <= 2.0,
        "clarity +100 introduced a {step}-code step on a one-code-per-four-column ramp"
    );
    let texture_step = max_code_step(&spec, params(100.0, 0.0, 0.0));
    assert!(
        texture_step <= 2.0,
        "texture +100 introduced a {texture_step}-code step"
    );
    // Amplifying local contrast necessarily amplifies the quantization steps already in an
    // 8-bit source; the frozen bound is what the two units together produce on a ramp whose
    // own steps are one code.
    let both = max_code_step(&spec, params(100.0, 100.0, 0.0));
    assert!(
        both <= 3.0,
        "texture and clarity together introduced a {both}-code step"
    );
}

#[test]
fn dehaze_recovers_a_synthetic_haze_built_from_the_forward_model() {
    // The whole scene region: with a constant transmission the estimator is exact, so the
    // only excluded band is where the refinement mixes across the sky boundary.
    let excluded = 32 + presence::dehaze_halo(MEASURE_LONG_SIDE);
    let flat = haze_recovery_error(&haze_flat(), excluded);
    assert!(
        flat < 1e-9,
        "constant-transmission recovery error {flat} is not float noise"
    );

    // A transmission that varies across the frame is over-estimated by the dark channel's
    // own min-filter reach, which leaves part of the veil in place. Both bounds are the
    // measured figures the study note records, not targets the estimator meets by design.
    let gentle = haze_recovery_error(&haze_ramp_gentle(), excluded);
    assert!(
        gentle < 0.03,
        "gently ramped transmission recovery error {gentle} exceeds the frozen bound"
    );
    let ramp = haze_recovery_error(&haze_ramp(), excluded);
    assert!(
        ramp < 0.35,
        "steeply ramped transmission recovery error {ramp} exceeds the frozen bound"
    );

    // The atmospheric light is recovered exactly from the synthetic sky.
    let estimated = presence::estimate_atmosphere(&haze_flat().build()).0;
    for channel in 0..3 {
        assert!(
            (estimated[channel] - HAZE_A[channel]).abs() < 1e-12,
            "channel {channel}: estimated A {} vs true {}",
            estimated[channel],
            HAZE_A[channel]
        );
    }
}

#[test]
fn negative_dehaze_adds_the_forward_models_veil() {
    let clear = haze_clear().build();
    let output = presence::apply_presence(&clear, params(0.0, 0.0, -100.0), MEASURE_LONG_SIDE);
    let a = presence::estimate_atmosphere(&clear).0;
    let rect = clear.frame_rect();
    let mut worst_spread: f64 = 0.0;
    let mut minimum_t = f64::INFINITY;
    let mut maximum_t = f64::NEG_INFINITY;
    for y in (rect.y0 + 64)..rect.y1 {
        for x in rect.x0..rect.x1 {
            let before = clear.get(x, y);
            let after = output.get(x, y);
            let mut implied = Vec::new();
            for channel in 0..3 {
                let denominator = before[channel] - a[channel];
                if denominator.abs() > 0.05 {
                    implied.push((after[channel] - a[channel]) / denominator);
                }
            }
            if implied.len() < 2 {
                continue;
            }
            let max = implied.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let min = implied.iter().cloned().fold(f64::INFINITY, f64::min);
            worst_spread = worst_spread.max(max - min);
            minimum_t = minimum_t.min(min);
            maximum_t = maximum_t.max(max);
        }
    }
    assert!(
        worst_spread < 1e-12,
        "the implied transmission differs between channels by {worst_spread}: the veil is not \
         one transmission applied to every channel"
    );
    assert!(
        (T_FLOOR..=1.0).contains(&minimum_t) && maximum_t <= 1.0,
        "implied transmission left [T_FLOOR, 1]: {minimum_t}..{maximum_t}"
    );
    // A haze-free scene has an estimated transmission of 1, so the added veil is exactly the
    // frozen uniform factor.
    assert!(
        (maximum_t - (1.0 - presence::VEIL_MAX)).abs() < 1e-9,
        "the veil on a haze-free scene should be 1 - VEIL_MAX, measured {maximum_t}"
    );
}

// ---------------------------------------------------------------------------
// The f32 evidence behind the frozen production tolerance
// ---------------------------------------------------------------------------

fn box_mean_f32(src: &[f32], width: usize, height: usize, r: i64) -> Vec<f32> {
    let mut horizontal = vec![0.0f32; width * height];
    let n = (2 * r + 1) as f32;
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            for dx in -r..=r {
                let sx = ((x as i64) + dx).clamp(0, width as i64 - 1) as usize;
                sum += src[y * width + sx];
            }
            horizontal[y * width + x] = sum / n;
        }
    }
    let mut out = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            for dy in -r..=r {
                let sy = ((y as i64) + dy).clamp(0, height as i64 - 1) as usize;
                sum += horizontal[sy * width + x];
            }
            out[y * width + x] = sum / n;
        }
    }
    out
}

fn guided_self_f32(src: &[f32], width: usize, height: usize, r: i64, eps: f32) -> Vec<f32> {
    let squared: Vec<f32> = src.iter().map(|v| v * v).collect();
    let mean = box_mean_f32(src, width, height, r);
    let mean_squared = box_mean_f32(&squared, width, height, r);
    let mut a = vec![0.0f32; width * height];
    let mut b = vec![0.0f32; width * height];
    for index in 0..width * height {
        let m = mean[index];
        let variance = (mean_squared[index] - m * m).max(0.0);
        let a_value = variance / (variance + eps);
        a[index] = a_value;
        b[index] = (1.0 - a_value) * m;
    }
    let mean_a = box_mean_f32(&a, width, height, r);
    let mean_b = box_mean_f32(&b, width, height, r);
    (0..width * height)
        .map(|index| mean_a[index] * src[index] + mean_b[index])
        .collect()
}

/// The largest difference between the f64 reference's self-guided filter and the same
/// filter evaluated in f32, on the encoded luminance of a textured patch.
fn f32_guided_error(r: i64, eps: f64) -> f64 {
    let image = patch().build();
    let rect = image.frame_rect();
    let width = rect.width();
    let height = rect.height();
    let encoded = image.encoded_luminance(rect);
    let flat: Vec<f32> = (0..height)
        .flat_map(|y| (0..width).map(move |x| (y, x)))
        .map(|(y, x)| encoded.get(x, y) as f32)
        .collect();
    let f32_result = guided_self_f32(&flat, width as usize, height as usize, r, eps as f32);
    let f64_result = presence::guided_self(&encoded, r, eps, rect);
    let mut worst: f64 = 0.0;
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;
            worst = worst.max((f64::from(f32_result[index]) - f64_result.get(x, y)).abs());
        }
    }
    worst
}

#[test]
fn the_frozen_production_tolerance_covers_the_measured_f32_filter_error() {
    let fine = f32_guided_error(
        presence::texture_fine_radius(MEASURE_LONG_SIDE),
        presence::EPS_TEXTURE,
    );
    let coarse = f32_guided_error(
        presence::texture_coarse_radius(MEASURE_LONG_SIDE),
        presence::EPS_TEXTURE,
    );
    let broad = f32_guided_error(
        presence::clarity_reduced_radius(MEASURE_LONG_SIDE),
        presence::EPS_CLARITY,
    );
    let worst_filter = fine.max(coarse).max(broad);

    // Two propagation paths set the frozen tolerance, both driven by this measured filter
    // error. Texture and Clarity carry it through their gain (at most 3) and then through
    // the encoded-to-linear slope, which is at most 2.4/1.055 = 2.28 at encoded 1.0.
    let luminance_path = worst_filter * 3.0 * 2.28;
    // Dehaze carries an error in the transmission through `(I - A)/t`, whose sensitivity is
    // `|I - A| / t^2`; the worst case is the transmission floor with a full-range veil.
    let dehaze_path = worst_filter * 1.0 / (T_FLOOR * T_FLOOR);

    let tolerance = 2e-4;
    assert!(
        luminance_path < tolerance,
        "the measured f32 filter error propagates to {luminance_path} through the luminance \
         units, which the frozen {tolerance} + {tolerance}*|reference| tolerance would not cover"
    );
    assert!(
        dehaze_path < tolerance,
        "the measured f32 filter error propagates to {dehaze_path} through dehaze's division \
         by the transmission floor, which the frozen tolerance would not cover"
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct CaseParams {
    texture: f64,
    clarity: f64,
    dehaze: f64,
}

impl From<CaseParams> for PresenceParams {
    fn from(value: CaseParams) -> Self {
        PresenceParams {
            texture: value.texture,
            clarity: value.clarity,
            dehaze: value.dehaze,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Case {
    name: String,
    image: ImageSpec,
    /// The stage long side the radii are scaled by, which need not be the fixture's own
    /// long side: a fixture is a region of a stage, and the radii belong to the stage.
    long_side: i64,
    params: CaseParams,
    /// Row-major linear-sRGB output at full f64 precision, unclamped.
    expected: Vec<[f64; 3]>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct CaseFile {
    generated_by: String,
    note: String,
    cases: Vec<Case>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Measurement {
    name: String,
    value: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct MeasurementFile {
    generated_by: String,
    note: String,
    measurements: Vec<Measurement>,
}

fn small_patch() -> ImageSpec {
    ImageSpec::Patch {
        width: 24,
        height: 24,
        mean: 0.3,
        seed: 4_242,
        tint: [1.0, 0.9, 0.75],
    }
}

fn small_step() -> ImageSpec {
    ImageSpec::Step {
        width: 24,
        height: 24,
        low: 0.08,
        high: 0.6,
        vertical: false,
    }
}

fn small_haze() -> ImageSpec {
    ImageSpec::Haze {
        width: 24,
        height: 24,
        t_min: 0.35,
        t_max: 0.8,
        a: HAZE_A,
        sky_rows: 4,
        cell: 4,
    }
}

fn oracle_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    let recipes: Vec<(&str, ImageSpec, i64, CaseParams)> = vec![
        (
            "patch-texture-plus100-stage6000",
            small_patch(),
            6000,
            CaseParams {
                texture: 100.0,
                clarity: 0.0,
                dehaze: 0.0,
            },
        ),
        (
            "patch-texture-minus100-stage6000",
            small_patch(),
            6000,
            CaseParams {
                texture: -100.0,
                clarity: 0.0,
                dehaze: 0.0,
            },
        ),
        (
            "patch-clarity-plus100-stage6000",
            small_patch(),
            6000,
            CaseParams {
                texture: 0.0,
                clarity: 100.0,
                dehaze: 0.0,
            },
        ),
        (
            "patch-clarity-minus60-stage6000",
            small_patch(),
            6000,
            CaseParams {
                texture: 0.0,
                clarity: -60.0,
                dehaze: 0.0,
            },
        ),
        (
            "patch-all-stage24",
            small_patch(),
            24,
            CaseParams {
                texture: 45.0,
                clarity: -35.0,
                dehaze: 70.0,
            },
        ),
        (
            "step-texture-plus100-stage6000",
            small_step(),
            6000,
            CaseParams {
                texture: 100.0,
                clarity: 0.0,
                dehaze: 0.0,
            },
        ),
        (
            "step-clarity-plus100-stage24",
            small_step(),
            24,
            CaseParams {
                texture: 0.0,
                clarity: 100.0,
                dehaze: 0.0,
            },
        ),
        (
            "haze-dehaze-plus100-stage6000",
            small_haze(),
            6000,
            CaseParams {
                texture: 0.0,
                clarity: 0.0,
                dehaze: 100.0,
            },
        ),
        (
            "haze-dehaze-minus100-stage24",
            small_haze(),
            24,
            CaseParams {
                texture: 0.0,
                clarity: 0.0,
                dehaze: -100.0,
            },
        ),
        (
            "haze-all-stage6000",
            small_haze(),
            6000,
            CaseParams {
                texture: 60.0,
                clarity: 40.0,
                dehaze: 55.0,
            },
        ),
    ];

    for (name, image, long_side, case_params) in recipes {
        let input = image.build();
        let output = presence::apply_presence(&input, case_params.into(), long_side);
        let rect = output.frame_rect();
        let mut expected = Vec::with_capacity((rect.width() * rect.height()) as usize);
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                expected.push(output.get(x, y));
            }
        }
        cases.push(Case {
            name: name.to_string(),
            image,
            long_side,
            params: case_params,
            expected,
        });
    }
    cases
}

fn measure_all() -> Vec<Measurement> {
    let mut measurements = Vec::new();
    let mut push = |name: &str, value: f64| {
        measurements.push(Measurement {
            name: name.to_string(),
            value,
        });
    };

    for (label, spec, vertical) in [
        ("step_high", step_high(), false),
        ("step_low", step_low(), false),
        ("step_vertical", step_vertical(), true),
    ] {
        for (suffix, amount) in [("plus100", 100.0), ("minus100", -100.0)] {
            let texture = edge_response(&spec, params(amount, 0.0, 0.0), vertical);
            push(
                &format!("texture_{label}_{suffix}_overshoot"),
                texture.overshoot,
            );
            push(
                &format!("texture_{label}_{suffix}_halo_px"),
                texture.halo_width,
            );
            let clarity = edge_response(&spec, params(0.0, amount, 0.0), vertical);
            push(
                &format!("clarity_{label}_{suffix}_overshoot"),
                clarity.overshoot,
            );
            push(
                &format!("clarity_{label}_{suffix}_halo_px"),
                clarity.halo_width,
            );
        }
    }

    for period in [2.5f64, 4.0, 6.0, 8.0, 10.0, 12.0, 16.0, 32.0, 64.0, 128.0] {
        let width = if period > 32.0 { 1024 } else { 512 };
        let label = format!("{}", (period * 10.0).round() as i64);
        push(
            &format!("texture_band_gain_p{label}_plus100"),
            band_gain(period, params(100.0, 0.0, 0.0), width),
        );
        push(
            &format!("texture_band_gain_p{label}_minus100"),
            band_gain(period, params(-100.0, 0.0, 0.0), width),
        );
    }

    for period in [8.0f64, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0] {
        let width = (period as i64 * 4).max(512);
        push(
            &format!("clarity_band_gain_p{}_plus100", period as i64),
            band_gain(period, params(0.0, 100.0, 0.0), width),
        );
        push(
            &format!("clarity_band_gain_p{}_minus100", period as i64),
            band_gain(period, params(0.0, -100.0, 0.0), width),
        );
    }

    let ramp = ramp8();
    push(
        "ramp8_max_code_step_clarity_plus100",
        max_code_step(&ramp, params(0.0, 100.0, 0.0)),
    );
    push(
        "ramp8_max_code_step_texture_plus100",
        max_code_step(&ramp, params(100.0, 0.0, 0.0)),
    );
    push(
        "ramp8_max_code_step_both_plus100",
        max_code_step(&ramp, params(100.0, 100.0, 0.0)),
    );

    for (label, amplitude, seed) in [("0005", 0.005, 3), ("0020", 0.02, 7), ("0080", 0.08, 11)] {
        push(
            &format!("noise_{label}_texture_plus100"),
            noise_amplification(amplitude, seed, params(100.0, 0.0, 0.0)),
        );
        push(
            &format!("noise_{label}_texture_minus100"),
            noise_amplification(amplitude, seed, params(-100.0, 0.0, 0.0)),
        );
        push(
            &format!("noise_{label}_clarity_plus100"),
            noise_amplification(amplitude, seed, params(0.0, 100.0, 0.0)),
        );
    }

    let excluded = 32 + presence::dehaze_halo(MEASURE_LONG_SIDE);
    push(
        "haze_flat_recovery_max_error",
        haze_recovery_error(&haze_flat(), excluded),
    );
    push(
        "haze_ramp_recovery_max_error",
        haze_recovery_error(&haze_ramp(), excluded),
    );
    push(
        "haze_ramp_gentle_recovery_max_error",
        haze_recovery_error(&haze_ramp_gentle(), excluded),
    );

    push(
        "f32_guided_error_r_fine",
        f32_guided_error(
            presence::texture_fine_radius(MEASURE_LONG_SIDE),
            presence::EPS_TEXTURE,
        ),
    );
    push(
        "f32_guided_error_r_coarse",
        f32_guided_error(
            presence::texture_coarse_radius(MEASURE_LONG_SIDE),
            presence::EPS_TEXTURE,
        ),
    );
    push(
        "f32_guided_error_r_clarity_reduced",
        f32_guided_error(
            presence::clarity_reduced_radius(MEASURE_LONG_SIDE),
            presence::EPS_CLARITY,
        ),
    );

    measurements
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("presence")
}

#[test]
#[ignore = "run explicitly to (re)generate fixtures/presence/*.json from the frozen reference"]
fn generate_presence_fixtures() {
    let directory = fixture_dir();
    fs::create_dir_all(&directory).expect("create fixtures/presence");

    let cases = CaseFile {
        generated_by:
            "crates/luxforge-reference/tests/studies/presence.rs generate_presence_fixtures"
                .to_string(),
        note: "Independent f64 reference (crates/luxforge-reference/src/presence.rs). Each case names a \
               deterministic image spec, the stage long side the radii scale by, the three \
               amounts, and the resulting linear sRGB output at full f64 precision, unclamped, \
               in the frozen unit order dehaze, texture, clarity. Production is compared \
               against this file within the tolerance frozen in docs/design/presence-study.md."
            .to_string(),
        cases: oracle_cases(),
    };
    let path = directory.join("cases.json");
    fs::write(
        &path,
        serde_json::to_string(&cases).expect("serialize cases"),
    )
    .expect("write cases.json");
    println!("wrote {} cases to {}", cases.cases.len(), path.display());

    let measurements = MeasurementFile {
        generated_by:
            "crates/luxforge-reference/tests/studies/presence.rs generate_presence_fixtures"
                .to_string(),
        note: "Every figure quoted in docs/design/presence-study.md, recomputed from the same \
               reference by measurements_match_reference on each test run."
            .to_string(),
        measurements: measure_all(),
    };
    let path = directory.join("measurements.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&measurements).expect("serialize measurements"),
    )
    .expect("write measurements.json");
    println!(
        "wrote {} measurements to {}",
        measurements.measurements.len(),
        path.display()
    );
}

#[test]
fn presence_fixtures_match_reference() {
    let path = fixture_dir().join("cases.json");
    let json = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{} is missing ({err}); run `cargo test --package luxforge-core \
             -p luxforge-reference --test studies -- --ignored generate_presence_fixtures` to (re)create it",
            path.display()
        )
    });
    let file: CaseFile = serde_json::from_str(&json).expect("parse cases.json");
    assert!(!file.cases.is_empty(), "cases.json has no cases");

    for case in &file.cases {
        let input = case.image.build();
        let output = presence::apply_presence(&input, case.params.into(), case.long_side);
        let rect = output.frame_rect();
        assert_eq!(
            case.expected.len() as i64,
            rect.width() * rect.height(),
            "case {}: fixture holds the wrong number of pixels",
            case.name
        );
        let mut index = 0usize;
        for y in rect.y0..rect.y1 {
            for x in rect.x0..rect.x1 {
                let actual = output.get(x, y);
                let expected = case.expected[index];
                for channel in 0..3 {
                    assert!(
                        (expected[channel] - actual[channel]).abs() < 1e-12,
                        "case {} at ({x}, {y}) channel {channel}: fixture {} but the reference \
                         now computes {} (regenerate the fixture if this is an intentional \
                         formula change)",
                        case.name,
                        expected[channel],
                        actual[channel]
                    );
                }
                index += 1;
            }
        }
    }
}

#[test]
fn measurements_match_reference() {
    let path = fixture_dir().join("measurements.json");
    let json = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "{} is missing ({err}); run `cargo test --package luxforge-core \
             -p luxforge-reference --test studies -- --ignored generate_presence_fixtures` to (re)create it",
            path.display()
        )
    });
    let file: MeasurementFile = serde_json::from_str(&json).expect("parse measurements.json");
    let recomputed = measure_all();
    assert_eq!(
        file.measurements.len(),
        recomputed.len(),
        "measurements.json has {} entries but the reference now produces {}",
        file.measurements.len(),
        recomputed.len()
    );
    for (stored, fresh) in file.measurements.iter().zip(recomputed.iter()) {
        assert_eq!(stored.name, fresh.name, "measurement order changed");
        assert!(
            (stored.value - fresh.value).abs() <= 1e-12 + 1e-12 * stored.value.abs(),
            "measurement {}: fixture {} but the reference now computes {}",
            stored.name,
            stored.value,
            fresh.value
        );
    }
}
