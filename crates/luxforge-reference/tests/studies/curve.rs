//! Independent proofs for the Tone curve reference, the candidate comparison that chose its
//! interpolant, and the oracle fixture production is checked against.
//!
//! The reference lives in `crates/luxforge-reference/src/curve.rs`; the construction, the pixel
//! rule and every property proved here are written out in `docs/design/tone-curve.md`, whose
//! "Reference, study and fixture" section names these tests and quotes the figures
//! `curve_study_figures` prints:
//!
//! ```sh
//! cargo test -p luxforge-reference --test studies curve::
//! cargo test -p luxforge-reference --test studies -- --ignored --nocapture curve::curve_study_figures
//! cargo test -p luxforge-reference --test studies -- --ignored regenerate_committed_curve_case_fixture
//! ```

use super::approximately_equal;
use luxforge_reference::SplitMix64;
use luxforge_reference::colour::{chroma, hue_degrees, to_oklab};
use luxforge_reference::curve::{CurvePoints, curve, curve_pixel, curve_pixel_with_input_error};
use luxforge_reference::srgb;
use luxforge_reference::tone::{ToneParams, luminance, tone_pixel};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Inputs: the named point sets, the random lists and the dense grid.
// ---------------------------------------------------------------------------

/// The design's named point sets, in the order it lists them.
fn named_sets() -> Vec<(&'static str, Vec<[f64; 2]>)> {
    let wiggle: Vec<[f64; 2]> = (0..16)
        .map(|k| {
            let x = f64::from(k) / 15.0;
            let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
            [x, x + 0.03 * sign]
        })
        .collect();
    vec![
        ("identity", vec![[0.0, 0.0], [1.0, 1.0]]),
        ("identity-3", vec![[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]),
        ("midtone-lift", vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]),
        (
            "s-curve",
            vec![[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]],
        ),
        (
            "inverse-s",
            vec![[0.0, 0.0], [0.25, 0.3], [0.75, 0.7], [1.0, 1.0]],
        ),
        ("lifted-black", vec![[0.0, 0.1], [1.0, 1.0]]),
        ("cut-white", vec![[0.0, 0.0], [1.0, 0.9]]),
        ("inner-span", vec![[0.2, 0.0], [0.8, 1.0]]),
        ("wiggle-16", wiggle),
        (
            "flat-pair",
            vec![[0.0, 0.0], [0.4, 0.35], [0.6, 0.35], [1.0, 1.0]],
        ),
        (
            "steep-600",
            vec![[0.0, 0.0], [0.5, 0.0], [0.5025, 1.0], [1.0, 1.0]],
        ),
        (
            "collapsed-4eps",
            vec![
                [0.0, 0.0],
                [0.5, 0.4],
                [0.5 + 4.0 * f64::EPSILON, 0.6],
                [1.0, 1.0],
            ],
        ),
        (
            "collapsed-1e-8",
            vec![[0.0, 0.0], [0.5, 0.4], [0.500_000_01, 0.6], [1.0, 1.0]],
        ),
        ("subnormal", vec![[0.0, 0.0], [1e-310, 1.0], [1.0, 1.0]]),
    ]
}

fn named_set(name: &str) -> Vec<[f64; 2]> {
    named_sets()
        .into_iter()
        .find(|(set, _)| *set == name)
        .unwrap_or_else(|| panic!("no named set {name}"))
        .1
}

/// The sets steeper than the forward tolerance's slope bound, or with collapsed spacing.
const STEEP_SETS: [&str; 4] = ["steep-600", "collapsed-4eps", "collapsed-1e-8", "subnormal"];

/// One seeded admissible list of 2 to 16 points: uniform sorted `x` and `y`, the ends pinned to
/// `0` and `1` about half the time, about a quarter of the outputs repeated (exactly flat
/// segments) and the black left unlifted about a third of the time.
fn random_list(rng: &mut SplitMix64) -> Vec<[f64; 2]> {
    let n = 2 + rng.next_usize(15);
    loop {
        let mut xs: Vec<f64> = (0..n).map(|_| rng.next_range(0.0, 1.0)).collect();
        xs.sort_by(f64::total_cmp);
        if rng.next_bool() {
            xs[0] = 0.0;
        }
        if rng.next_bool() {
            xs[n - 1] = 1.0;
        }
        if xs.windows(2).any(|pair| pair[1] <= pair[0]) {
            continue;
        }
        let mut ys: Vec<f64> = (0..n).map(|_| rng.next_range(0.0, 1.0)).collect();
        ys.sort_by(f64::total_cmp);
        for k in 1..n {
            if rng.next_u32(4) == 0 {
                ys[k] = ys[k - 1];
            }
        }
        if rng.next_u32(3) == 0 {
            ys[0] = 0.0;
        }
        if rng.next_u32(3) == 0 {
            ys[n - 1] = 1.0;
        }
        return xs.into_iter().zip(ys).map(|(x, y)| [x, y]).collect();
    }
}

fn random_lists(count: usize) -> Vec<Vec<[f64; 2]>> {
    let mut rng = SplitMix64(0x7c0e_c0de);
    (0..count).map(|_| random_list(&mut rng)).collect()
}

/// A random list whose points lie on the diagonal with the span covering `[0, 1]`.
fn random_diagonal_list(rng: &mut SplitMix64) -> Vec<[f64; 2]> {
    let n = 2 + rng.next_usize(15);
    let mut xs: Vec<f64> = (0..n - 2).map(|_| rng.next_range(0.0, 1.0)).collect();
    xs.push(0.0);
    xs.push(1.0);
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    xs.into_iter().map(|x| [x, x]).collect()
}

/// Every named set and the 10,000 random lists, labelled.
fn all_lists() -> Vec<(String, Vec<[f64; 2]>)> {
    named_sets()
        .into_iter()
        .map(|(name, points)| (name.to_owned(), points))
        .chain(
            random_lists(10_000)
                .into_iter()
                .enumerate()
                .map(|(index, points)| (format!("random-{index}"), points)),
        )
        .collect()
}

/// The 4001-point grid over the extended domain `[-0.5, 2.0]`.
fn dense_grid() -> Vec<f64> {
    (0..=4000)
        .map(|k| -0.5 + 2.5 * f64::from(k) / 4000.0)
        .collect()
}

/// The grid plus 64 points inside every segment, sorted: what the candidate comparison and the
/// collapsed-spacing proofs sample, so a segment narrower than the grid step is still seen.
fn grid_with_segments(points: &[[f64; 2]]) -> Vec<f64> {
    let mut xs = dense_grid();
    for pair in points.windows(2) {
        let (x0, x1) = (pair[0][0], pair[1][0]);
        for j in 0..=64 {
            xs.push((x0 + (x1 - x0) * f64::from(j) / 64.0).clamp(x0, x1));
        }
    }
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    xs
}

/// `C` on segment `segment` at `samples + 1` points, the ends included, each clamped inside the
/// segment so rounding never hands the sample to a neighbour.
fn segment_samples(points: &[[f64; 2]], segment: usize, samples: u32) -> Vec<f64> {
    let (x0, x1) = (points[segment][0], points[segment + 1][0]);
    (0..=samples)
        .map(|j| (x0 + (x1 - x0) * f64::from(j) / f64::from(samples)).clamp(x0, x1))
        .collect()
}

