//! Independent proofs for grain's frozen hash, field and application, the reduced-stage rule's
//! statistics, and the oracle fixtures production is checked against.
//!
//! This study shares no code with `luxforge-core`. The frozen equations live in
//! `crates/luxforge-reference/src/grain.rs`; `docs/design/grain-study.md` writes them out and
//! records the measurements below, which `print_the_study_measurements` reproduces.

use super::approximately_equal;
use luxforge_reference::SplitMix64;
use luxforge_reference::grain::{
    AMOUNT_MAX, CLUMP, COARSE, FIELD_CLAMP, FINE, FIXED_ONE, Field, Footprint, GrainParams, SEED,
    apply, beta, cell, component_key, grain_pixel, hash, lattice, lattice_value, mix, noise, step,
};
use luxforge_reference::srgb;
use luxforge_reference::tone::luminance;
use luxforge_test_macros::slow;
use std::path::{Path, PathBuf};

// ---- Shared inputs and measures ------------------------------------------------------------------

const SLIDER_GRID: [f64; 5] = [0.0, 25.0, 50.0, 75.0, 100.0];

fn params(amount: f64, size: f64, roughness: f64) -> GrainParams {
    GrainParams {
        amount,
        size,
        roughness,
    }
}

fn grey_at(encoded: f64) -> [f64; 3] {
    [srgb::decode_encoded(encoded); 3]
}

/// Mean, standard deviation and kurtosis of the unclamped field over a `side x side` window at
/// the origin, and the share of pixels past the clamp.
fn field_moments(field: &Field, side: u32) -> (f64, f64, f64, f64) {
    let (mut s1, mut s2, mut s4, mut tail) = (0.0, 0.0, 0.0, 0u64);
    for y in 0..side {
        for x in 0..side {
            let g = field.unclamped(x, y);
            s1 += g;
            s2 += g * g;
            s4 += g.powi(4);
            if g.abs() > FIELD_CLAMP {
                tail += 1;
            }
        }
    }
    let n = f64::from(side) * f64::from(side);
    let mean = s1 / n;
    let variance = s2 / n - mean * mean;
    (
        mean,
        variance.sqrt(),
        s4 / n / (variance * variance),
        tail as f64 / n,
    )
}

/// The lag, in pixels along x, at which the field's autocorrelation first falls to one half,
/// linearly interpolated: the feature size the study reports for each Size.
fn half_correlation_lag(field: &Field, side: u32) -> f64 {
    let values = field_window(field, side);
    let at = |x: u32, y: u32| values[(y * side + x) as usize];
    let variance = values.iter().map(|v| v * v).sum::<f64>() / values.len() as f64;
    let mut previous = 1.0;
    for lag in 1..side / 4 {
        let mut sum = 0.0;
        let mut count = 0.0;
        for y in 0..side {
            for x in 0..side - lag {
                sum += at(x, y) * at(x + lag, y);
                count += 1.0;
            }
        }
        let rho = sum / count / variance;
        if rho <= 0.5 {
            return f64::from(lag - 1) + (previous - 0.5) / (previous - rho);
        }
        previous = rho;
    }
    f64::from(side / 4)
}

fn field_window(field: &Field, side: u32) -> Vec<f64> {
    (0..side)
        .flat_map(|y| (0..side).map(move |x| (x, y)))
        .map(|(x, y)| field.at(x, y))
        .collect()
}

fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -std::f64::consts::TAU / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (angle * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// The radially averaged spectrum's bands: eight bands of equal width over `[0, 0.5)` cycles a
/// pixel.
const BANDS: usize = 8;

/// The share of an `n x n` image's variance (mean removed, no window) in each radial band of its
/// periodogram, frequencies at or past 0.5 cycles a pixel (the corners) left out.
fn band_shares(image: &[f64], n: usize) -> [f64; BANDS] {
    let mean = image.iter().sum::<f64>() / image.len() as f64;
    let mut re: Vec<f64> = image.iter().map(|v| v - mean).collect();
    let mut im = vec![0.0; n * n];
    for row in 0..n {
        fft(
            &mut re[row * n..(row + 1) * n],
            &mut im[row * n..(row + 1) * n],
        );
    }
    let (mut column_re, mut column_im) = (vec![0.0; n], vec![0.0; n]);
    for column in 0..n {
        for row in 0..n {
            column_re[row] = re[row * n + column];
            column_im[row] = im[row * n + column];
        }
        fft(&mut column_re, &mut column_im);
        for row in 0..n {
            re[row * n + column] = column_re[row];
            im[row * n + column] = column_im[row];
        }
    }
    let signed = |k: usize| {
        (if k > n / 2 {
            k as f64 - n as f64
        } else {
            k as f64
        }) / n as f64
    };
    let mut sums = [0.0; BANDS];
    for ky in 0..n {
        for kx in 0..n {
            let radius = signed(kx).hypot(signed(ky));
            if (kx == 0 && ky == 0) || radius >= 0.5 {
                continue;
            }
            sums[(radius * 2.0 * BANDS as f64) as usize] +=
                re[ky * n + kx].powi(2) + im[ky * n + kx].powi(2);
        }
    }
    let total: f64 = sums.iter().sum();
    sums.map(|sum| sum / total)
}

fn deviation(values: &[f64]) -> f64 {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

/// One reduced-stage comparison on a flat grey: the full-resolution grain over `s * n` square
/// pixels reduced by the linear-light area average of each `s x s` block (the target), and the
/// reduced stage's own grain over `n x n` (the candidate), each as encoded values.
struct Reduced {
    target: Vec<f64>,
    candidate: Vec<f64>,
}

impl Reduced {
    fn deviation_ratio(&self) -> f64 {
        deviation(&self.candidate) / deviation(&self.target)
    }

    fn worst_band_difference(&self, n: usize) -> f64 {
        let target = band_shares(&self.target, n);
        let candidate = band_shares(&self.candidate, n);
        target
            .iter()
            .zip(candidate.iter())
            .map(|(t, c)| (t - c).abs())
            .fold(0.0, f64::max)
    }

    /// The encoded means' difference, in output codes.
    fn mean_difference_codes(&self) -> f64 {
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        (mean(&self.candidate) - mean(&self.target)) * 255.0
    }
}

/// The comparisons at every tone in `tones` (encoded greys), sharing one field evaluation.
fn reduced_pairs(params: &GrainParams, tones: &[f64], s: u32, n: u32) -> Vec<Reduced> {
    let beta = beta(params.amount);
    let full = Field::new(params.size, params.roughness, Footprint::FULL);
    let footprint = Footprint {
        x: f64::from(s),
        y: f64::from(s),
    };
    let reduced = Field::new(params.size, params.roughness, footprint);
    let greys: Vec<f64> = tones.iter().map(|&e| srgb::decode_encoded(e)).collect();
    let mut pairs: Vec<Reduced> = tones
        .iter()
        .map(|_| Reduced {
            target: Vec::with_capacity((n * n) as usize),
            candidate: Vec::with_capacity((n * n) as usize),
        })
        .collect();
    let block = f64::from(s * s);
    let mut sums = vec![0.0; greys.len()];
    for y in 0..n {
        for x in 0..n {
            sums.iter_mut().for_each(|sum| *sum = 0.0);
            for dy in 0..s {
                for dx in 0..s {
                    let g = full.at(x * s + dx, y * s + dy);
                    for (sum, &grey) in sums.iter_mut().zip(greys.iter()) {
                        *sum += apply([grey; 3], g, beta, full.variance)[1];
                    }
                }
            }
            let g = reduced.at(x, y);
            for ((pair, sum), &grey) in pairs.iter_mut().zip(sums.iter()).zip(greys.iter()) {
                pair.target.push(srgb::encode_extended(sum / block));
                let out = apply([grey; 3], g, beta, reduced.variance)[1];
                pair.candidate.push(srgb::encode_extended(out));
            }
        }
    }
    pairs
}

/// The mean of `rgb` under the field's own values `g` and their negations, the antithetic pairs
/// that cancel every odd moment of the field's sample: the application's own bias at that tone,
/// as the encoded difference of the linear means in output codes, per channel.
fn antithetic_shift_codes(rgb: [f64; 3], values: &[f64], beta: f64, variance: f64) -> [f64; 3] {
    let mut sums = [0.0; 3];
    for &g in values {
        let up = apply(rgb, g, beta, variance);
        let down = apply(rgb, -g, beta, variance);
        for channel in 0..3 {
            sums[channel] += 0.5 * (up[channel] + down[channel]);
        }
    }
    let n = values.len() as f64;
    [0, 1, 2].map(|c| (srgb::encode_extended(sums[c] / n) - srgb::encode_extended(rgb[c])) * 255.0)
}

// ---- Frozen statistical tolerances ---------------------------------------------------------------

/// The field's sample mean and standard deviation over a 1024 x 1024 window at the origin.
const FIELD_MEAN_BOUND: f64 = 0.06;
const FIELD_DEVIATION_BOUND: f64 = 0.02;
/// The application's bias at every tone at Amount 100, measured antithetically, in output codes.
const MEAN_SHIFT_CODES: f64 = 0.15;
/// Reduced stages against the full-resolution grain reduced by the same area average, on
/// 256 x 256 stage pixels at encoded greys 0.2, 0.5 and 0.8: the standard deviation within 5%,
/// each radial band's share of the variance within 0.08, the encoded means within 0.5 code.
const REDUCED_DEVIATION: f64 = 0.05;
const REDUCED_BAND_SHARE: f64 = 0.08;
const REDUCED_MEAN_CODES: f64 = 0.5;
const REDUCED_TONES: [f64; 3] = [0.2, 0.5, 0.8];

// ---- The hash ------------------------------------------------------------------------------------

#[test]
fn the_hash_matches_its_frozen_test_vectors() {
    // The anchors the study's table repeats, for a transcription to check first.
    assert_eq!(mix(0), 0);
    assert_eq!(mix(1), 0x6889_90C0);
    assert_eq!(mix(0xDEAD_BEEF), 0xE628_C683);
    assert_eq!(component_key(FINE), mix(SEED));
    assert_eq!(
        [FINE, COARSE, CLUMP].map(component_key),
        [0xA99C_2DFC, 0x80D1_5DEC, 0x4AC8_A78E]
    );
    assert_eq!(hash(FINE, 0, 0), mix(mix(component_key(FINE))));
    assert_eq!(hash(FINE, 0, 0), 0x5B3A_2C9D);
    assert_eq!(lattice_value(0x5B3A_2C9D), -0.469_360_351_562_5);
    assert_eq!(hash(CLUMP, 123_456, 654_321), 0xFF65_CB59);
    assert_eq!(hash(COARSE, u32::MAX, u32::MAX), 0xB775_2287);
    assert_eq!(load_hash_vectors(), build_hash_vectors(), "{REGENERATE}");
}

#[test]
fn lattice_values_are_symmetric_with_variance_one_sixth() {
    // Every hash's bitwise complement is its exact negation, so over all of u32 the values are
    // symmetric about zero with mean exactly 0.
    let mut rng = SplitMix64(31);
    for _ in 0..10_000 {
        let h = rng.next_u64() as u32;
        assert_eq!(lattice_value(!h), -lattice_value(h), "hash {h:#010x}");
        assert!(lattice_value(h).abs() < 1.0);
    }
    assert_eq!(lattice_value(0), -1.0 + 1.0 / 65536.0);
    assert_eq!(lattice_value(u32::MAX), 1.0 - 1.0 / 65536.0);
    // Over each component's lattice: the sample mean, variance and neighbour correlations.
    let n = 512u32;
    for component in [FINE, COARSE, CLUMP] {
        let (mut s1, mut s2, mut across, mut down) = (0.0, 0.0, 0.0, 0.0);
        for j in 0..n {
            for i in 0..n {
                let v = lattice_value(hash(component, i, j));
                s1 += v;
                s2 += v * v;
                across += v * lattice_value(hash(component, i + 1, j));
                down += v * lattice_value(hash(component, i, j + 1));
            }
        }
        let count = f64::from(n * n);
        assert!((s1 / count).abs() < 0.004, "component {component} mean");
        assert!(
            (s2 / count - 1.0 / 6.0).abs() < 0.002,
            "component {component} variance"
        );
        for correlation in [across, down] {
            assert!(
                (correlation / count * 6.0).abs() < 0.01,
                "component {component} neighbour correlation"
            );
        }
    }
}

#[test]
fn size_zero_at_full_resolution_is_one_lattice_value_a_pixel() {
    assert_eq!(cell(0.0), 1.0);
    assert_eq!(step(1.0, 1.0), FIXED_ONE);
    let field = Field::new(0.0, 0.0, Footprint::FULL);
    assert_eq!(field.fine_step, (FIXED_ONE, FIXED_ONE));
    for (x, y) in [(0u32, 0u32), (1, 0), (0, 1), (4095, 2047), (70_000, 65_536)] {
        assert_eq!(lattice(x, FIXED_ONE), (x, 0));
        let expected = 6f64.sqrt() * lattice_value(hash(FINE, x, y));
        assert!((field.unclamped(x, y) - expected).abs() <= 1e-15 * expected.abs().max(1.0));
    }
}

// ---- The field -----------------------------------------------------------------------------------

#[test]
#[slow]
fn the_field_has_zero_mean_and_unit_deviation_at_every_size_and_roughness() {
    for size in SLIDER_GRID {
        for roughness in SLIDER_GRID {
            let field = Field::new(size, roughness, Footprint::FULL);
            let (mean, sd, _, tail) = field_moments(&field, 1024);
            assert!(
                mean.abs() <= FIELD_MEAN_BOUND,
                "size {size} roughness {roughness}: mean {mean}"
            );
            assert!(
                (sd - 1.0).abs() <= FIELD_DEVIATION_BOUND,
                "size {size} roughness {roughness}: sd {sd}"
            );
            assert!(
                tail <= 1e-4,
                "size {size} roughness {roughness}: {tail} past the clamp"
            );
        }
    }
}

#[test]
fn the_field_is_bounded_and_its_clamp_holds() {
    let mut rng = SplitMix64(5);
    for size in SLIDER_GRID {
        for roughness in SLIDER_GRID {
            let field = Field::new(size, roughness, Footprint::FULL);
            // A normalised noise is below 2 in magnitude and a raw one below 1.
            let bound =
                2.0 * (field.fine_gain + field.coarse_gain) * (1.0 + field.clump_gain).sqrt();
            for _ in 0..500 {
                let (x, y) = (rng.next_u32(1 << 20), rng.next_u32(1 << 20));
                let g = field.unclamped(x, y);
                assert!(g.abs() < bound, "size {size} roughness {roughness}: {g}");
                assert!(field.at(x, y).abs() <= FIELD_CLAMP);
            }
        }
    }
    let mut rng = SplitMix64(17);
    for _ in 0..20_000 {
        let step = (1 + rng.next_u32(FIXED_ONE), 1 + rng.next_u32(FIXED_ONE));
        let (x, y) = (rng.next_u32(1 << 24), rng.next_u32(1 << 24));
        let (raw, normalised) = noise(CLUMP, x, y, step);
        assert!(raw.abs() < 1.0);
        assert!(normalised.abs() < 2.0);
    }
}

#[test]
fn the_field_depends_on_position_alone() {
    // A whole frame, and the same frame from tiles and windows of any origin and size, address
    // the same values: nothing but the position and the sliders enters the field.
    let (width, height) = (97u32, 61u32);
    for (size, roughness) in [(0.0, 0.0), (25.0, 50.0), (100.0, 100.0)] {
        let p = params(100.0, size, roughness);
        let rgb = |x: u32, y: u32| {
            let e = f64::from(x + y) / f64::from(width + height);
            [srgb::decode_encoded(e), 0.2, 0.1]
        };
        let whole: Vec<[f64; 3]> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|(x, y)| grain_pixel(x, y, rgb(x, y), &p, Footprint::FULL))
            .collect();
        for (tile_w, tile_h) in [(16u32, 16u32), (7, 13), (97, 1), (1, 61)] {
            for ty in (0..height).step_by(tile_h as usize) {
                for tx in (0..width).step_by(tile_w as usize) {
                    for y in ty..(ty + tile_h).min(height) {
                        for x in tx..(tx + tile_w).min(width) {
                            let tiled = grain_pixel(x, y, rgb(x, y), &p, Footprint::FULL);
                            assert_eq!(tiled, whole[(y * width + x) as usize]);
                        }
                    }
                }
            }
        }
        // The field takes no frame size: a 6000 x 4000 frame's field is the same field.
        let field = Field::new(size, roughness, Footprint::FULL);
        let other = Field::new(size, roughness, Footprint::of((6000, 4000), (6000, 4000)));
        assert_eq!(field, other);
        for (x, y) in [(0u32, 0u32), (96, 60), (5999, 3999)] {
            assert_eq!(field.at(x, y), other.at(x, y));
        }
    }
}

#[test]
#[slow]
fn size_sets_the_feature_size_and_roughness_the_irregularity_at_fixed_strength() {
    let mut lags = Vec::new();
    for size in SLIDER_GRID {
        lags.push(half_correlation_lag(
            &Field::new(size, 0.0, Footprint::FULL),
            256,
        ));
    }
    // Size 0 is about one pixel: the correlation falls below a half within the first lag.
    assert!(lags[0] < 1.0, "{lags:?}");
    assert!(lags.windows(2).all(|pair| pair[1] > pair[0]), "{lags:?}");
    for size in [0.0, 50.0, 100.0] {
        let mut previous_kurtosis = 0.0;
        for roughness in SLIDER_GRID {
            let field = Field::new(size, roughness, Footprint::FULL);
            let (_, sd, kurtosis, _) = field_moments(&field, 512);
            assert!((sd - 1.0).abs() <= 2.0 * FIELD_DEVIATION_BOUND);
            assert!(
                kurtosis > previous_kurtosis,
                "size {size} roughness {roughness}: kurtosis {kurtosis}"
            );
            previous_kurtosis = kurtosis;
        }
    }
}

// ---- The application -----------------------------------------------------------------------------

#[test]
fn amount_zero_is_the_exact_identity() {
    let inputs = [
        [0.0, 0.0, 0.0],
        [0.2, 0.2, 0.2],
        [0.8, 0.1, 0.05],
        [1.0, 1.0, 1.0],
        [1.5, 1.2, 1.1],
        [-0.1, 0.3, 0.6],
        [4e-7, 4e-7, 4e-7],
    ];
    for size in SLIDER_GRID {
        for roughness in SLIDER_GRID {
            let p = params(0.0, size, roughness);
            for rgb in inputs {
                for (x, y) in [(0u32, 0u32), (13, 7), (5000, 3000)] {
                    assert_eq!(grain_pixel(x, y, rgb, &p, Footprint::FULL), rgb);
                    let reduced = Footprint { x: 4.0, y: 4.0 };
                    assert_eq!(grain_pixel(x, y, rgb, &p, reduced), rgb);
                }
            }
        }
    }
    for rgb in inputs {
        for g in [-4.0, 0.0, 4.0] {
            assert_eq!(apply(rgb, g, 0.0, 1.0), rgb);
        }
    }
}

#[test]
fn black_white_and_values_above_white_are_unchanged() {
    let unchanged = [
        [0.0, 0.0, 0.0],
        [-0.02, -0.01, -0.03],
        [0.05, -0.2, 0.0],
        [1.0, 1.0, 1.0],
        [1.0, 0.4, 0.2],
        [0.3, 1.0, 0.1],
        [1.5, 1.2, 1.1],
        [3.0, 2.0, 1.0],
        [1.2, 0.4, 0.1],
        [1.586_899_311_362_360_6, 0.5, 0.25],
    ];
    for rgb in unchanged {
        for g in [-4.0, -1.0, 1.0, 4.0] {
            assert_eq!(apply(rgb, g, AMOUNT_MAX, 1.0), rgb, "{rgb:?} at {g}");
        }
    }
}

#[test]
fn a_below_white_channel_never_passes_white_and_never_crosses_black() {
    let mut rng = SplitMix64(11);
    for _ in 0..20_000 {
        let m = rng.next_range(1e-9, 1.0);
        let rgb = [
            rng.next_range(0.0, 1.0) * m,
            rng.next_range(0.0, 1.0) * m,
            m,
        ];
        let g = match rng.next_u32(3) {
            0 => -FIELD_CLAMP,
            1 => FIELD_CLAMP,
            _ => rng.next_range(-FIELD_CLAMP, FIELD_CLAMP),
        };
        let beta = rng.next_range(0.0, AMOUNT_MAX);
        let variance = rng.next_range(0.0, 1.0);
        let out = apply(rgb, g, beta, variance);
        for (channel, input) in out.iter().zip(rgb.iter()) {
            assert!(
                (0.0..1.0).contains(channel),
                "{rgb:?} g {g} beta {beta}: {out:?}"
            );
            if luminance(rgb) >= 1e-6 && *input == 0.0 {
                assert_eq!(*channel, 0.0);
            }
        }
    }
}

#[test]
fn chroma_is_preserved_by_the_luminance_ratio() {
    let mut rng = SplitMix64(23);
    for _ in 0..5000 {
        let rgb = [
            rng.next_range(0.001, 0.99),
            rng.next_range(0.001, 0.99),
            rng.next_range(0.001, 0.99),
        ];
        let g = rng.next_range(-FIELD_CLAMP, FIELD_CLAMP);
        let out = apply(rgb, g, AMOUNT_MAX, 1.0);
        let ratio = out[0] / rgb[0];
        for channel in 1..3 {
            let other = out[channel] / rgb[channel];
            assert!((other - ratio).abs() <= 1e-12 * ratio, "{rgb:?}: {out:?}");
        }
        let expected = luminance(out) / luminance(rgb);
        assert!((ratio - expected).abs() <= 1e-12 * expected);
    }
}

#[test]
fn the_near_black_rule_adds_the_luminance_change() {
    for rgb in [[5e-7, 5e-7, 5e-7], [9e-7, 2e-7, 1e-7]] {
        let l = luminance(rgb);
        assert!(l > 0.0 && l < 1e-6);
        let out = apply(rgb, 2.5, AMOUNT_MAX, 1.0);
        let shift = out[0] - rgb[0];
        assert!(shift != 0.0);
        for channel in 1..3 {
            assert!((out[channel] - rgb[channel] - shift).abs() < 1e-21);
        }
    }
}

#[test]
#[slow]
fn the_application_is_mean_preserving_at_every_tone() {
    let colours: [[f64; 3]; 3] = [[1.0, 1.0, 1.0], [1.0, 0.5, 0.2], [0.15, 0.4, 1.0]];
    let mut worst = (0.0f64, String::new());
    for (size, roughness) in [(0.0, 0.0), (25.0, 50.0), (100.0, 100.0)] {
        let field = Field::new(size, roughness, Footprint::FULL);
        let values = field_window(&field, 256);
        // The correction is exact in expectation over the field; on a finite window it is held
        // against the window's own second moment, which the normalisation test bounds.
        let variance = values.iter().map(|g| g * g).sum::<f64>() / values.len() as f64;
        assert!((variance - field.variance).abs() < 0.2);
        for step in 1..=49 {
            let e = f64::from(step) / 50.0;
            for colour in colours {
                // The colour scaled so its largest channel encodes to `e`.
                let rgb = colour.map(|c| c * srgb::decode_encoded(e));
                let shift = antithetic_shift_codes(rgb, &values, beta(100.0), variance);
                for (channel, value) in shift.iter().enumerate() {
                    if value.abs() > worst.0.abs() {
                        worst = (
                            *value,
                            format!(
                                "size {size} roughness {roughness} tone {e} {colour:?} channel \
                                 {channel}"
                            ),
                        );
                    }
                }
            }
        }
    }
    eprintln!("worst application bias {} codes at {}", worst.0, worst.1);
    assert!(
        worst.0.abs() <= MEAN_SHIFT_CODES,
        "{} codes at {}",
        worst.0,
        worst.1
    );
}

#[test]
fn finite_input_never_produces_a_non_finite_output() {
    let mut rng = SplitMix64(41);
    for _ in 0..2000 {
        let p = params(
            rng.next_range(0.0, 100.0),
            rng.next_range(0.0, 100.0),
            rng.next_range(0.0, 100.0),
        );
        let footprint = Footprint {
            x: rng.next_range(1.0, 40.0),
            y: rng.next_range(1.0, 40.0),
        };
        let rgb = [
            rng.next_range(-1.0, 3.0),
            rng.next_range(-1.0, 3.0),
            rng.next_range(-1.0, 3.0),
        ];
        let (x, y) = (rng.next_u64() as u32, rng.next_u64() as u32);
        let out = grain_pixel(x, y, rgb, &p, footprint);
        assert!(out.iter().all(|c| c.is_finite()), "{p:?} {rgb:?}: {out:?}");
    }
}

// ---- Reduced stages ------------------------------------------------------------------------------

#[test]
fn a_full_resolution_footprint_compiles_the_full_resolution_field() {
    for size in SLIDER_GRID {
        for roughness in SLIDER_GRID {
            let full = Field::new(size, roughness, Footprint::FULL);
            let expected = (65536.0 / cell(size) + 0.5).floor() as u32;
            assert_eq!(full.fine_step, (expected, expected));
            assert!((full.variance - 1.0).abs() < 1e-15);
            // A stage larger than full resolution is clamped to the full field.
            let finer = Field::new(size, roughness, Footprint::of((800, 600), (400, 300)));
            assert_eq!(finer, full);
        }
    }
    // A reduced component's lattice is never finer than one cell a stage pixel, and the reduced
    // field's expected variance is below one.
    for s in [2.0, 4.0, 8.0, 5.997] {
        for size in SLIDER_GRID {
            let field = Field::new(size, 100.0, Footprint { x: s, y: s });
            for step in [field.fine_step, field.coarse_step, field.clump_step] {
                assert!(step.0 <= FIXED_ONE && step.1 <= FIXED_ONE);
            }
            assert!(field.variance < 1.0);
        }
    }
}

fn check_reduced_stage(s: u32) {
    let n = 256u32;
    for size in SLIDER_GRID {
        for roughness in [0.0, 50.0, 100.0] {
            let p = params(100.0, size, roughness);
            let pairs = reduced_pairs(&p, &REDUCED_TONES, s, n);
            for (pair, tone) in pairs.iter().zip(REDUCED_TONES) {
                let label = format!("s {s} size {size} roughness {roughness} tone {tone}");
                let ratio = pair.deviation_ratio();
                assert!(
                    (ratio - 1.0).abs() <= REDUCED_DEVIATION,
                    "{label}: deviation ratio {ratio}"
                );
                let band = pair.worst_band_difference(n as usize);
                assert!(band <= REDUCED_BAND_SHARE, "{label}: band share {band}");
                let mean = pair.mean_difference_codes();
                assert!(
                    mean.abs() <= REDUCED_MEAN_CODES,
                    "{label}: mean difference {mean} codes"
                );
            }
        }
    }
}

#[test]
#[slow]
fn the_reduced_stage_matches_the_reduced_full_resolution_grain_at_two() {
    check_reduced_stage(2);
}

#[test]
#[slow]
fn the_reduced_stage_matches_the_reduced_full_resolution_grain_at_four() {
    check_reduced_stage(4);
}

#[test]
#[slow]
fn the_reduced_stage_matches_the_reduced_full_resolution_grain_at_eight() {
    check_reduced_stage(8);
}

// ---- Worked examples -----------------------------------------------------------------------------

#[test]
fn worked_example_one_the_hash_and_a_lattice_value() {
    // key = mix(SEED + 0); hash(0, 3, 5) = mix(3 + mix(5 + key)); the value sums its halves.
    let key = component_key(FINE);
    let h = mix(3u32.wrapping_add(mix(5u32.wrapping_add(key))));
    assert_eq!(hash(FINE, 3, 5), h);
    let expected = f64::from((h & 0xFFFF) + (h >> 16) + 1) / 65536.0 - 1.0;
    assert_eq!(lattice_value(h), expected);
}

#[test]
fn worked_example_two_the_default_compile_and_a_lattice_coordinate() {
    // Size 25, Roughness 50: cell 6^0.25 = 1.5650846, steps round(65536 / (m * cell)).
    let field = Field::new(25.0, 50.0, Footprint::FULL);
    assert_eq!(field.fine_step, (41874, 41874));
    assert_eq!(field.coarse_step, (20937, 20937));
    assert_eq!(field.clump_step, (10468, 10468));
    // Pixel 10 at the fine step: 10 * 41874 = 418740 = 6 * 65536 + 25524.
    assert_eq!(lattice(10, 41874), (6, 25524));
    // r = 0.375: gains sqrt(6) / sqrt(1 + r^2) and r times it; the clump gain 0.9 * 0.5.
    let norm = (1.0f64 + 0.375 * 0.375).sqrt();
    assert!((field.fine_gain - 6f64.sqrt() / norm).abs() < 1e-15);
    assert!((field.coarse_gain - 0.375 * 6f64.sqrt() / norm).abs() < 1e-15);
    assert_eq!(field.clump_gain, 0.45);
}

#[test]
fn worked_example_three_a_mid_grey_pixel() {
    // Encoded 0.5 grey, g = 1, Amount 100: t = 0.5, s = 4 * 0.05 * 0.25 = 0.05,
    // E' = 0.5 + 0.05 - 0.7 * 0.0025 / 0.555.
    let out = apply(grey_at(0.5), 1.0, beta(100.0), 1.0);
    let expected = srgb::decode_encoded(0.5 + 0.05 - 0.7 * 0.0025 / 0.555);
    for channel in out {
        assert!((channel - expected).abs() < 1e-15, "{channel} {expected}");
    }
}

// ---- Oracle fixtures -----------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct MixVector {
    input: u32,
    output: u32,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct HashVector {
    component: u32,
    i: u32,
    j: u32,
    hash: u32,
    value: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct HashVectors {
    seed: u32,
    component_keys: [u32; 3],
    mix: Vec<MixVector>,
    hash: Vec<HashVector>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct Compiled {
    fine_step: [u32; 2],
    coarse_step: [u32; 2],
    clump_step: [u32; 2],
    fine_gain: f64,
    coarse_gain: f64,
    clump_gain: f64,
    variance: f64,
}

impl From<Field> for Compiled {
    fn from(f: Field) -> Self {
        Self {
            fine_step: [f.fine_step.0, f.fine_step.1],
            coarse_step: [f.coarse_step.0, f.coarse_step.1],
            clump_step: [f.clump_step.0, f.clump_step.1],
            fine_gain: f.fine_gain,
            coarse_gain: f.coarse_gain,
            clump_gain: f.clump_gain,
            variance: f.variance,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct FieldSample {
    x: u32,
    y: u32,
    field: f64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct FieldCase {
    label: String,
    size: f64,
    roughness: f64,
    footprint: [f64; 2],
    compiled: Compiled,
    samples: Vec<FieldSample>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct FieldCases {
    /// The full-resolution steps `[fine, coarse, clump]` at every integer Size `0..=100`.
    full_resolution_steps: Vec<[u32; 3]>,
    cases: Vec<FieldCase>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
struct PixelCase {
    label: String,
    amount: f64,
    size: f64,
    roughness: f64,
    footprint: [f64; 2],
    x: u32,
    y: u32,
    input_linear_rgb: [f64; 3],
    expected_field: f64,
    expected_linear_rgb: [f64; 3],
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/grain")
        .join(name)
}

fn build_hash_vectors() -> HashVectors {
    let inputs = [
        0u32,
        1,
        2,
        0xFF,
        0x1_0000,
        0x7FFF_FFFF,
        0x8000_0000,
        0xDEAD_BEEF,
        0xFFFF_FFFF,
    ];
    let mut vectors = Vec::new();
    for component in [FINE, COARSE, CLUMP] {
        for (i, j) in [
            (0u32, 0u32),
            (1, 0),
            (0, 1),
            (3, 5),
            (65535, 65536),
            (123_456, 654_321),
            (u32::MAX, u32::MAX),
        ] {
            let h = hash(component, i, j);
            vectors.push(HashVector {
                component,
                i,
                j,
                hash: h,
                value: lattice_value(h),
            });
        }
    }
    HashVectors {
        seed: SEED,
        component_keys: [FINE, COARSE, CLUMP].map(component_key),
        mix: inputs
            .iter()
            .map(|&input| MixVector {
                input,
                output: mix(input),
            })
            .collect(),
        hash: vectors,
    }
}

const SAMPLE_POSITIONS: [(u32, u32); 6] = [
    (0, 0),
    (1, 0),
    (0, 1),
    (123, 45),
    (5999, 3999),
    (65_543, 70_000),
];

fn build_field_cases() -> FieldCases {
    let full_resolution_steps = (0..=100)
        .map(|size| {
            let field = Field::new(f64::from(size), 100.0, Footprint::FULL);
            [field.fine_step.0, field.coarse_step.0, field.clump_step.0]
        })
        .collect();
    let mut cases = Vec::new();
    let mut push = |label: String, size: f64, roughness: f64, footprint: Footprint| {
        let field = Field::new(size, roughness, footprint);
        cases.push(FieldCase {
            label,
            size,
            roughness,
            footprint: [footprint.x, footprint.y],
            compiled: field.into(),
            samples: SAMPLE_POSITIONS
                .iter()
                .map(|&(x, y)| FieldSample {
                    x,
                    y,
                    field: field.at(x, y),
                })
                .collect(),
        });
    };
    for size in SLIDER_GRID {
        for roughness in SLIDER_GRID {
            push(
                format!("full/size{size}/roughness{roughness}"),
                size,
                roughness,
                Footprint::FULL,
            );
        }
    }
    for s in [2.0, 4.0, 8.0] {
        for size in [0.0, 25.0, 100.0] {
            for roughness in [0.0, 100.0] {
                push(
                    format!("reduced/s{s}/size{size}/roughness{roughness}"),
                    size,
                    roughness,
                    Footprint { x: s, y: s },
                );
            }
        }
    }
    // A Fit-like stage: a 6000 x 4000 output drawn at 1000 x 667.
    for size in [25.0, 100.0] {
        push(
            format!("fit/6000x4000-at-1000x667/size{size}"),
            size,
            50.0,
            Footprint::of((1000, 667), (6000, 4000)),
        );
    }
    FieldCases {
        full_resolution_steps,
        cases,
    }
}

fn build_pixel_cases() -> Vec<PixelCase> {
    let mut cases = Vec::new();
    let mut push =
        |label: String, p: GrainParams, footprint: Footprint, (x, y): (u32, u32), rgb: [f64; 3]| {
            let field = Field::new(p.size, p.roughness, footprint);
            cases.push(PixelCase {
                label,
                amount: p.amount,
                size: p.size,
                roughness: p.roughness,
                footprint: [footprint.x, footprint.y],
                x,
                y,
                input_linear_rgb: rgb,
                expected_field: field.at(x, y),
                expected_linear_rgb: grain_pixel(x, y, rgb, &p, footprint),
            });
        };
    let default = params(100.0, 25.0, 50.0);
    let positions = [(0u32, 0u32), (123, 45), (5999, 3999)];
    for e in [0.1, 0.3, 0.5, 0.7, 0.9] {
        for &(x, y) in &positions {
            push(
                format!("flat/{e}/{x}x{y}"),
                default,
                Footprint::FULL,
                (x, y),
                grey_at(e),
            );
        }
    }
    for step in 0..=10 {
        let e = f64::from(step) / 10.0;
        push(
            format!("wedge/{e}"),
            params(50.0, 25.0, 50.0),
            Footprint::FULL,
            (step, 17),
            grey_at(e),
        );
    }
    for (name, rgb) in [
        ("red", [0.8, 0.1, 0.05]),
        ("green", [0.05, 0.6, 0.1]),
        ("blue", [0.1, 0.1, 0.7]),
        ("yellow", [0.9, 0.9, 0.05]),
        ("skin", [0.45, 0.25, 0.18]),
    ] {
        for &(x, y) in &positions {
            push(
                format!("colour/{name}/{x}x{y}"),
                default,
                Footprint::FULL,
                (x, y),
                rgb,
            );
        }
    }
    for (name, rgb) in [
        ("black", [0.0, 0.0, 0.0]),
        ("below-black", [-0.02, -0.01, -0.03]),
        ("near-black", [5e-7, 5e-7, 5e-7]),
        ("white", [1.0, 1.0, 1.0]),
        ("channel-at-white", [1.0, 0.4, 0.2]),
        ("headroom-grey", [1.5, 1.2, 1.1]),
        ("headroom-colour", [1.2, 0.4, 0.1]),
        ("headroom-bright", [3.0, 2.0, 1.0]),
    ] {
        push(
            format!("ends/{name}"),
            default,
            Footprint::FULL,
            (123, 45),
            rgb,
        );
    }
    for amount in [0.0, 100.0] {
        for size in [0.0, 100.0] {
            for roughness in [0.0, 100.0] {
                for &(x, y) in &positions {
                    push(
                        format!("sliders/amount{amount}/size{size}/roughness{roughness}/{x}x{y}"),
                        params(amount, size, roughness),
                        Footprint::FULL,
                        (x, y),
                        grey_at(0.5),
                    );
                }
            }
        }
    }
    for s in [2.0, 4.0, 8.0] {
        for (size, roughness) in [(25.0, 50.0), (0.0, 0.0), (100.0, 100.0)] {
            for &(x, y) in &positions[..2] {
                push(
                    format!("reduced/s{s}/size{size}/roughness{roughness}/{x}x{y}"),
                    params(100.0, size, roughness),
                    Footprint { x: s, y: s },
                    (x, y),
                    grey_at(0.5),
                );
            }
        }
    }
    cases
}

fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = fixture_path(name);
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&contents).unwrap_or_else(|error| panic!("{name}: {error}"))
}

fn load_hash_vectors() -> HashVectors {
    load("hash-vectors.json")
}

const REGENERATE: &str = "regenerate with `cargo test -p luxforge-reference --test studies -- \
                          --ignored regenerate_committed_grain_fixtures`";

#[test]
fn committed_field_cases_match_the_reference() {
    let committed: FieldCases = load("field-cases.json");
    let fresh = build_field_cases();
    assert_eq!(
        committed.full_resolution_steps, fresh.full_resolution_steps,
        "{REGENERATE}"
    );
    assert_eq!(committed.cases.len(), fresh.cases.len(), "{REGENERATE}");
    for (c, f) in committed.cases.iter().zip(fresh.cases.iter()) {
        assert_eq!(c.label, f.label, "{REGENERATE}");
        let steps = |compiled: &Compiled| {
            (
                compiled.fine_step,
                compiled.coarse_step,
                compiled.clump_step,
            )
        };
        assert_eq!(steps(&c.compiled), steps(&f.compiled), "{}", c.label);
        let gains = [
            (c.compiled.fine_gain, f.compiled.fine_gain),
            (c.compiled.coarse_gain, f.compiled.coarse_gain),
            (c.compiled.clump_gain, f.compiled.clump_gain),
            (c.compiled.variance, f.compiled.variance),
        ];
        let samples = c
            .samples
            .iter()
            .zip(f.samples.iter())
            .map(|(a, b)| (a.field, b.field));
        for (a, b) in gains.into_iter().chain(samples) {
            assert!(approximately_equal(a, b), "{} stale: {a} vs {b}", c.label);
        }
    }
}

#[test]
fn committed_pixel_cases_match_the_reference() {
    let committed: Vec<PixelCase> = load("pixel-cases.json");
    let fresh = build_pixel_cases();
    assert_eq!(committed.len(), fresh.len(), "{REGENERATE}");
    for (c, f) in committed.iter().zip(fresh.iter()) {
        assert_eq!(c.label, f.label, "{REGENERATE}");
        assert!(approximately_equal(c.expected_field, f.expected_field));
        let committed_values = c
            .input_linear_rgb
            .iter()
            .chain(c.expected_linear_rgb.iter());
        let fresh_values = f
            .input_linear_rgb
            .iter()
            .chain(f.expected_linear_rgb.iter());
        for (a, b) in committed_values.zip(fresh_values) {
            assert!(approximately_equal(*a, *b), "{} stale: {a} vs {b}", c.label);
        }
    }
}

#[test]
#[ignore = "regenerates the committed oracle fixtures; run after changing the frozen equations in \
            crates/luxforge-reference/src/grain.rs, and re-freeze docs/design/grain-study.md"]
fn regenerate_committed_grain_fixtures() {
    let write = |name: &str, json: String| {
        std::fs::write(fixture_path(name), json + "\n").expect("write a grain fixture");
    };
    write(
        "hash-vectors.json",
        serde_json::to_string_pretty(&build_hash_vectors()).unwrap(),
    );
    write(
        "field-cases.json",
        serde_json::to_string_pretty(&build_field_cases()).unwrap(),
    );
    write(
        "pixel-cases.json",
        serde_json::to_string_pretty(&build_pixel_cases()).unwrap(),
    );
}

// ---- The study's measurements --------------------------------------------------------------------

/// Prints every table `docs/design/grain-study.md` records, so each can be reproduced.
#[test]
#[ignore = "prints the study's measurements; run with --nocapture"]
fn print_the_study_measurements() {
    println!("| Size | cell | fine step | half-correlation lag | band shares |");
    for size in SLIDER_GRID {
        let field = Field::new(size, 0.0, Footprint::FULL);
        let lag = half_correlation_lag(&field, 256);
        let shares = band_shares(&field_window(&field, 256), 256);
        let shares: Vec<String> = shares.iter().map(|s| format!("{s:.3}")).collect();
        println!(
            "| {size} | {:.3} | {} | {lag:.2} | {} |",
            cell(size),
            field.fine_step.0,
            shares.join(" ")
        );
    }
    println!();
    println!("| Size | Roughness | mean | sd | kurtosis | past the clamp |");
    for size in SLIDER_GRID {
        for roughness in SLIDER_GRID {
            let field = Field::new(size, roughness, Footprint::FULL);
            let (mean, sd, kurtosis, tail) = field_moments(&field, 1024);
            println!(
                "| {size} | {roughness} | {mean:+.4} | {sd:.4} | {kurtosis:.2} | {tail:.1e} |"
            );
        }
    }
    println!();
    println!("| tone | grain sd (codes) | bias uncorrected (codes) | bias corrected (codes) |");
    let field = Field::new(25.0, 50.0, Footprint::FULL);
    let values = field_window(&field, 256);
    for e in [0.02, 0.05, 0.1, 0.2, 0.3, 0.5, 0.7, 0.8, 0.9, 0.95] {
        let rgb = grey_at(e);
        let encoded: Vec<f64> = values
            .iter()
            .map(|&g| srgb::encode_extended(apply(rgb, g, beta(100.0), 1.0)[0]))
            .collect();
        let uncorrected = antithetic_shift_codes(rgb, &values, beta(100.0), 0.0)[0];
        let corrected = antithetic_shift_codes(rgb, &values, beta(100.0), 1.0)[0];
        println!(
            "| {e} | {:.2} | {uncorrected:+.3} | {corrected:+.4} |",
            deviation(&encoded) * 255.0
        );
    }
    println!();
    println!(
        "| s | Size | Roughness | worst sd ratio | worst band share difference | worst mean |"
    );
    for s in [2u32, 4, 8] {
        for size in SLIDER_GRID {
            for roughness in [0.0, 50.0, 100.0] {
                let p = params(100.0, size, roughness);
                let pairs = reduced_pairs(&p, &REDUCED_TONES, s, 256);
                let ratio = pairs
                    .iter()
                    .map(Reduced::deviation_ratio)
                    .max_by(|a, b| (a - 1.0).abs().total_cmp(&(b - 1.0).abs()))
                    .unwrap();
                let band = pairs
                    .iter()
                    .map(|p| p.worst_band_difference(256))
                    .fold(0.0, f64::max);
                let mean = pairs
                    .iter()
                    .map(Reduced::mean_difference_codes)
                    .max_by(|a, b| a.abs().total_cmp(&b.abs()))
                    .unwrap();
                println!("| {s} | {size} | {roughness} | {ratio:.3} | {band:.3} | {mean:+.3} |");
            }
        }
    }
}
