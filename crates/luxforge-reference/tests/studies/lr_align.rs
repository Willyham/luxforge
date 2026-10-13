//! The Lightroom alignment rounds' measures and fits (`crates/luxforge-reference/src/lr_align.rs`)
//! on synthetic cases with known answers, before they are trusted on Lightroom's exports: a known
//! scale, a reversed control and a range shortfall recovered by the fit; a known gain, chroma
//! scale, hue rotation and locality by the pointwise measures; a known blur's radius, a known
//! sharpening's overshoot and halo, a known band gain and noise deviation by the spatial measures;
//! known crossings by the profiles; and known positions and shifts by the dot measures.

use luxforge_reference::colour::{self, Oklab};
use luxforge_reference::lr_align::{
    self, Band, Direction, Lab, Target, axis_profile, band_energy, band_gains, blur_from_gains,
    box_blur, carried, crossing, deviation, dots, edge, grid_angle, keystone, linear_gain,
    locality, match_points, orientation, radial_profile, sectors, tone_locality,
};
use luxforge_reference::{SplitMix64, srgb};

/// A grey wedge and three rings of hues: the patches a synthetic round measures.
fn patches() -> Vec<[f64; 3]> {
    let mut patches: Vec<[f64; 3]> = (1..12)
        .map(|step| [srgb::decode((step * 21) as u8); 3])
        .collect();
    for l in [0.45, 0.6, 0.75] {
        for step in 0..12 {
            let angle = f64::from(step) * 30f64.to_radians();
            patches.push(colour::from_oklab(Oklab {
                l,
                a: 0.08 * angle.cos(),
                b: 0.08 * angle.sin(),
            }));
        }
    }
    patches
}

/// Saturation by `s` in Oklab, the synthetic control both "editors" share.
fn saturate(rgb: [f64; 3], s: f64) -> [f64; 3] {
    let lab = colour::to_oklab(rgb);
    let k = (1.0 + s / 100.0).max(0.0);
    colour::from_oklab(Oklab {
        l: lab.l,
        a: lab.a * k,
        b: lab.b * k,
    })
}

/// A different base rendering, so carrying a response across has a base to cancel.
fn other_base(rgb: [f64; 3]) -> [f64; 3] {
    rgb.map(|v| (v * 1.08).powf(0.95))
}

const VALUES: [f64; 8] = [-100.0, -75.0, -50.0, -25.0, 25.0, 50.0, 75.0, 100.0];

/// Luxforge's sampled responses: saturation on a dense grid.
fn luxforge() -> (Vec<f64>, Vec<Vec<Lab>>, Vec<Lab>) {
    let values: Vec<f64> = (0..=40).map(|i| -100.0 + 5.0 * f64::from(i)).collect();
    let labs = values
        .iter()
        .map(|v| {
            patches()
                .iter()
                .map(|p| lr_align::lab(saturate(*p, *v)))
                .collect()
        })
        .collect();
    let neutral = patches().iter().map(|p| lr_align::lab(*p)).collect();
    (values, labs, neutral)
}

/// "Lightroom": the same control at `map(v)`, over another base rendering, carried onto Luxforge's
/// neutral.
fn lightroom(map: impl Fn(f64) -> f64, neutral: &[Lab]) -> Vec<(f64, Target)> {
    VALUES
        .iter()
        .map(|v| {
            let labs = patches()
                .iter()
                .map(|p| {
                    let base = other_base(*p);
                    lr_align::lab(carried(*p, base, saturate(base, map(*v))))
                })
                .collect();
            (*v, Target::new(neutral, labs))
        })
        .collect()
}