// ---------------------------------------------------------------------------
// Properties of `C`.
// ---------------------------------------------------------------------------

#[test]
fn interpolates_every_knot() {
    for (name, points) in all_lists() {
        let reference = CurvePoints::new(&points);
        for (index, [x, y]) in points.iter().copied().enumerate() {
            assert_eq!(curve(&reference, x), y, "{name}: knot {index} at x = {x}");
        }
    }
}

#[test]
fn dense_forward_differences_are_nondecreasing_for_named_and_10000_random_lists() {
    let grid = dense_grid();
    for (name, points) in all_lists() {
        let reference = CurvePoints::new(&points);
        let values: Vec<f64> = grid.iter().map(|&x| curve(&reference, x)).collect();
        for (k, pair) in values.windows(2).enumerate() {
            assert!(
                pair[1] >= pair[0],
                "{name}: C decreases from {} at x = {} to {} at x = {}",
                pair[0],
                grid[k],
                pair[1],
                grid[k + 1]
            );
        }
    }
}

#[test]
fn no_segment_leaves_its_knot_values() {
    let grid = dense_grid();
    for (name, points) in all_lists() {
        let reference = CurvePoints::new(&points);
        for segment in 0..points.len() - 1 {
            let (y0, y1) = (points[segment][1], points[segment + 1][1]);
            for x in segment_samples(&points, segment, 256) {
                let value = curve(&reference, x);
                assert!(
                    (y0..=y1).contains(&value),
                    "{name}: segment {segment} gives {value} at x = {x}, outside [{y0}, {y1}]"
                );
            }
        }
        for &x in grid.iter().filter(|x| (0.0..=1.0).contains(*x)) {
            let value = curve(&reference, x);
            assert!(
                (0.0..=1.0).contains(&value),
                "{name}: C({x}) = {value} leaves [0, 1]"
            );
        }
    }
}

#[test]
fn knot_slopes_are_at_most_three_times_the_smaller_secant() {
    for (name, points) in all_lists() {
        let reference = CurvePoints::new(&points);
        let slopes = reference.knot_slopes();
        let secants: Vec<f64> = points
            .windows(2)
            .map(|pair| (pair[1][1] - pair[0][1]) / (pair[1][0] - pair[0][0]))
            .collect();
        let n = points.len();
        assert_eq!(slopes.len(), n, "{name}");
        assert!(
            slopes.iter().all(|d| d.is_finite() && *d >= 0.0),
            "{name}: {slopes:?}"
        );
        let end = |secant: f64| if secant.is_finite() { secant } else { 0.0 };
        assert_eq!(slopes[0], end(secants[0]), "{name}: the first end slope");
        assert_eq!(
            slopes[n - 1],
            end(secants[n - 2]),
            "{name}: the last end slope"
        );
        for i in 1..n - 1 {
            let bound = 3.0 * secants[i - 1].min(secants[i]);
            if bound.is_finite() {
                assert!(
                    slopes[i] <= bound,
                    "{name}: knot {i} slope {} exceeds three times the smaller secant ({bound})",
                    slopes[i]
                );
            } else {
                // Both secants are infinite: the knot belongs only to linear segments.
                assert_eq!(slopes[i], 0.0, "{name}: knot {i}");
            }
            if secants[i - 1] == 0.0 || secants[i] == 0.0 {
                assert_eq!(slopes[i], 0.0, "{name}: knot {i} beside a flat segment");
            }
        }
    }
}

#[test]
fn is_c1_at_interior_knots() {
    for (name, points) in all_lists() {
        let reference = CurvePoints::new(&points);
        let slopes = reference.knot_slopes();
        for i in 1..points.len() - 1 {
            let (h_before, h_after) = (
                points[i][0] - points[i - 1][0],
                points[i + 1][0] - points[i][0],
            );
            let linear = |h: f64, dy: f64| !(dy / h).is_finite();
            if linear(h_before, points[i][1] - points[i - 1][1])
                || linear(h_after, points[i + 1][1] - points[i][1])
            {
                continue;
            }
            // Centred one-sided differences at a millionth of the narrower segment: the
            // truncation error is about C'' times the step, far below the tolerance; spacing
            // too fine for a meaningful step is covered by the collapsed-spacing test instead.
            let step = 1e-6 * h_before.min(h_after);
            if step < 1e-12 {
                continue;
            }
            let x = points[i][0];
            let left = (curve(&reference, x) - curve(&reference, x - step)) / step;
            let right = (curve(&reference, x + step) - curve(&reference, x)) / step;
            let tolerance = 1e-4 * (1.0 + reference.peak_slope());
            assert!(
                (left - slopes[i]).abs() <= tolerance && (right - slopes[i]).abs() <= tolerance,
                "{name}: knot {i} slope {} but left {left} and right {right}",
                slopes[i]
            );
        }
    }
}

#[test]
fn on_diagonal_points_are_the_identity() {
    let grid = dense_grid();
    let mut rng = SplitMix64(0xd1a6);
    let mut lists = vec![named_set("identity"), named_set("identity-3")];
    lists.extend((0..1000).map(|_| random_diagonal_list(&mut rng)));
    let mut largest = 0.0_f64;
    for points in lists {
        let reference = CurvePoints::new(&points);
        assert!(
            reference.knot_slopes().iter().all(|&d| d == 1.0),
            "{points:?}: {:?}",
            reference.knot_slopes()
        );
        assert_eq!(reference.peak_slope(), 1.0, "{points:?}");
        for &x in &grid {
            let deviation = (curve(&reference, x) - x).abs();
            largest = largest.max(deviation);
            assert!(
                deviation <= 2.0 * f64::EPSILON,
                "{points:?}: C({x}) = {}",
                curve(&reference, x)
            );
        }
        for rgb in [
            [0.2, 0.5, 0.9],
            [-0.1, 0.0, 0.3],
            [1.5, 1.2, 0.9],
            [1e-7, 0.0, 0.0],
        ] {
            assert_eq!(curve_pixel(&reference, rgb), rgb, "{points:?}");
        }
    }
    println!("on-diagonal lists: largest |C(x) - x| = {largest:e}");
}

#[test]
fn two_points_are_affine() {
    let mut rng = SplitMix64(0xaff1);
    let mut lists = vec![
        named_set("identity"),
        named_set("lifted-black"),
        named_set("cut-white"),
        named_set("inner-span"),
    ];
    for _ in 0..1000 {
        let mut x = [rng.next_range(0.0, 1.0), rng.next_range(0.0, 1.0)];
        let mut y = [rng.next_range(0.0, 1.0), rng.next_range(0.0, 1.0)];
        x.sort_by(f64::total_cmp);
        y.sort_by(f64::total_cmp);
        if rng.next_bool() {
            x = [0.0, 1.0];
        }
        if x[0] < x[1] {
            lists.push(vec![[x[0], y[0]], [x[1], y[1]]]);
        }
    }
    for points in lists {
        let reference = CurvePoints::new(&points);
        let ([x0, a], [x1, b]) = (points[0], points[1]);
        for k in 0..=1000 {
            let x = f64::from(k) / 1000.0;
            let expected = a + (b - a) * ((x.clamp(x0, x1) - x0) / (x1 - x0));
            let value = curve(&reference, x);
            assert!(
                (value - expected).abs() <= 4.0 * f64::EPSILON,
                "{points:?}: C({x}) = {value}, affine {expected}"
            );
        }
    }
}

