//! The Tone curve's interpolant and its one pointwise unit.
//!
//! This is the production transcription of `docs/design/tone-curve.md` ("The curve `C`",
//! "Processing contract" and "Production evaluation"), checked against the independent `f64`
//! reference at `crates/luxforge-reference/src/curve.rs` through the frozen fixture
//! `fixtures/curve/curve-cases.json`. The design is the only place the justifications live.
//!
//! [`Interpolant`] builds the open monotone piecewise-cubic Hermite interpolant (PCHIP:
//! Fritsch–Carlson monotonicity with Fritsch–Butland weighted-harmonic-mean knot slopes and secant
//! end slopes) in `f64`, and evaluates it in `f64` for the sample query. [`ToneCurve`] casts its
//! knots, inverse widths and per-segment `t`-form coefficients to `f32` once; the per-pixel path is
//! `f32` throughout, ignores the row coordinates and never computes a width or its inverse in `f32`,
//! so knots closer than an `f32` step collapse to one value without a division by zero.
//!
//! The sRGB transfer, the luminance weights, the near-black threshold and the reconstruction are
//! the shared ones in [`crate::colour`], never restated here.
use crate::{
    colour::{
        luma,
        srgb::{self, decode_f32, encode_f32},
    },
    modules::PointwiseColor,
    render::gpu::{GpuDescription, GpuProgram, GpuProgramKind},
};
use std::sync::Arc;

/// The Tone curve unit's GPU program (`unit.wgsl`): the knot count, the curve's ends and black
/// level as words, and the interpolant's knots, inverse widths and coefficients as a storage block.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_curve_tone_curve",
    source: include_str!("unit.wgsl"),
    kind: GpuProgramKind::Colour,
    words: 4,
    enabled: false,
};

/// The curve's interpolant over its checked points, built once in `f64`: the knots, the segment
/// widths and the knot slopes.
#[derive(Clone, Debug)]
pub(super) struct Interpolant {
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// `h_i = x_{i+1} - x_i`, strictly positive for a checked list (subnormal at the smallest).
    h: Vec<f64>,
    /// `d_i`, the knot slopes; `0` where a slope belongs only to linear segments and is never
    /// evaluated.
    d: Vec<f64>,
    /// The exact identity map, for which [`Interpolant::value`] returns its input exactly.
    identity: bool,
}

impl Interpolant {
    /// The interpolant through `points`, which the host's `curve` kind has checked: `2..=16`
    /// points, every coordinate in `[0, 1]`, `x` strictly increasing and `y` non-decreasing.
    pub(super) fn new(points: &[[f64; 2]]) -> Self {
        let n = points.len();
        assert!(n >= 2, "a checked curve holds at least two points");
        let xs: Vec<f64> = points.iter().map(|point| point[0]).collect();
        let ys: Vec<f64> = points.iter().map(|point| point[1]).collect();
        let h: Vec<f64> = xs.windows(2).map(|pair| pair[1] - pair[0]).collect();
        // Δ_i; +∞ only where a subnormal width overflows the division, which makes the segment
        // linear.
        let secants: Vec<f64> = (0..n - 1).map(|i| (ys[i + 1] - ys[i]) / h[i]).collect();
        let finite_or_zero = |value: f64| if value.is_finite() { value } else { 0.0 };
        let mut d = vec![0.0; n];
        d[0] = finite_or_zero(secants[0]);
        d[n - 1] = finite_or_zero(secants[n - 2]);
        for i in 1..n - 1 {
            let (before, after) = (secants[i - 1], secants[i]);
            d[i] = if before == 0.0 || after == 0.0 {
                0.0
            } else if !before.is_finite() && !after.is_finite() {
                // Both neighbours are linear segments, which never evaluate this slope.
                0.0
            } else {
                let w1 = 2.0 * h[i] + h[i - 1];
                let w2 = h[i] + 2.0 * h[i - 1];
                // A non-finite secant enters the weighted harmonic mean as +∞: its term is 0.
                (w1 + w2) / (w1 / before + w2 / after)
            };
        }
        Self {
            identity: Self::is_identity(points),
            xs,
            ys,
            h,
            d,
        }
    }