/// A Lightroom control half as strong is fitted as `v / 2`, monotone, with small residuals and no
/// shortfall; its 21 knots run across −100..100 through the neutral anchor.
#[test]
fn a_known_scale_is_recovered() {
    let (values, labs, neutral) = luxforge();
    let fit = lr_align::fit(
        &values,
        &labs,
        &lightroom(|v| v / 2.0, &neutral),
        Some((0.0, 0.0)),
        (-100.0, 100.0),
    );
    assert_eq!(fit.direction, Direction::Increasing);
    for point in &fit.points {
        assert!(
            (point.luxforge - point.lightroom / 2.0).abs() <= 1.5,
            "{point:?}"
        );
        assert!(point.residual < 0.3, "{point:?}");
        assert!(!point.shortfall);
        assert!(point.identity.unwrap() > point.residual);
    }
    assert_eq!(fit.knots.len(), lr_align::KNOTS);
    assert_eq!(fit.knots[10].lightroom, 0.0);
    assert_eq!(fit.knots[10].luxforge, Some(0.0));
    assert!(fit.knots[10].measured && fit.knots[15].measured && !fit.knots[11].measured);
    assert!((fit.knots[20].luxforge.unwrap() - 50.0).abs() <= 1.5);
}

/// A control that runs the other way is fitted decreasing.
#[test]
fn a_reversed_control_is_fitted_decreasing() {
    let (values, labs, neutral) = luxforge();
    let fit = lr_align::fit(
        &values,
        &labs,
        &lightroom(|v| -v * 0.8, &neutral),
        Some((0.0, 0.0)),
        (-100.0, 100.0),
    );
    assert_eq!(fit.direction, Direction::Decreasing);
    for point in &fit.points {
        assert!(
            (point.luxforge + 0.8 * point.lightroom).abs() <= 1.5,
            "{point:?}"
        );
    }
}

/// A Lightroom control twice as strong needs Luxforge values beyond its range: those points are
/// reported as shortfalls at the range's end, not clamped silently, and the others still fit.
#[test]
fn a_range_shortfall_is_reported() {
    let (values, labs, neutral) = luxforge();
    let fit = lr_align::fit(
        &values,
        &labs,
        &lightroom(|v| 2.0 * v, &neutral),
        Some((0.0, 0.0)),
        (-100.0, 100.0),
    );
    for point in &fit.points {
        if point.lightroom >= 75.0 {
            assert!(point.shortfall, "{point:?}");
            assert_eq!(point.luxforge, 100.0);
        } else if point.lightroom.abs() <= 25.0 {
            assert!(!point.shortfall, "{point:?}");
            assert!(
                (point.luxforge - 2.0 * point.lightroom).abs() <= 2.0,
                "{point:?}"
            );
        }
    }
}

/// Through a noisy sample the pooled map stays monotone and passes through the anchor.
#[test]
fn a_noisy_fit_stays_monotone() {
    let (values, labs, neutral) = luxforge();
    let wobble = |v: f64| {
        if v == 50.0 {
            15.0
        } else if v == 75.0 {
            30.0
        } else {
            v * 0.6
        }
    };
    let fit = lr_align::fit(
        &values,
        &labs,
        &lightroom(wobble, &neutral),
        Some((0.0, 0.0)),
        (-100.0, 100.0),
    );
    assert!(
        fit.points
            .windows(2)
            .all(|p| p[0].luxforge <= p[1].luxforge)
    );
}

/// A known exposure gain is recovered from linear luminance, clipped samples left out.
#[test]
fn a_known_gain_is_recovered() {
    let neutral: Vec<[f64; 3]> = (0..=255).map(|c| [srgb::decode(c); 3]).collect();
    let gain = 2f64.powf(0.7);
    let adjusted: Vec<[f64; 3]> = neutral
        .iter()
        .map(|p| p.map(|v| (v * gain).min(1.0)))
        .collect();
    let measured = linear_gain(&neutral, &adjusted, 0.002, 0.98).unwrap();
    assert!((measured - gain).abs() < 1e-9, "{measured}");
}