#[test]
fn equal_outputs_are_exactly_flat() {
    let mut flat_segments = 0usize;
    for (name, points) in all_lists() {
        let reference = CurvePoints::new(&points);
        for segment in 0..points.len() - 1 {
            let y = points[segment][1];
            if points[segment + 1][1] != y {
                continue;
            }
            flat_segments += 1;
            assert_eq!(
                reference.knot_slopes()[segment],
                0.0,
                "{name}: knot {segment}"
            );
            assert_eq!(
                reference.knot_slopes()[segment + 1],
                0.0,
                "{name}: knot {}",
                segment + 1
            );
            for x in segment_samples(&points, segment, 256) {
                assert_eq!(
                    curve(&reference, x),
                    y,
                    "{name}: segment {segment} at x = {x}"
                );
            }
        }
    }
    let flat_pair = CurvePoints::new(&named_set("flat-pair"));
    for k in 0..=2000 {
        let x = 0.4 + 0.2 * f64::from(k) / 2000.0;
        assert_eq!(
            curve(&flat_pair, x.clamp(0.4, 0.6)),
            0.35,
            "flat-pair at {x}"
        );
    }
    assert!(
        flat_segments > 1000,
        "only {flat_segments} flat segments were drawn"
    );
}

#[test]
fn subnormal_and_collapsed_spacing_stays_finite_and_monotone() {
    for name in STEEP_SETS {
        let points = named_set(name);
        let reference = CurvePoints::new(&points);
        assert!(
            reference.knot_slopes().iter().all(|d| d.is_finite()),
            "{name}: {:?}",
            reference.knot_slopes()
        );
        // The grid, every segment densely, and 64 ulps either side of every knot.
        let mut xs = grid_with_segments(&points);
        for [x, _] in &points {
            let mut below = *x;
            let mut above = *x;
            for _ in 0..64 {
                below = below.next_down();
                above = above.next_up();
                xs.push(below);
                xs.push(above);
            }
        }
        xs.sort_by(f64::total_cmp);
        xs.dedup();
        let values: Vec<f64> = xs.iter().map(|&x| curve(&reference, x)).collect();
        assert!(
            values.iter().all(|v| v.is_finite()),
            "{name}: a non-finite C"
        );
        // Samples a few ulps apart are closer than the cubic's own f64 evaluation rounding, so
        // monotonicity is asserted to within 8 ulps of the value: a real fold would be far
        // larger. The 4001-point grid proof above holds exactly.
        let mut largest_rounding = 0.0_f64;
        for (k, pair) in values.windows(2).enumerate() {
            let drop = pair[0] - pair[1];
            largest_rounding = largest_rounding.max(drop);
            assert!(
                drop <= 8.0 * f64::EPSILON * pair[0].abs().max(1.0),
                "{name}: C decreases by {drop} between x = {} and {}",
                xs[k],
                xs[k + 1]
            );
        }
        println!(
            "{name}: {} samples, largest rounding decrease {largest_rounding:e}",
            xs.len()
        );
        for [x, y] in &points {
            assert_eq!(curve(&reference, *x), *y, "{name}: knot at {x}");
        }
        // The pixel rule on the fixture's grey ramp: finite and non-decreasing. On a plateau of
        // `C` the output is `grey · (L_out / L)` with `L` the weighted sum of three equal
        // channels, which rounds a few ulps either side of `grey`, so the pixel path is
        // non-decreasing to within that rounding (`C` itself is exactly, above).
        let mut previous = f64::NEG_INFINITY;
        for grey in grey_ramp() {
            let out = curve_pixel(&reference, [grey; 3]);
            assert!(
                out.iter().all(|v| v.is_finite()),
                "{name}: {grey} -> {out:?}"
            );
            assert!(
                out[0] >= previous - 4.0 * f64::EPSILON * previous.abs(),
                "{name}: the grey ramp decreases from {previous} to {} at {grey}",
                out[0]
            );
            previous = out[0];
        }
    }
    let peak = |name: &str| CurvePoints::new(&named_set(name)).peak_slope();
    assert!(
        (peak("steep-600") - 600.0).abs() < 1e-9,
        "steep-600: {}",
        peak("steep-600")
    );
    assert!(peak("collapsed-4eps").is_finite() && peak("collapsed-4eps") > 1e14);
    assert!(peak("collapsed-1e-8").is_finite() && peak("collapsed-1e-8") > 1e7);
    assert_eq!(peak("subnormal"), f64::INFINITY);
}

// ---------------------------------------------------------------------------
// Properties of the pixel rule.
// ---------------------------------------------------------------------------

/// The fixture's grey ramp: encoded luminance over `[-0.5, 2.0]` in 64 steps, decoded and cast to
/// `f32`.
fn grey_ramp() -> Vec<f64> {
    (0..=64)
        .map(|k| f64::from(srgb::decode_encoded(-0.5 + 2.5 * f64::from(k) / 64.0) as f32))
        .collect()
}

fn random_colour(rng: &mut SplitMix64, lo: f64, hi: f64) -> [f64; 3] {
    std::array::from_fn(|_| rng.next_range(lo, hi))
}

#[test]
fn greys_stay_grey_and_hue_is_kept_when_black_is_not_lifted() {
    let mut rng = SplitMix64(0x6e7);
    let mut largest_hue = 0.0_f64;
    let mut largest_saturation = 0.0_f64;
    let lists: Vec<(String, Vec<[f64; 2]>)> = all_lists().into_iter().take(14 + 1000).collect();
    for (name, points) in lists {
        let reference = CurvePoints::new(&points);
        for grey in grey_ramp() {
            let out = curve_pixel(&reference, [grey; 3]);
            assert!(
                out[0].to_bits() == out[1].to_bits() && out[1].to_bits() == out[2].to_bits(),
                "{name}: grey {grey} -> {out:?}"
            );
        }
        if points[0][1] != 0.0 {
            continue;
        }
        assert_eq!(reference.floor(), 0.0, "{name}");
        for _ in 0..200 {
            let rgb = random_colour(&mut rng, 0.0, 1.2);
            if luminance(rgb) < 1e-4 {
                continue;
            }
            let out = curve_pixel(&reference, rgb);
            if luminance(out) < 1e-4 {
                // The curve sent this tone to (near) black: there is no hue left to compare.
                continue;
            }
            // A uniform non-negative scale: every channel pair keeps its ratio.
            for (a, b) in [(0, 1), (1, 2), (0, 2)] {
                let (cross_1, cross_2) = (out[a] * rgb[b], out[b] * rgb[a]);
                assert!(
                    (cross_1 - cross_2).abs() <= 1e-14 * cross_1.abs().max(cross_2.abs()),
                    "{name}: {rgb:?} -> {out:?} is not a uniform scale"
                );
            }
            let (lab_in, lab_out) = (to_oklab(rgb), to_oklab(out));
            if chroma(lab_in) > 1e-3 {
                let mut difference = (hue_degrees(lab_out) - hue_degrees(lab_in)).abs();
                difference = difference.min(360.0 - difference);
                largest_hue = largest_hue.max(difference);
                assert!(
                    difference < 1e-9,
                    "{name}: {rgb:?} hue moves {difference} degrees"
                );
                let saturation_in = chroma(lab_in) / lab_in.l;
                let saturation_out = chroma(lab_out) / lab_out.l;
                let relative = (saturation_out - saturation_in).abs() / saturation_in;
                largest_saturation = largest_saturation.max(relative);
                assert!(
                    relative < 1e-9,
                    "{name}: {rgb:?} relative saturation moves {relative}"
                );
            }
        }
    }
    println!(
        "unlifted black: largest Oklab hue change {largest_hue:e} degrees, largest relative \
         saturation change {largest_saturation:e}"
    );
}