    /// Whether `points` are the exact identity map, compared in `f64`: `(0, 0)` first, `(1, 1)`
    /// last and every point on the diagonal, so `-0.0` equals `0.0`.
    pub(super) fn is_identity(points: &[[f64; 2]]) -> bool {
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return false;
        };
        *first == [0.0, 0.0] && *last == [1.0, 1.0] && points.iter().all(|p| p[0] == p[1])
    }

    fn len(&self) -> usize {
        self.xs.len()
    }

    fn first(&self) -> f64 {
        self.ys[0]
    }

    fn last(&self) -> f64 {
        self.ys[self.len() - 1]
    }

    /// Segment `i`'s coefficients in `t`, `C = c0 + t (c1 + t (c2 + t c3))` with
    /// `t = (x - x_i) / h_i`: an exactly flat segment is the constant `y_i`, a linear one (whose
    /// secant is not finite) `y_i + Δy t`, and a cubic one the Hermite basis collected by powers.
    fn coefficients(&self, i: usize) -> [f64; 4] {
        let (y0, y1, h) = (self.ys[i], self.ys[i + 1], self.h[i]);
        let rise = y1 - y0;
        if rise == 0.0 {
            return [y0, 0.0, 0.0, 0.0];
        }
        if !(rise / h).is_finite() {
            return [y0, rise, 0.0, 0.0];
        }
        let (m0, m1) = (h * self.d[i], h * self.d[i + 1]);
        [y0, m0, 3.0 * rise - 2.0 * m0 - m1, -2.0 * rise + m0 + m1]
    }

    /// `C(x)` in `f64`, by the first rule that applies: the unit-slope tails outside `[0, 1]`, the
    /// flat holds beyond the span, and otherwise the segment whose left knot is the last at or
    /// below `x`. An exact identity point list answers `x` exactly.
    pub(super) fn value(&self, x: f64) -> f64 {
        if self.identity {
            return x;
        }
        let n = self.len();
        if x < 0.0 {
            return self.first() + x;
        }
        if x > 1.0 {
            return self.last() + (x - 1.0);
        }
        if x <= self.xs[0] {
            return self.first();
        }
        if x >= self.xs[n - 1] {
            return self.last();
        }
        let i = (self.xs.partition_point(|&k| k <= x) - 1).min(n - 2);
        let [c0, c1, c2, c3] = self.coefficients(i);
        let t = (x - self.xs[i]) / self.h[i];
        c0 + t * (c1 + t * (c2 + t * c3))
    }
}

/// `value` as `f32`, saturating at `f32::MAX` rather than rounding to infinity.
fn saturating_f32(value: f64) -> f32 {
    if value >= f64::from(f32::MAX) {
        f32::MAX
    } else {
        value as f32
    }
}

/// The Tone curve's one pointwise unit: the interpolant's knots, inverse widths and per-segment
/// coefficients cast once to `f32`, the curve's ends and its black level `L_floor = decode(y_0)`.
#[derive(Debug)]
pub(super) struct ToneCurve {
    /// The checked points in `f64`, which [`PointwiseColor::describe`] writes exactly.
    points: Vec<[f64; 2]>,
    /// `x_i` per knot.
    x32: Vec<f32>,
    /// `1 / h_i` per segment, computed in `f64` and saturating at `f32::MAX`.
    inv_h: Vec<f32>,
    /// `[c0, c1, c2, c3]` per segment.
    coefficients: Vec<[f32; 4]>,
    first: f32,
    last: f32,
    floor: f32,
}

impl ToneCurve {
    pub(super) fn new(interpolant: &Interpolant) -> Self {
        let segments = interpolant.len() - 1;
        Self {
            points: interpolant
                .xs
                .iter()
                .zip(&interpolant.ys)
                .map(|(&x, &y)| [x, y])
                .collect(),
            x32: interpolant.xs.iter().map(|&x| x as f32).collect(),
            inv_h: interpolant
                .h
                .iter()
                .map(|&h| saturating_f32(1.0 / h))
                .collect(),
            coefficients: (0..segments)
                .map(|i| interpolant.coefficients(i).map(|c| c as f32))
                .collect(),
            first: interpolant.first() as f32,
            last: interpolant.last() as f32,
            floor: srgb::decode(interpolant.first()) as f32,
        }
    }

    /// `C(x)` in `f32`, by the design's five rules: the two unit-slope tails, the two flat holds,
    /// and otherwise the later segment whose left knot is at or below `x`, with `t` clamped to
    /// `[0, 1]` so a collapsed segment keeps `C` within its knot values.
    #[inline]
    fn curve(&self, x: f32) -> f32 {
        let n = self.x32.len();
        if x < 0.0 {
            return self.first + x;
        }
        if x > 1.0 {
            return self.last + (x - 1.0);
        }
        if x <= self.x32[0] {
            return self.first;
        }
        if x >= self.x32[n - 1] {
            return self.last;
        }
        let i = (self.x32.partition_point(|&k| k <= x) - 1).min(n - 2);
        let t = ((x - self.x32[i]) * self.inv_h[i]).clamp(0.0, 1.0);
        let [c0, c1, c2, c3] = self.coefficients[i];
        c0 + t * (c1 + t * (c2 + t * c3))
    }
}