/// A known chroma scale and hue rotation are recovered in every sector; greys are not read.
#[test]
fn a_known_chroma_scale_and_hue_rotation_are_recovered() {
    let neutral = patches();
    let adjusted: Vec<[f64; 3]> = neutral
        .iter()
        .map(|p| {
            let lab = colour::to_oklab(*p);
            let (c, h) = (
                colour::chroma(lab) * 1.3,
                lab.b.atan2(lab.a) + 10f64.to_radians(),
            );
            colour::from_oklab(Oklab {
                l: lab.l,
                a: c * h.cos(),
                b: c * h.sin(),
            })
        })
        .collect();
    let found = sectors(&neutral, &adjusted, 12, 0.02);
    assert_eq!(found.len(), 12);
    for sector in found {
        assert_eq!(sector.samples, 3);
        assert!((sector.chroma_ratio - 1.3).abs() < 1e-4, "{sector:?}");
        assert!((sector.hue_shift - 10.0).abs() < 1e-3, "{sector:?}");
        assert!(sector.lightness_shift.abs() < 1e-6, "{sector:?}");
    }
}

/// A pointwise change is not local; one that pulls each patch towards its surround is.
#[test]
fn locality_tells_a_local_change_from_a_pointwise_one() {
    let surrounds = [0.02, 0.2, 0.8];
    let greys = [0.05, 0.2, 0.5];
    let group = |change: &dyn Fn(f64, f64) -> f64| -> Vec<Vec<([f64; 3], [f64; 3])>> {
        greys
            .iter()
            .map(|g| {
                surrounds
                    .iter()
                    .map(|s| ([*g; 3], [change(*g, *s); 3]))
                    .collect()
            })
            .collect()
    };
    let pointwise = locality(&group(&|g, _| g * 1.5));
    let local = locality(&group(&|g, s| g + 0.3 * (g - s)));
    assert!(pointwise < 1e-9, "{pointwise}");
    assert!(local > 2.0, "{local}");
}

/// A step blurred by a Gaussian of `sigma`, sampled at pixel centres, the edge at `at`.
fn blurred_step(n: usize, at: f64, sigma: f64) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let x = (i as f64 + 0.5 - at) / sigma;
            0.2 + 0.6 * normal_cdf(x)
        })
        .collect()
}

fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// The error function by its Taylor series, to full double precision for |x| ≤ 6.
fn erf(x: f64) -> f64 {
    if x.abs() > 6.0 {
        return x.signum();
    }
    let (mut term, mut sum) = (x, x);
    for n in 1..200 {
        term *= -x * x / n as f64;
        let next = term / (2 * n + 1) as f64;
        sum += next;
        if next.abs() < 1e-17 {
            break;
        }
    }
    sum * 2.0 / std::f64::consts::PI.sqrt()
}

/// A known blur's radius is recovered from its line spread, its 10–90% rise is `2.563 σ` once
/// the blur spans several pixels, and a blurred step has no halo; a falling edge reads the same.
#[test]
fn a_known_blur_recovers_its_radius() {
    for sigma in [0.8, 1.5, 3.0] {
        let measured = edge(&blurred_step(200, 100.0, sigma), 100.0);
        assert!(
            (measured.spread - sigma).abs() < 0.015 * sigma,
            "{sigma}: {measured:?}"
        );
        assert!(
            measured.overshoot < 1e-6 && measured.halo == 0.0,
            "{measured:?}"
        );
        if sigma >= 3.0 {
            assert!(
                (measured.rise / (2.0 * 1.2816) - sigma).abs() < 0.05 * sigma,
                "{measured:?}"
            );
        }
    }
    let mut falling = blurred_step(200, 100.0, 1.5);
    falling.reverse();
    assert!((edge(&falling, 100.0).spread - 1.5).abs() < 0.015);
}

/// Unsharp masking of a hard step by `amount` with a Gaussian of `sigma`: overshoot of `amount ·
/// (1 − Φ(0.5 / σ))` beside the edge on each side, and a halo reaching `1.645 σ`, where the excess
/// falls to 10% of the peak.
#[test]
fn a_known_sharpening_recovers_its_overshoot_and_halo() {
    for (amount, sigma) in [(0.5, 2.0), (1.0, 4.0)] {
        let n = 200;
        let at = 100.0;
        let step: Vec<f64> = (0..n)
            .map(|i| if (i as f64) + 0.5 > at { 0.8 } else { 0.2 })
            .collect();
        let blurred = blurred_step(n, at, sigma);
        let sharpened: Vec<f64> = step
            .iter()
            .zip(&blurred)
            .map(|(s, b)| s + amount * (s - b))
            .collect();
        let measured = edge(&sharpened, at);
        let expected = amount * (1.0 - normal_cdf(0.5 / sigma));
        assert!((measured.overshoot - expected).abs() < 1e-6, "{measured:?}");
        assert!(
            (measured.undershoot - expected).abs() < 1e-6,
            "{measured:?}"
        );
        assert!((measured.halo - 1.645 * sigma).abs() <= 1.0, "{measured:?}");
    }
}