#[test]
fn a_lifted_black_never_inverts_a_channel_about_the_floor() {
    let mut rng = SplitMix64(0xf100);
    let mut lifted: Vec<(String, Vec<[f64; 2]>)> = all_lists()
        .into_iter()
        .filter(|(_, points)| points[0][1] > 0.0)
        .collect();
    assert!(lifted.len() > 1000, "only {} lifted lists", lifted.len());
    lifted.truncate(2000);
    for (name, points) in lifted {
        let reference = CurvePoints::new(&points);
        let floor = reference.floor();
        assert!(floor > 0.0, "{name}");
        for _ in 0..100 {
            let rgb = random_colour(&mut rng, -0.2, 1.5);
            let l_in = luminance(rgb);
            if l_in.abs() < 1e-6 {
                continue;
            }
            let l_out = srgb::decode_encoded(curve(&reference, srgb::encode_extended(l_in)));
            let scale = (l_out - floor) / l_in;
            assert!(scale >= 0.0, "{name}: {rgb:?} scales by {scale}");
            let out = curve_pixel(&reference, rgb);
            for channel in 0..3 {
                let offset = out[channel] - floor;
                assert!(
                    offset * rgb[channel] >= 0.0,
                    "{name}: {rgb:?} -> {out:?}: channel {channel} crosses the floor {floor}"
                );
            }
            for (a, b) in [(0, 1), (1, 2), (0, 2)] {
                if rgb[a] <= rgb[b] {
                    assert!(
                        out[a] <= out[b],
                        "{name}: {rgb:?} -> {out:?} reorders {a} and {b}"
                    );
                } else {
                    assert!(
                        out[a] >= out[b],
                        "{name}: {rgb:?} -> {out:?} reorders {a} and {b}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_curve_through_basic_black_and_white_points_matches_basic_when_black_is_not_lifted() {
    let mut rng = SplitMix64(0xba51c);
    let mut settings: Vec<(f64, f64)> = Vec::new();
    for whites in [0.0, 25.0, 50.0, 100.0] {
        for blacks in [0.0, -25.0, -50.0, -100.0] {
            settings.push((whites, blacks));
        }
    }
    settings.extend((0..32).map(|_| (rng.next_range(0.0, 100.0), rng.next_range(-100.0, 0.0))));
    let mut compared = 0usize;
    let mut largest = 0.0_f64;
    for (whites, blacks) in settings {
        // Basic's Whites/Blacks stage: `(x - bp) / (wp - bp)` with these endpoints
        // (docs/design/basic-tone.md, K_W = K_B = 0.25).
        let wp = 1.0 - (whites / 100.0) * 0.25;
        let bp = -(blacks / 100.0) * 0.25;
        let params = ToneParams {
            whites,
            blacks,
            ..ToneParams::NEUTRAL
        };
        let reference = CurvePoints::new(&[[bp, 0.0], [wp, 1.0]]);
        for _ in 0..2000 {
            let rgb = random_colour(&mut rng, 0.0, 1.0);
            let x = srgb::encode_extended(luminance(rgb));
            if !(bp..=wp).contains(&x) {
                continue;
            }
            compared += 1;
            let (basic, ours) = (tone_pixel(rgb, params), curve_pixel(&reference, rgb));
            for channel in 0..3 {
                let deviation = (ours[channel] - basic[channel]).abs();
                largest = largest.max(deviation / (1.0 + basic[channel].abs()));
                assert!(
                    deviation <= 1e-12 + 1e-12 * basic[channel].abs(),
                    "Whites {whites} Blacks {blacks}: {rgb:?} -> curve {ours:?}, Basic {basic:?}"
                );
            }
        }
    }
    assert!(compared > 50_000, "only {compared} pixels compared");
    println!("Basic cross-check: {compared} pixels, largest deviation {largest:e}");
}

// ---------------------------------------------------------------------------
// Lifted-black noise.
// ---------------------------------------------------------------------------

/// Standard normal deviates by Box–Muller over `SplitMix64`, both deviates of each pair used.
struct Normals {
    rng: SplitMix64,
    spare: Option<f64>,
}

impl Normals {
    fn new(seed: u64) -> Self {
        Self {
            rng: SplitMix64(seed),
            spare: None,
        }
    }

    fn next(&mut self) -> f64 {
        if let Some(value) = self.spare.take() {
            return value;
        }
        let u1 = 1.0 - self.rng.next_range(0.0, 1.0);
        let u2 = self.rng.next_range(0.0, 1.0);
        let radius = (-2.0 * u1.ln()).sqrt();
        let angle = 2.0 * std::f64::consts::PI * u2;
        self.spare = Some(radius * angle.sin());
        radius * angle.cos()
    }
}

/// The near-black noise field: 20,000 pixels, each channel independently normal with mean and
/// standard deviation `0.0015` in linear light.
fn noise_field() -> Vec<[f64; 3]> {
    let mut normals = Normals::new(1);
    (0..20_000)
        .map(|_| std::array::from_fn(|_| 0.0015 + 0.0015 * normals.next()))
        .collect()
}

/// The frozen luminance-ratio reconstruction applied to the curve without subtracting the floor:
/// what the curve unit would render had it kept Basic's rule.
fn frozen_ratio_pixel(points: &CurvePoints, rgb: [f64; 3]) -> [f64; 3] {
    let l_in = luminance(rgb);
    let l_out = srgb::decode_encoded(curve(points, srgb::encode_extended(l_in)));
    if l_in.abs() < 1e-6 {
        rgb.map(|channel| channel + (l_out - l_in))
    } else {
        rgb.map(|channel| channel * (l_out / l_in))
    }
}

fn code_spread(rgb: [f64; 3]) -> u8 {
    let codes = rgb.map(srgb::code);
    codes.iter().max().unwrap() - codes.iter().min().unwrap()
}

/// Spread percentiles by nearest rank, `sorted[ceil(p · n / 100) - 1]`: the definition of
/// `luxforge_testbase::Distribution`, which this crate may not depend on (`cargo xtask
/// check-repository`'s `independent-references` rule), so the `one-distribution` rule names this
/// file as its one other home.
struct Spread {
    sorted: Vec<u8>,
}

impl Spread {
    fn of(field: &[[f64; 3]], pixel: impl Fn([f64; 3]) -> [f64; 3]) -> Self {
        let mut sorted: Vec<u8> = field.iter().map(|&rgb| code_spread(pixel(rgb))).collect();
        sorted.sort_unstable();
        Self { sorted }
    }

    fn percentile(&self, p: f64) -> u8 {
        let rank = ((p / 100.0) * self.sorted.len() as f64).ceil() as usize;
        self.sorted[rank.max(1) - 1]
    }

    fn max(&self) -> u8 {
        *self.sorted.last().unwrap()
    }

    fn share_above(&self, codes: u8) -> f64 {
        self.sorted.iter().filter(|&&s| s > codes).count() as f64 / self.sorted.len() as f64
    }

    fn describe(&self) -> String {
        format!(
            "p50 {}, p90 {}, p99 {}, max {}; {:.1}% of pixels above 40 codes",
            self.percentile(50.0),
            self.percentile(90.0),
            self.percentile(99.0),
            self.max(),
            100.0 * self.share_above(40)
        )
    }
}

#[test]
fn lifted_black_near_black_noise_spread() {
    let field = noise_field();
    let points = CurvePoints::new(&named_set("lifted-black"));
    let floor_subtracted = Spread::of(&field, |rgb| curve_pixel(&points, rgb));
    let frozen = Spread::of(&field, |rgb| frozen_ratio_pixel(&points, rgb));
    println!(
        "lifted-black noise spread, floor-subtracted ratio: {}",
        floor_subtracted.describe()
    );
    println!(
        "lifted-black noise spread, frozen ratio: {}",
        frozen.describe()
    );
    assert!(
        floor_subtracted.percentile(99.0) <= 24,
        "99th-percentile spread {} codes",
        floor_subtracted.percentile(99.0)
    );
    assert!(
        floor_subtracted.max() <= 48,
        "largest spread {} codes",
        floor_subtracted.max()
    );
}

// ---------------------------------------------------------------------------
// The committed fixture.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct PointSetEntry {
    points: Vec<[f64; 2]>,
    /// [`CurvePoints::peak_slope`]; `null` when it is not finite in `f64` (a segment whose secant
    /// overflows, which the curve evaluates as linear).
    peak_slope: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct CurveCase {
    name: String,
    set: String,
    input_linear_rgb: [f64; 3],
    expected_linear_rgb: [f64; 3],
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct CurveFixture {
    point_sets: BTreeMap<String, PointSetEntry>,
    cases: Vec<CurveCase>,
    samples: BTreeMap<String, Vec<[f64; 2]>>,
}

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/curve/curve-cases.json")
}

fn f32_triple(rgb: [f64; 3]) -> [f64; 3] {
    rgb.map(|v| f64::from(v as f32))
}

/// Every input each set is evaluated on, `f32`-representable.
fn fixture_inputs() -> Vec<(String, [f64; 3])> {
    let mut inputs: Vec<(String, [f64; 3])> = grey_ramp()
        .into_iter()
        .enumerate()
        .map(|(k, grey)| (format!("ramp-{k:02}"), [grey; 3]))
        .collect();
    let corners: [(&str, [f64; 3]); 8] = [
        ("black", [0.0, 0.0, 0.0]),
        ("red", [1.0, 0.0, 0.0]),
        ("green", [0.0, 1.0, 0.0]),
        ("blue", [0.0, 0.0, 1.0]),
        ("cyan", [0.0, 1.0, 1.0]),
        ("magenta", [1.0, 0.0, 1.0]),
        ("yellow", [1.0, 1.0, 0.0]),
        ("white", [1.0, 1.0, 1.0]),
    ];
    for (name, rgb) in corners {
        inputs.push((name.to_owned(), rgb));
    }
    for (name, rgb) in corners {
        inputs.push((format!("{name}-quarter"), rgb.map(|v| v * 0.25)));
    }
    for (name, rgb) in [
        ("near-black-chroma-1", [3e-4, 1e-5, 1e-5]),
        ("near-black-chroma-2", [2e-4, -1e-4, 3e-4]),
        ("negative-luminance-1", [-0.1, 0.0, 0.0]),
        ("negative-luminance-2", [0.2, -0.1, 0.1]),
        ("over-white", [1.5, 1.2, 0.9]),
    ] {
        inputs.push((name.to_owned(), f32_triple(rgb)));
    }
    inputs
}

fn build_fixture() -> CurveFixture {
    let inputs = fixture_inputs();
    let mut point_sets = BTreeMap::new();
    let mut cases = Vec::new();
    let mut samples = BTreeMap::new();
    for (set, points) in named_sets() {
        let reference = CurvePoints::new(&points);
        let peak = reference.peak_slope();
        point_sets.insert(
            set.to_owned(),
            PointSetEntry {
                points: points.clone(),
                peak_slope: peak.is_finite().then_some(peak),
            },
        );
        for (input, rgb) in &inputs {
            cases.push(CurveCase {
                name: format!("{set}/{input}"),
                set: set.to_owned(),
                input_linear_rgb: *rgb,
                expected_linear_rgb: curve_pixel(&reference, *rgb),
            });
        }
        samples.insert(
            set.to_owned(),
            (0..=256)
                .map(|i| {
                    let x = f64::from(i) / 256.0;
                    [x, curve(&reference, x)]
                })
                .collect(),
        );
    }
    CurveFixture {
        point_sets,
        cases,
        samples,
    }
}

fn assert_close(label: &str, committed: f64, fresh: f64) {
    assert!(
        approximately_equal(committed, fresh),
        "fixtures/curve/curve-cases.json is stale at {label}: committed {committed}, reference \
         {fresh}; regenerate it with `cargo test -p luxforge-reference --test studies -- \
         --ignored regenerate_committed_curve_case_fixture`"
    );
}

#[test]
fn committed_curve_case_fixture_matches_the_reference() {
    let path = fixture_path();
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let committed: CurveFixture = serde_json::from_str(&contents)
        .expect("fixtures/curve/curve-cases.json must be valid JSON");
    let fresh = build_fixture();

    assert_eq!(
        committed.point_sets.keys().collect::<Vec<_>>(),
        fresh.point_sets.keys().collect::<Vec<_>>(),
        "the committed point sets differ from the named sets"
    );
    for (set, entry) in &committed.point_sets {
        let fresh_entry = &fresh.point_sets[set];
        assert_eq!(entry.points, fresh_entry.points, "{set}: points");
        match (entry.peak_slope, fresh_entry.peak_slope) {
            (Some(a), Some(b)) => assert_close(&format!("{set} peak_slope"), a, b),
            (None, None) => {}
            (a, b) => panic!("{set}: peak_slope committed {a:?}, reference {b:?}"),
        }
    }

    assert_eq!(committed.cases.len(), fresh.cases.len(), "case count");
    for (committed_case, fresh_case) in committed.cases.iter().zip(&fresh.cases) {
        assert_eq!(
            committed_case.name, fresh_case.name,
            "case order or names drifted"
        );
        assert_eq!(
            committed_case.set, fresh_case.set,
            "{}",
            committed_case.name
        );
        for channel in 0..3 {
            assert_close(
                &format!("{} input", committed_case.name),
                committed_case.input_linear_rgb[channel],
                fresh_case.input_linear_rgb[channel],
            );
            assert_close(
                &format!("{} expected", committed_case.name),
                committed_case.expected_linear_rgb[channel],
                fresh_case.expected_linear_rgb[channel],
            );
        }
        assert!(
            committed_case
                .input_linear_rgb
                .iter()
                .all(|&v| f64::from(v as f32) == v),
            "{}: the input is not f32-representable",
            committed_case.name
        );
    }

    assert_eq!(
        committed.samples.keys().collect::<Vec<_>>(),
        fresh.samples.keys().collect::<Vec<_>>(),
        "sample sets"
    );
    for (set, committed_samples) in &committed.samples {
        let fresh_samples = &fresh.samples[set];
        assert_eq!(committed_samples.len(), 257, "{set}: sample count");
        for (i, (a, b)) in committed_samples.iter().zip(fresh_samples).enumerate() {
            assert_eq!(a[0], b[0], "{set}: sample {i} x");
            assert_close(&format!("{set} sample {i}"), a[1], b[1]);
        }
    }
}

#[test]
#[ignore = "regenerates the committed oracle fixture; run explicitly after an intended change to \
            crates/luxforge-reference/src/curve.rs, and re-freeze docs/design/tone-curve.md to match"]
fn regenerate_committed_curve_case_fixture() {
    let fixture = build_fixture();
    let json = serde_json::to_string_pretty(&fixture).expect("serializable");
    let path = fixture_path();
    std::fs::create_dir_all(path.parent().expect("fixture directory")).expect("create directory");
    std::fs::write(&path, json + "\n").expect("write fixtures/curve/curve-cases.json");
}

// ---------------------------------------------------------------------------
// The candidate comparison and the figures the design quotes.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Candidate {
    Linear,
    NaturalCubic,
    CatmullRom,
    Pchip,
}

impl Candidate {
    const ALL: [Self; 4] = [
        Self::Linear,
        Self::NaturalCubic,
        Self::CatmullRom,
        Self::Pchip,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::NaturalCubic => "natural cubic",
            Self::CatmullRom => "Catmull-Rom",
            Self::Pchip => "PCHIP",
        }
    }
}

/// One candidate interpolant through a point list, with the same flat holds and unit-slope tails
/// as the reference, so only the span's construction differs.
struct Interpolant {
    candidate: Candidate,
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// Knot tangents (Catmull-Rom) or second derivatives (natural cubic).
    knots: Vec<f64>,
    pchip: CurvePoints,
}

impl Interpolant {
    fn new(candidate: Candidate, points: &[[f64; 2]]) -> Self {
        let xs: Vec<f64> = points.iter().map(|p| p[0]).collect();
        let ys: Vec<f64> = points.iter().map(|p| p[1]).collect();
        let n = xs.len();
        let h: Vec<f64> = xs.windows(2).map(|p| p[1] - p[0]).collect();
        let secant: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / h[i]).collect();
        let knots = match candidate {
            Candidate::Linear | Candidate::Pchip => Vec::new(),
            Candidate::CatmullRom => (0..n)
                .map(|i| match i {
                    0 => secant[0],
                    i if i == n - 1 => secant[n - 2],
                    i => (ys[i + 1] - ys[i - 1]) / (xs[i + 1] - xs[i - 1]),
                })
                .collect(),
            Candidate::NaturalCubic => {
                // h_{i-1} M_{i-1} + 2 (h_{i-1} + h_i) M_i + h_i M_{i+1} = 6 (Δ_i − Δ_{i-1}),
                // M_0 = M_{n-1} = 0, by the Thomas algorithm.
                let mut m = vec![0.0; n];
                if n > 2 {
                    let size = n - 2;
                    let mut diagonal: Vec<f64> =
                        (1..n - 1).map(|i| 2.0 * (h[i - 1] + h[i])).collect();
                    let mut rhs: Vec<f64> = (1..n - 1)
                        .map(|i| 6.0 * (secant[i] - secant[i - 1]))
                        .collect();
                    for k in 1..size {
                        let factor = h[k] / diagonal[k - 1];
                        diagonal[k] -= factor * h[k];
                        rhs[k] -= factor * rhs[k - 1];
                    }
                    m[size] = rhs[size - 1] / diagonal[size - 1];
                    for k in (0..size - 1).rev() {
                        m[k + 1] = (rhs[k] - h[k + 1] * m[k + 2]) / diagonal[k];
                    }
                }
                m
            }
        };
        Self {
            candidate,
            pchip: CurvePoints::new(points),
            xs,
            ys,
            knots,
        }
    }

    fn value(&self, x: f64) -> f64 {
        if let Candidate::Pchip = self.candidate {
            return curve(&self.pchip, x);
        }
        let n = self.xs.len();
        if x < 0.0 {
            return self.ys[0] + x;
        }
        if x > 1.0 {
            return self.ys[n - 1] + (x - 1.0);
        }
        if x <= self.xs[0] {
            return self.ys[0];
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1];
        }
        let i = (self.xs.partition_point(|&k| k <= x) - 1).min(n - 2);
        let (x0, x1, y0, y1) = (self.xs[i], self.xs[i + 1], self.ys[i], self.ys[i + 1]);
        let h = x1 - x0;
        let t = (x - x0) / h;
        match self.candidate {
            Candidate::Linear => y0 + (y1 - y0) * t,
            Candidate::CatmullRom => {
                let (t2, t3) = (t * t, t * t * t);
                y0 * (2.0 * t3 - 3.0 * t2 + 1.0)
                    + h * self.knots[i] * (t3 - 2.0 * t2 + t)
                    + y1 * (-2.0 * t3 + 3.0 * t2)
                    + h * self.knots[i + 1] * (t3 - t2)
            }
            Candidate::NaturalCubic => {
                let (m0, m1) = (self.knots[i], self.knots[i + 1]);
                let (a, b) = (x1 - x, x - x0);
                m0 * a * a * a / (6.0 * h)
                    + m1 * b * b * b / (6.0 * h)
                    + (y0 / h - m0 * h / 6.0) * a
                    + (y1 / h - m1 * h / 6.0) * b
            }
            Candidate::Pchip => unreachable!(),
        }
    }

    /// The slope of segment `i` at its start (`end == false`) or its end.
    fn segment_slope(&self, i: usize, end: bool) -> f64 {
        let h = self.xs[i + 1] - self.xs[i];
        let secant = (self.ys[i + 1] - self.ys[i]) / h;
        match self.candidate {
            Candidate::Linear => secant,
            Candidate::CatmullRom => self.knots[if end { i + 1 } else { i }],
            Candidate::Pchip => {
                if self.ys[i] == self.ys[i + 1] {
                    0.0
                } else if !secant.is_finite() {
                    secant
                } else {
                    self.pchip.knot_slopes()[if end { i + 1 } else { i }]
                }
            }
            Candidate::NaturalCubic => {
                let (m0, m1) = (self.knots[i], self.knots[i + 1]);
                if end {
                    secant + h * (2.0 * m1 + m0) / 6.0
                } else {
                    secant - h * (2.0 * m0 + m1) / 6.0
                }
            }
        }
    }

    /// The largest jump in slope across an interior knot.
    fn largest_slope_jump(&self) -> f64 {
        (1..self.xs.len() - 1)
            .map(|i| {
                let jump = (self.segment_slope(i, false) - self.segment_slope(i - 1, true)).abs();
                if jump.is_nan() { f64::INFINITY } else { jump }
            })
            .fold(0.0, f64::max)
    }
}

struct CandidateFigures {
    overshoot: f64,
    segment_excursion: f64,
    inversions: usize,
    non_finite: usize,
    slope_jump: f64,
    distinct_codes: usize,
    longest_run: usize,
}

fn candidate_figures(candidate: Candidate, points: &[[f64; 2]]) -> CandidateFigures {
    let interpolant = Interpolant::new(candidate, points);
    let xs = grid_with_segments(points);
    let values: Vec<f64> = xs.iter().map(|&x| interpolant.value(x)).collect();
    let non_finite = values.iter().filter(|v| !v.is_finite()).count();
    let mut overshoot = 0.0_f64;
    for (&x, &value) in xs.iter().zip(&values) {
        if (0.0..=1.0).contains(&x) && value.is_finite() {
            overshoot = overshoot.max(value - 1.0).max(-value);
        }
    }
    let mut segment_excursion = 0.0_f64;
    for segment in 0..points.len() - 1 {
        let (y0, y1) = (points[segment][1], points[segment + 1][1]);
        for x in segment_samples(points, segment, 256) {
            let value = interpolant.value(x);
            if value.is_finite() {
                segment_excursion = segment_excursion.max(value - y1).max(y0 - value);
            }
        }
    }
    // Samples a few ulps apart are closer than an interpolant's own f64 rounding, so a step down
    // counts as an inversion only beyond 8 ulps of the value, as in the collapsed-spacing proof.
    let inversions = values
        .windows(2)
        .filter(|pair| pair[0] - pair[1] > 8.0 * f64::EPSILON * pair[0].abs().max(1.0))
        .count();
    let ramp = ramp_codes(|x| interpolant.value(x));
    CandidateFigures {
        overshoot,
        segment_excursion,
        inversions,
        non_finite,
        slope_jump: interpolant.largest_slope_jump(),
        distinct_codes: distinct_codes(&ramp),
        longest_run: longest_run(&ramp),
    }
}

/// The 512-pixel 8-bit grey ramp through a curve: input code `round(k · 255 / 511)`, output the
/// curve's encoded value quantized.
fn ramp_codes(curve_at: impl Fn(f64) -> f64) -> Vec<u8> {
    (0..512)
        .map(|k| {
            let code = (f64::from(k) * 255.0 / 511.0).round();
            srgb::quantize(curve_at(code / 255.0))
        })
        .collect()
}

fn distinct_codes(codes: &[u8]) -> usize {
    let mut sorted = codes.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.len()
}

fn longest_run(codes: &[u8]) -> usize {
    let (mut longest, mut run) = (0usize, 0usize);
    for (k, &code) in codes.iter().enumerate() {
        run = if k > 0 && code == codes[k - 1] {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
    }
    longest
}

fn format_figure(value: f64) -> String {
    if value.is_infinite() {
        "inf".to_owned()
    } else if value == 0.0 {
        "0".to_owned()
    } else if value.abs() >= 1e4 || value.abs() < 1e-3 {
        format!("{value:.3e}")
    } else {
        format!("{value:.4}")
    }
}

#[test]
#[ignore = "run explicitly to reproduce the figures quoted in docs/design/tone-curve.md"]
fn curve_study_figures() {
    println!("== Named sets: peak slope and knot slopes");
    for (name, points) in named_sets() {
        let reference = CurvePoints::new(&points);
        println!(
            "{name}: peak_slope {}, floor {}, knot slopes [{}]",
            format_figure(reference.peak_slope()),
            format_figure(reference.floor()),
            reference
                .knot_slopes()
                .iter()
                .map(|&d| format_figure(d))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    println!(
        "\n== Candidate comparison: largest overshoot beyond [0, 1] | largest excursion beyond a \
         segment's knot values | inversions | non-finite values (grid plus 64 points per segment) \
         | largest slope jump at an interior knot | 512-pixel ramp: distinct codes, longest run"
    );
    let mut totals: Vec<(Candidate, usize, usize, f64, f64)> = Vec::new();
    for candidate in Candidate::ALL {
        let (mut sets_overshooting, mut sets_inverting) = (0usize, 0usize);
        let (mut worst_overshoot, mut worst_jump) = (0.0_f64, 0.0_f64);
        for (name, points) in named_sets() {
            let figures = candidate_figures(candidate, &points);
            println!(
                "{:<14} {:<14} overshoot {:>10} | excursion {:>10} | inversions {:>4} | \
                 non-finite {:>4} | slope jump {:>10} | ramp {:>3} codes, run {:>3}",
                candidate.name(),
                name,
                format_figure(figures.overshoot),
                format_figure(figures.segment_excursion),
                figures.inversions,
                figures.non_finite,
                format_figure(figures.slope_jump),
                figures.distinct_codes,
                figures.longest_run,
            );
            if figures.overshoot > 0.0 || figures.non_finite > 0 {
                sets_overshooting += 1;
            }
            if figures.inversions > 0 || figures.non_finite > 0 {
                sets_inverting += 1;
            }
            worst_overshoot = worst_overshoot.max(figures.overshoot);
            if !STEEP_SETS.contains(&name) {
                worst_jump = worst_jump.max(figures.slope_jump);
            }
        }
        totals.push((
            candidate,
            sets_overshooting,
            sets_inverting,
            worst_overshoot,
            worst_jump,
        ));
    }
    println!("\n== Candidate summary over the 14 named sets");
    for (candidate, overshooting, inverting, overshoot, jump) in totals {
        println!(
            "{}: {overshooting} sets leave [0, 1] or go non-finite (largest overshoot {}), \
             {inverting} sets invert or go non-finite, largest slope jump outside the steep sets {}",
            candidate.name(),
            format_figure(overshoot),
            format_figure(jump)
        );
    }

    println!("\n== Random lists (10,000): PCHIP against the free splines");
    for candidate in Candidate::ALL {
        let (mut overshooting, mut inverting) = (0usize, 0usize);
        let mut worst = 0.0_f64;
        for points in random_lists(10_000) {
            let interpolant = Interpolant::new(candidate, &points);
            let values: Vec<f64> = dense_grid().iter().map(|&x| interpolant.value(x)).collect();
            let over = dense_grid()
                .iter()
                .zip(&values)
                .filter(|(x, _)| (0.0..=1.0).contains(*x))
                .map(|(_, &v)| (v - 1.0).max(-v).max(0.0))
                .fold(0.0, f64::max);
            if over > 0.0 {
                overshooting += 1;
            }
            worst = worst.max(over);
            if values.windows(2).any(|pair| pair[1] < pair[0]) {
                inverting += 1;
            }
        }
        println!(
            "{}: {overshooting} lists leave [0, 1] (largest {}), {inverting} lists invert",
            candidate.name(),
            format_figure(worst)
        );
    }

    println!("\n== Lifted-black noise (20,000 pixels, mean and sd 0.0015 linear)");
    let field = noise_field();
    let lifted = CurvePoints::new(&named_set("lifted-black"));
    println!(
        "floor-subtracted ratio: {}",
        Spread::of(&field, |rgb| curve_pixel(&lifted, rgb)).describe()
    );
    println!(
        "frozen ratio:           {}",
        Spread::of(&field, |rgb| frozen_ratio_pixel(&lifted, rgb)).describe()
    );
    println!(
        "no curve (the field):   {}",
        Spread::of(&field, |rgb| rgb).describe()
    );
    let blacks = ToneParams {
        blacks: 100.0,
        ..ToneParams::NEUTRAL
    };
    println!(
        "Basic Blacks +100:      {}",
        Spread::of(&field, |rgb| tone_pixel(rgb, blacks)).describe()
    );
    let probe = [0.2, 0.05, 0.02];
    println!(
        "probe {probe:?} under lifted-black: floor-subtracted codes {:?}, frozen ratio codes {:?}",
        curve_pixel(&lifted, probe).map(srgb::code),
        frozen_ratio_pixel(&lifted, probe).map(srgb::code)
    );

    println!("\n== Reference round trips");
    let mut encode_round_trip = 0.0_f64;
    for k in 0..=25_000 {
        let l = -0.5 + 2.5 * f64::from(k) / 25_000.0;
        let back = srgb::decode_encoded(srgb::encode_extended(l));
        encode_round_trip = encode_round_trip.max((back - l).abs() / l.abs().max(1e-300));
    }
    println!(
        "extended encode then decode over linear [-0.5, 2.0] (25,001 points): largest relative \
         error {}",
        format_figure(encode_round_trip)
    );
    let mut rng = SplitMix64(0xd1a6);
    let mut identity_error = 0.0_f64;
    for _ in 0..1000 {
        let reference = CurvePoints::new(&random_diagonal_list(&mut rng));
        for x in dense_grid() {
            identity_error = identity_error.max((curve(&reference, x) - x).abs());
        }
    }
    println!(
        "on-diagonal lists (1,000 random): largest |C(x) - x| over the grid {}",
        format_figure(identity_error)
    );
    let almost = CurvePoints::new(&[[0.0, 0.0], [0.5, 0.5], [1.0, 1.0f64.next_down()]]);
    let mut pixel_round_trip = 0.0_f64;
    for grey in grey_ramp() {
        let out = curve_pixel(&almost, [grey; 3])[0];
        if (0.0..=1.0).contains(&srgb::encode_extended(grey)) && grey != 0.0 {
            pixel_round_trip = pixel_round_trip.max((out - grey).abs() / grey.abs());
        }
    }
    println!(
        "a curve one ulp below the identity at white, through the pixel rule on the fixture's grey \
         ramp inside [0, 1]: largest relative change {}",
        format_figure(pixel_round_trip)
    );
    println!(
        "\n== The backward band: the largest change of a grey pixel from a 2^-20 relative error \
         in its encoded input, over 20,001 encoded inputs in [0, 1] plus 1,025 per segment, as a \
         multiple of the forward tolerance 1e-5 + 1e-5 |reference|"
    );
    for (name, points) in named_sets() {
        let reference = CurvePoints::new(&points);
        let mut encoded: Vec<f64> = (0..=20_000).map(|k| f64::from(k) / 20_000.0).collect();
        for segment in 0..points.len() - 1 {
            encoded.extend(segment_samples(&points, segment, 1024));
        }
        let (mut largest, mut largest_multiple) = (0.0_f64, 0.0_f64);
        for x in encoded {
            let grey = srgb::decode_encoded(x);
            let exact = curve_pixel(&reference, [grey; 3])[0];
            for relative in [-(2f64.powi(-20)), 2f64.powi(-20)] {
                let perturbed = curve_pixel_with_input_error(&reference, [grey; 3], relative)[0];
                let change = (perturbed - exact).abs();
                largest = largest.max(change);
                largest_multiple = largest_multiple.max(change / (1e-5 + 1e-5 * exact.abs()));
            }
        }
        println!(
            "{name}: peak_slope {}, largest change {}, {} forward tolerances",
            format_figure(reference.peak_slope()),
            format_figure(largest),
            format_figure(largest_multiple)
        );
    }
}

// ---------------------------------------------------------------------------
// Visual review: writes PNGs to a temp directory for manual inspection. Ignored by default; no
// image is committed.
// ---------------------------------------------------------------------------

#[test]
#[ignore = "writes PNGs to a temp dir for manual visual review; not part of cargo xtask check"]
fn visual_review_writes_curve_sweeps_to_a_temp_dir() {
    use image::{RgbImage, open};

    let out_dir = std::env::temp_dir().join("luxforge-curve-visual-review");
    std::fs::create_dir_all(&out_dir).expect("create temp dir");

    let source =
        open(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg"))
            .expect("open orientation-1.jpg")
            .to_rgb8();

    for (name, points) in named_sets() {
        let reference = CurvePoints::new(&points);
        let render = |rgb: [f64; 3]| image::Rgb(curve_pixel(&reference, rgb).map(srgb::code));

        // A 512-pixel 8-bit grey ramp (64 rows) over a saturated orange ramp in linear light (32
        // rows), so tone placement, banding and the shadows' colour are visible in one strip.
        let (width, height) = (512u32, 96u32);
        let mut ramp = RgbImage::new(width, height);
        for (x, y, pixel) in ramp.enumerate_pixels_mut() {
            let rgb = if y < 64 {
                let code = (f64::from(x) * 255.0 / 511.0).round() as u8;
                [srgb::decode(code); 3]
            } else {
                let t = f64::from(x) / f64::from(width - 1);
                [t, 0.4 * t, 0.08 * t]
            };
            *pixel = render(rgb);
        }
        let row: Vec<u8> = (0..width).map(|x| ramp.get_pixel(x, 0)[0]).collect();
        println!(
            "{name}: grey ramp {} distinct codes, longest run {}, {} decreasing steps",
            distinct_codes(&row),
            longest_run(&row),
            row.windows(2).filter(|pair| pair[1] < pair[0]).count()
        );
        ramp.save(out_dir.join(format!("ramp-{name}.png")))
            .expect("save ramp png");

        let mut quadrants = RgbImage::new(source.width(), source.height());
        for (input, output) in source.pixels().zip(quadrants.pixels_mut()) {
            *output = render(input.0.map(srgb::decode));
        }
        quadrants
            .save(out_dir.join(format!("quadrants-{name}.png")))
            .expect("save quadrants png");
    }

    println!("wrote visual review PNGs to {}", out_dir.display());
}