impl PointwiseColor for ToneCurve {
    /// Encoded luminance through the curve, reconstructed over the curve's black level; nothing is
    /// clamped, and the row coordinates are ignored.
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            let l_in = luma::rec709(*pixel);
            let l_out = decode_f32(self.curve(encode_f32(l_in)));
            *pixel = luma::reconstruct_over_floor(*pixel, l_in, l_out, self.floor);
        }
    }

    fn is_finite(&self) -> bool {
        self.x32
            .iter()
            .chain(&self.inv_h)
            .chain(self.coefficients.iter().flatten())
            .chain([&self.first, &self.last, &self.floor])
            .all(|value| value.is_finite())
    }

    /// The effect and its `f64` points in the shortest round-trip form: every coefficient is a pure
    /// function of them.
    fn describe(&self) -> String {
        let points: Vec<String> = self
            .points
            .iter()
            .map(|[x, y]| format!("[{x}, {y}]"))
            .collect();
        format!("{}({})", super::CURVE_EFFECT, points.join(", "))
    }

    /// The knot count, the curve's ends and its black level as words, and the knots, inverse
    /// widths and per-segment coefficients as the block: the `f32` values `apply_row` reads, all
    /// pure functions of the points the description writes. The block is built here, `O(points)`,
    /// so a compile that never plans for the GPU builds nothing.
    fn gpu(&self) -> Option<GpuDescription> {
        let block: Vec<u32> = self
            .x32
            .iter()
            .chain(&self.inv_h)
            .chain(self.coefficients.iter().flatten())
            .map(|value| value.to_bits())
            .collect();
        Some(
            GpuDescription::new(
                &PROGRAM,
                vec![
                    self.x32.len() as u32,
                    self.first.to_bits(),
                    self.last.to_bits(),
                    self.floor.to_bits(),
                ],
            )
            .with_block(Arc::from(block)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_reference::curve::{self as reference, CurvePoints};
    use serde_json::Value;
    use std::{fs, path::PathBuf};

    /// The frozen tolerance's slope bound: a set steeper than this takes the backward tolerance.
    const FORWARD_SLOPE: f64 = 64.0;
    /// The backward tolerance's relative error in the encoded input.
    const INPUT_ERROR: f64 = 1.0 / 1_048_576.0;

    fn unit(points: &[[f64; 2]]) -> ToneCurve {
        ToneCurve::new(&Interpolant::new(points))
    }

    fn apply(unit: &ToneCurve, rgb: [f32; 3]) -> [f32; 3] {
        let mut row = [rgb];
        unit.apply_row(0, 0, &mut row);
        row[0]
    }

    fn tolerance(reference: f64) -> f64 {
        1e-5 + 1e-5 * reference.abs()
    }

    fn fixture_file() -> Value {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/curve/curve-cases.json");
        serde_json::from_str(&fs::read_to_string(&path).expect("the curve fixtures"))
            .expect("valid fixture JSON")
    }

    fn points_of(value: &Value) -> Vec<[f64; 2]> {
        value
            .as_array()
            .expect("a point list")
            .iter()
            .map(|point| {
                let pair = point.as_array().expect("a point");
                [pair[0].as_f64().unwrap(), pair[1].as_f64().unwrap()]
            })
            .collect()
    }

    fn triple(value: &Value) -> [f64; 3] {
        let values = value.as_array().expect("three values");
        std::array::from_fn(|channel| values[channel].as_f64().expect("a value"))
    }

    /// A step down counts as an inversion only beyond eight `f32` ulps of the value, the study's
    /// rule for rounding against a flat or steep curve.
    fn inverts(before: f32, after: f32) -> bool {
        after < before && before - after > 8.0 * f32::EPSILON * before.abs().max(f32::MIN_POSITIVE)
    }

    /// A dense grey ramp over `[-0.5, 2.0]` in encoded luminance, decoded to linear `f32` greys.
    fn grey_ramp() -> Vec<f32> {
        (0..=20_000)
            .map(|k| srgb::decode(-0.5 + 2.5 * f64::from(k) / 20_000.0) as f32)
            .collect()
    }

    fn assert_finite_and_monotone(name: &str, unit: &ToneCurve) {
        assert!(unit.is_finite(), "{name}: coefficients");
        let mut previous: Option<f32> = None;
        for grey in grey_ramp() {
            let out = apply(unit, [grey; 3]);
            assert!(
                out.iter().all(|c| c.is_finite()),
                "{name}: {grey} -> {out:?}"
            );
            assert!(
                out[0] == out[1] && out[1] == out[2],
                "{name}: grey {grey} -> {out:?}"
            );
            if let Some(before) = previous {
                assert!(
                    !inverts(before, out[0]),
                    "{name}: {grey} maps to {} below the previous grey's {before}",
                    out[0]
                );
            }
            previous = Some(out[0]);
        }
    }

    /// Production against every one of the 1,204 frozen cases, directly: within the forward
    /// tolerance for a set whose peak slope is at most 64, and otherwise within the reference's
    /// range under a `2^-20` relative error in the encoded input, widened by the forward
    /// tolerance. The greatest deviation of each class is printed for the design's record: forward
    /// as the absolute linear deviation, backward as the distance outside the reference band
    /// (zero inside it) and as the absolute deviation from the unperturbed reference.
    #[test]
    fn production_matches_the_frozen_curve_fixture_within_tolerance() {
        let file = fixture_file();
        let sets = file["point_sets"].as_object().expect("the point sets");
        let cases = file["cases"].as_array().expect("the cases");
        assert_eq!(sets.len(), 14, "every named set is checked");
        assert_eq!(cases.len(), 1204, "every frozen case is checked");

        let mut forward = (0.0f64, 0.0f64, String::new());
        let mut backward = (0.0f64, 0.0f64, String::new());
        let mut backward_outside = 0.0f64;
        for case in cases {
            let name = case["name"].as_str().expect("a case name");
            let set_name = case["set"].as_str().expect("a set name");
            let set = &sets[set_name];
            let points = points_of(&set["points"]);
            // `null` is a peak slope that is not finite (`subnormal`): steep.
            let steep = set["peak_slope"]
                .as_f64()
                .is_none_or(|slope| slope > FORWARD_SLOPE);
            let input = triple(&case["input_linear_rgb"]);
            let expected = triple(&case["expected_linear_rgb"]);
            let rgb = input.map(|channel| channel as f32);
            assert_eq!(
                rgb.map(f64::from),
                input,
                "{name}: the input is f32-representable"
            );
            let produced = if Interpolant::is_identity(&points) {
                // The exact identity compiles to no unit, so the pixel is untouched.
                rgb
            } else {
                apply(&unit(&points), rgb)
            };
            let bands = steep.then(|| {
                let reference = CurvePoints::new(&points);
                [
                    reference::curve_pixel_with_input_error(&reference, input, -INPUT_ERROR),
                    reference::curve_pixel_with_input_error(&reference, input, INPUT_ERROR),
                ]
            });
            for channel in 0..3 {
                let reference = expected[channel];
                let value = f64::from(produced[channel]);
                let deviation = (value - reference).abs();
                match bands {
                    None => {
                        assert!(
                            deviation <= tolerance(reference),
                            "{name} channel {channel}: production {value} against {reference}, \
                             deviation {deviation} over {}",
                            tolerance(reference)
                        );
                        if deviation > forward.0 {
                            forward = (
                                deviation,
                                deviation / tolerance(reference),
                                format!("{name} channel {channel}"),
                            );
                        }
                    }
                    Some([below, above]) => {
                        let low = reference.min(below[channel]).min(above[channel]);
                        let high = reference.max(below[channel]).max(above[channel]);
                        let (low, high) = (low - tolerance(low), high + tolerance(high));
                        assert!(
                            (low..=high).contains(&value),
                            "{name} channel {channel}: production {value} outside the backward \
                             band [{low}, {high}] around {reference}"
                        );
                        let outside = (low - value).max(value - high).max(0.0);
                        backward_outside = backward_outside.max(outside);
                        if deviation > backward.0 {
                            backward = (
                                deviation,
                                deviation / tolerance(reference),
                                format!("{name} channel {channel}"),
                            );
                        }
                    }
                }
            }
        }
        // Printed with --nocapture so the design and the handoff can quote measured figures.
        println!(
            "forward class (peak slope <= 64): largest deviation {:e} ({:.3} forward tolerances) at {}",
            forward.0, forward.1, forward.2
        );
        println!(
            "backward class (steeper): largest deviation from the unperturbed reference {:e} \
             ({:.3e} forward tolerances) at {}; largest distance outside the band {:e}",
            backward.0, backward.1, backward.2, backward_outside
        );

        // The steep sets' units also stay finite and monotone on the grey ramp.
        for (name, set) in sets {
            let points = points_of(&set["points"]);
            if !Interpolant::is_identity(&points) {
                assert_finite_and_monotone(name, &unit(&points));
            }
        }
    }

    /// Knots `4 · f64::EPSILON` and `1e-8` apart, and the drag clamp's closest pair, compile to
    /// finite coefficients and render finite and monotone; the collapsed segment stays within its
    /// knot values.
    #[test]
    fn collapsed_knots_render_finite_and_monotone() {
        for (name, points) in [
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
            (
                "collapsed-at-white",
                vec![[0.0, 0.0], [1.0 - 4.0 * f64::EPSILON, 0.0], [1.0, 1.0]],
            ),
        ] {
            let unit = unit(&points);
            assert_finite_and_monotone(name, &unit);
            assert!(
                unit.coefficients.iter().flatten().all(|c| c.abs() <= 9.0),
                "{name}"
            );
        }
        let unit = unit(&[
            [0.0, 0.0],
            [0.5, 0.4],
            [0.5 + 4.0 * f64::EPSILON, 0.6],
            [1.0, 1.0],
        ]);
        // Both collapsed knots are the one f32 value 0.5: the later segment answers there.
        assert_eq!(unit.x32[1], unit.x32[2]);
        let at = unit.curve(0.5);
        assert!((0.4..=0.6).contains(&at), "{at}");
    }

    /// A knot at `1e-310`: the first secant overflows, so the segment is linear, and every
    /// coefficient, inverse width included (saturated at `f32::MAX`), is finite.
    #[test]
    fn a_subnormal_span_compiles_to_finite_coefficients() {
        let points = [[0.0, 0.0], [1e-310, 1.0], [1.0, 1.0]];
        let interpolant = Interpolant::new(&points);
        assert!(interpolant.d.iter().all(|d| d.is_finite()));
        assert_eq!(interpolant.coefficients(0), [0.0, 1.0, 0.0, 0.0]);
        let unit = ToneCurve::new(&interpolant);
        assert!(unit.is_finite());
        assert_eq!(unit.inv_h[0], f32::MAX);
        assert_finite_and_monotone("subnormal", &unit);
        assert_eq!(unit.curve(0.0), 0.0);
        assert_eq!(unit.curve(0.25), 1.0);
        assert_eq!(interpolant.value(0.25), 1.0);
    }

    /// A curve whose first output is `0` has no lifted black, so every pixel is the frozen
    /// luminance-ratio reconstruction of the curve's output, bit for bit.
    #[test]
    fn a_black_that_is_not_lifted_reconstructs_bit_identically_to_the_frozen_rule() {
        for points in [
            vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]],
            vec![[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]],
            vec![[0.2, 0.0], [0.8, 1.0]],
        ] {
            let unit = unit(&points);
            assert_eq!(unit.floor.to_bits(), 0.0f32.to_bits());
            for rgb in [
                [0.3f32, 0.1, 0.05],
                [3e-4, 1e-5, 1e-5],
                [2e-7, -1e-7, 3e-7],
                [-0.1, 0.0, 0.0],
                [1.5, 1.2, 0.9],
                [0.18, 0.18, 0.18],
            ] {
                let l_in = luma::rec709(rgb);
                let l_out = decode_f32(unit.curve(encode_f32(l_in)));
                let frozen = luma::reconstruct(rgb, l_in, l_out);
                assert_eq!(
                    apply(&unit, rgb).map(f32::to_bits),
                    frozen.map(f32::to_bits),
                    "{points:?} {rgb:?}"
                );
            }
        }
    }

    /// Exactly on the diagonal is no unit; one `f64` ulp off it is a curve, and compiles to one.
    #[test]
    fn a_curve_one_ulp_off_the_diagonal_compiles_to_one_unit() {
        use crate::modules::{Processing, ToolModule};
        use serde_json::json;
        let module = super::super::CurveModule::new();
        let stage = crate::CompileStage::exact(crate::modules::Stage {
            width: 4,
            height: 4,
        });
        let units = |payload: Value| match module
            .compile(super::super::CURVE_EFFECT, 1, &payload, stage)
            .unwrap()
        {
            Processing::Color(operation) => operation.len(),
            other => panic!("a colour operation, not {other:?}"),
        };
        let half_up = f64::from_bits(0.5f64.to_bits() + 1);
        assert!(Interpolant::is_identity(&[
            [0.0, 0.0],
            [0.5, 0.5],
            [1.0, 1.0]
        ]));
        assert!(!Interpolant::is_identity(&[
            [0.0, 0.0],
            [0.5, half_up],
            [1.0, 1.0]
        ]));
        assert_eq!(units(json!({"luminance": [[0, 0], [0.5, 0.5], [1, 1]]})), 0);
        assert_eq!(units(json!({"luminance": [[-0.0, -0.0], [1, 1]]})), 0);
        assert_eq!(
            units(json!({"luminance": [[0, 0], [0.5, half_up], [1, 1]]})),
            1
        );
        assert_eq!(units(json!({"luminance": [[0, 1e-300], [1, 1]]})), 1);
    }

    /// The description names the effect and writes every coordinate exactly.
    #[test]
    fn the_unit_describes_its_points_exactly() {
        let third = 1.0 / 3.0;
        let sum = 0.1 + 0.2;
        let unit = unit(&[[0.0, 0.0], [sum, third], [1.0, 1.0]]);
        assert_eq!(
            unit.describe(),
            "luxforge.curve.tone([0, 0], [0.30000000000000004, 0.3333333333333333], [1, 1])"
        );
        let other = super::tests::unit(&[[0.0, 0.0], [0.3, third], [1.0, 1.0]]);
        assert_ne!(unit.describe(), other.describe());
    }

    /// The interpolant's `f64` value interpolates every knot, holds flat beyond the span inside
    /// `[0, 1]`, extends with unit slope outside it and is the identity exactly for an identity
    /// list.
    #[test]
    fn the_interpolant_interpolates_holds_and_extends() {
        let points = [[0.2, 0.1], [0.5, 0.6], [0.8, 0.9]];
        let curve = Interpolant::new(&points);
        for [x, y] in points {
            assert_eq!(curve.value(x), y);
        }
        assert_eq!(curve.value(0.0), 0.1);
        assert_eq!(curve.value(1.0), 0.9);
        assert_eq!(curve.value(-0.25), 0.1 - 0.25);
        assert_eq!(curve.value(1.5), 0.9 + 0.5);
        let identity = Interpolant::new(&[[0.0, 0.0], [0.3, 0.3], [1.0, 1.0]]);
        for k in 0..=256 {
            let x = f64::from(k) / 256.0;
            assert_eq!(identity.value(x), x);
        }
    }

    /// The GPU program's words are the knot count, the curve's ends and its black level, its
    /// block the knots, inverse widths and coefficients the CPU unit evaluates, and two separately
    /// built units that describe themselves identically carry identical uniforms and blocks.
    #[test]
    fn gpu_uniforms_follow_the_description() {
        let sets: Vec<Vec<[f64; 2]>> = vec![
            vec![[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]],
            vec![[0.0, 0.1], [1.0, 0.9]],
            vec![[0.1, 0.0], [0.5, 0.5], [0.500_000_01, 0.6], [0.9, 1.0]],
            (0..16)
                .map(|k| {
                    let x = f64::from(k) / 15.0;
                    [x, x * x]
                })
                .collect(),
        ];
        let build = || -> Vec<ToneCurve> { sets.iter().map(|points| unit(points)).collect() };
        let (first, second) = (build(), build());
        let units: Vec<&dyn PointwiseColor> = first
            .iter()
            .chain(&second)
            .map(|unit| unit as &dyn PointwiseColor)
            .collect();
        crate::render::gpu::testing::assert_uniforms_follow_descriptions(&units);
        for curve in &first {
            let description = curve.gpu().expect("the curve has a program");
            let n = curve.x32.len();
            assert_eq!(description.words[0] as usize, n);
            assert_eq!(f32::from_bits(description.words[3]), curve.floor);
            let block = description.block.expect("the knots are a block");
            assert_eq!(block.len(), n + (n - 1) + 4 * (n - 1));
            assert_eq!(f32::from_bits(block[n - 1]), curve.x32[n - 1]);
            assert_eq!(f32::from_bits(block[n]), curve.inv_h[0]);
            assert_eq!(f32::from_bits(block[2 * n - 1]), curve.coefficients[0][0]);
            assert_eq!(
                f32::from_bits(block[block.len() - 1]),
                curve.coefficients[n - 2][3]
            );
        }
    }
}