/// The zone plate the spatial target draws, `side` pixels square, Nyquist at `radius`, through
/// `filter` applied to its amplitude at each local frequency.
fn plate(side: usize, radius: f64, filter: impl Fn(f64) -> f64) -> (Vec<f64>, (f64, f64)) {
    let centre = (side as f64 / 2.0, side as f64 / 2.0);
    let plane = (0..side * side)
        .map(|i| {
            let (x, y) = (
                (i % side) as f64 + 0.5 - centre.0,
                (i / side) as f64 + 0.5 - centre.1,
            );
            let r2 = x * x + y * y;
            let f = r2.sqrt() / (2.0 * radius);
            0.5 + 0.3 * filter(f) * (std::f64::consts::PI * r2 / (2.0 * radius)).cos()
        })
        .collect();
    (plane, centre)
}

/// A known gain in every band is recovered, and a known Gaussian's `σ` from its band gains.
#[test]
fn a_known_band_gain_and_blur_are_recovered() {
    let (side, radius) = (600, 280.0);
    let (neutral, centre) = plate(side, radius, |_| 1.0);
    let (doubled, _) = plate(side, radius, |_| 1.4);
    for band in band_gains(&neutral, &doubled, side, centre, radius, 5) {
        assert!((band.gain - 1.4).abs() < 0.02, "{band:?}");
    }
    let sigma: f64 = 1.2;
    let attenuation = |f: f64| (-2.0 * std::f64::consts::PI.powi(2) * sigma * sigma * f * f).exp();
    let (blurred, _) = plate(side, radius, attenuation);
    let bands = band_gains(&neutral, &blurred, side, centre, radius, 10);
    let measured = blur_from_gains(&bands).unwrap();
    assert!((measured - sigma).abs() < 0.05, "{measured} from {bands:?}");
    assert!(
        blur_from_gains(&[Band {
            low: 0.0,
            high: 0.05,
            gain: 1.0
        }])
        .is_none()
    );
}

/// A noise field scaled by `k` measures `k` times the deviation.
#[test]
fn a_known_noise_scale_is_recovered() {
    let mut rng = SplitMix64(7);
    let noise: Vec<f64> = (0..40_000).map(|_| rng.next_range(-0.5, 0.5)).collect();
    let scaled: Vec<f64> = noise.iter().map(|v| 0.35 * v).collect();
    assert!((deviation(&scaled) / deviation(&noise) - 0.35).abs() < 1e-12);
}

/// A radial falloff with its half point at `ρ = 0.6` and an axial ramp with its half point at
/// `t = 0.5` are found at those places, and the axial ramp's direction is read from its ends.
#[test]
fn profiles_find_known_crossings() {
    let (width, height) = (300usize, 200usize);
    let centre = (150.0, 100.0);
    let axes = (150.0, 100.0);
    let radial: Vec<f64> = (0..width * height)
        .map(|i| {
            let (x, y) = ((i % width) as f64 + 0.5, (i / width) as f64 + 0.5);
            let rho = ((x - centre.0) / axes.0).hypot((y - centre.1) / axes.1);
            1.0 / (1.0 + ((rho - 0.6) / 0.08).exp())
        })
        .collect();
    let profile = radial_profile(&radial, width, centre, axes, 0.0, 60, 1.2);
    let half = crossing(&profile, 0.5).unwrap();
    assert!((half - 0.6).abs() < 0.01, "{half}");
    let from = (60.0, 40.0);
    let to = (240.0, 160.0);
    let ramp: Vec<f64> = (0..width * height)
        .map(|i| {
            let (x, y) = ((i % width) as f64 + 0.5, (i / width) as f64 + 0.5);
            let t = ((x - from.0) * (to.0 - from.0) + (y - from.1) * (to.1 - from.1))
                / ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2));
            t.clamp(0.0, 1.0)
        })
        .collect();
    let axial = axis_profile(&ramp, width, from, to, 40, (-0.5, 1.5));
    let half = crossing(&axial, 0.5).unwrap();
    assert!((half - 0.5).abs() < 0.03, "{half}");
    assert!(axial.first().unwrap()[1] < axial.last().unwrap()[1]);
}

/// Antialiased dots at known centres are found within a twentieth of a pixel, and a known shift is
/// measured by matching.
#[test]
fn dots_are_found_and_a_known_shift_is_measured() {
    let (width, height) = (400usize, 300usize);
    let draw = |shift: (f64, f64)| -> (Vec<f64>, Vec<[f64; 2]>) {
        let mut plane = vec![0.8; width * height];
        let mut centres = Vec::new();
        for row in 0..5 {
            for column in 0..7 {
                let (cx, cy) = (
                    30.0 + 50.0 * f64::from(column) + shift.0,
                    40.0 + 50.0 * f64::from(row) + shift.1,
                );
                centres.push([cx, cy]);
                for y in (cy as usize - 8)..(cy as usize + 9) {
                    for x in (cx as usize - 8)..(cx as usize + 9) {
                        let mut inside = 0;
                        for sy in 0..8 {
                            for sx in 0..8 {
                                let px = x as f64 + (f64::from(sx) + 0.5) / 8.0 - cx;
                                let py = y as f64 + (f64::from(sy) + 0.5) / 8.0 - cy;
                                inside += i32::from(px * px + py * py <= 25.0);
                            }
                        }
                        plane[y * width + x] = 0.8 - 0.7 * f64::from(inside) / 64.0;
                    }
                }
            }
        }
        (plane, centres)
    };
    let (plane, centres) = draw((0.0, 0.0));
    let found = dots(&plane, width, 0.45, 4);
    assert_eq!(found.len(), centres.len());
    let (error, unmatched) = match_points(&found, &centres, 5.0);
    assert!(error < 0.05 && unmatched == 0, "{error}");
    let (shifted, _) = draw((0.3, -0.4));
    let (shift, unmatched) = match_points(&dots(&shifted, width, 0.45, 4), &found, 5.0);
    assert!((shift - 0.5).abs() < 0.05 && unmatched == 0, "{shift}");
}

/// Dot centres of a 9 × 6 grid on 50-pixel pitch through `map`.
fn grid_points(map: impl Fn(f64, f64) -> (f64, f64)) -> Vec<[f64; 2]> {
    let mut points = Vec::new();
    for row in 0..6 {
        for column in 0..9 {
            let (x, y) = map(
                f64::from(column) * 50.0 - 200.0,
                f64::from(row) * 50.0 - 125.0,
            );
            points.push([x + 300.0, y + 200.0]);
        }
    }
    points
}

/// A grid rotated by a known angle measures that angle, clockwise positive with y down.
#[test]
fn a_known_rotation_is_recovered() {
    for degrees in [-10.0f64, -1.0, 0.0, 2.0, 5.0] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let points = grid_points(|x, y| (x * cos - y * sin, x * sin + y * cos));
        let measured = grid_angle(&points).unwrap();
        assert!((measured - degrees).abs() < 1e-9, "{degrees}: {measured}");
    }
}

/// A keystone that narrows the top row by a known ratio measures it, and a square grid measures 1.
#[test]
fn a_known_keystone_is_recovered() {
    assert!((keystone(&grid_points(|x, y| (x, y)), true).unwrap() - 1.0).abs() < 1e-12);
    // Rows scaled linearly from 0.8 at the top (y = −125) to 1.0 at the bottom (y = 125).
    let scale = |y: f64| 0.9 + 0.1 * y / 125.0;
    let points = grid_points(|x, y| (x * scale(y), y));
    assert!((keystone(&points, true).unwrap() - 0.8).abs() < 1e-9);
    let points = grid_points(|x, y| (x, y * (0.9 + 0.1 * x / 200.0)));
    assert!((keystone(&points, false).unwrap() - 0.8).abs() < 1e-9);
}

/// An ellipse turned by a known angle measures that orientation, and its profile in its own turned
/// axes finds its edge at `ρ = 1`.
#[test]
fn a_turned_ellipse_has_its_known_orientation_and_edge() {
    let (width, height) = (400usize, 300usize);
    let centre = (200.0, 150.0);
    for degrees in [-30.0f64, 0.0, 30.0, 60.0] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let plane: Vec<f64> = (0..width * height)
            .map(|i| {
                let (dx, dy) = (
                    (i % width) as f64 + 0.5 - centre.0,
                    (i / width) as f64 + 0.5 - centre.1,
                );
                let (u, v) = (dx * cos + dy * sin, -dx * sin + dy * cos);
                f64::from(u8::from((u / 120.0).hypot(v / 60.0) <= 1.0))
            })
            .collect();
        let measured = orientation(&plane, width).unwrap();
        assert!((measured - degrees).abs() < 0.5, "{degrees}: {measured}");
        let profile = radial_profile(&plane, width, centre, (120.0, 60.0), degrees, 50, 2.0);
        assert!((crossing(&profile, 0.5).unwrap() - 1.0).abs() < 0.05);
    }
}

/// A box blur keeps a flat plane and the mean of any plane; band energies scale with the plane's
/// contrast, and a fine pattern's energy sits in the finest band, a coarse one's in a coarser band.
#[test]
fn band_energy_scales_with_contrast_and_separates_scales() {
    let (width, height) = (256usize, 128usize);
    let flat = vec![0.4; width * height];
    assert!(
        box_blur(&flat, width, 5)
            .iter()
            .all(|v| (v - 0.4).abs() < 1e-12)
    );
    let wave = |period: f64, amplitude: f64| -> Vec<f64> {
        (0..width * height)
            .map(|i| 0.5 + amplitude * (std::f64::consts::TAU * (i % width) as f64 / period).sin())
            .collect()
    };
    let radii = [2, 8, 32];
    let fine = band_energy(&wave(4.0, 0.1), width, &radii);
    let coarse = band_energy(&wave(64.0, 0.1), width, &radii);
    assert!(fine[0] > fine[1] && fine[0] > fine[2], "{fine:?}");
    assert!(coarse[2] > coarse[0] && coarse[1] > coarse[0], "{coarse:?}");
    let doubled = band_energy(&wave(64.0, 0.2), width, &radii);
    for (d, c) in doubled.iter().zip(&coarse) {
        assert!((d / c - 2.0).abs() < 1e-9);
    }
}

/// On a photograph-like field of mixed lightness, a pointwise tone curve measures no more locality
/// than its own slope across one bin, and one that pulls each sample towards its neighbourhood's
/// mean measures some.
#[test]
fn tone_locality_tells_a_local_tone_change_from_a_pointwise_one() {
    let mut rng = SplitMix64(11);
    let width = 200usize;
    let neutral: Vec<f64> = (0..width * 100)
        .map(|i| {
            let region = if (i % width) < width / 2 { 40.0 } else { 60.0 };
            (region + rng.next_range(-40.0, 40.0)).clamp(0.0, 100.0)
        })
        .collect();
    let pointwise: Vec<f64> = neutral
        .iter()
        .map(|l| 100.0 * (l / 100.0).powf(0.8))
        .collect();
    assert!(tone_locality(&neutral, &pointwise, 1.0, 20) < 0.5);
    let local: Vec<f64> = neutral
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let region = if (i % width) < width / 2 { 40.0 } else { 60.0 };
            l + 0.4 * (region - l)
        })
        .collect();
    assert!(tone_locality(&neutral, &local, 1.0, 20) > 5.0);
}
