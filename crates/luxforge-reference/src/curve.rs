//! Independent f64 reference for the Tone curve module's one pointwise unit.
//!
//! This is the literal transcription of the numerical contract in `docs/design/tone-curve.md`
//! ("Processing contract" and "The curve `C`"), which is where every justification lives; the two
//! must be read together. It shares no code with production. The luminance weights come from
//! [`crate::tone::luminance`] and the extended sRGB transfer from
//! [`crate::srgb::encode_extended`] and [`crate::srgb::decode_encoded`]; no transfer function is
//! restated here.
//!
//! The curve `C` maps encoded luminance to encoded luminance:
//!
//! * on the span `[x_0, x_{n-1}]`, an open monotone piecewise-cubic Hermite interpolant (PCHIP:
//!   Fritsch–Carlson monotonicity with Fritsch–Butland weighted-harmonic-mean knot slopes and
//!   secant end slopes), the open form of the construction the mixer's hue warp froze
//!   ([`crate::mixer::HueWarp`] holds the periodic form, which is not refactored here);
//! * inside `[0, 1]` beyond the span, held flat at `y_0` and `y_{n-1}`;
//! * outside `[0, 1]`, the unit-slope tails `y_0 + x` and `y_{n-1} + (x - 1)`.
//!
//! The pixel rule is the frozen luminance-ratio reconstruction applied to the floor-subtracted
//! output, with `L_floor = decode(C(0))`, so it is the frozen rule itself whenever the curve's
//! black is not lifted.
//!
//! The property proofs, the candidate comparison and the committed fixture
//! (`fixtures/curve/curve-cases.json`) live in
//! `crates/luxforge-reference/tests/studies/curve.rs`.

use crate::srgb::{decode_encoded, encode_extended};
use crate::tone::luminance;

/// Below this linear luminance the reconstruction switches from the ratio to the additive rule.
/// It is `tone.rs`'s `EPSILON_L`, the Basic tone contract's near-black threshold
/// (`docs/design/basic-tone.md`, "Luminance ratio and gamut policy"), declared again here because
/// that constant is private to the frozen module and this reference does not edit it.
const EPSILON_L: f64 = 1e-6;

/// The most points a curve may hold: the Tone curve field's `points_max`.
pub const MAX_POINTS: usize = 16;

/// A checked point list with its interpolant built once: the knots, the per-segment widths and
/// secants, the knot slopes and the black level the reconstruction subtracts.
#[derive(Clone, Debug, PartialEq)]
pub struct CurvePoints {
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// `h_i = x_{i+1} - x_i`, strictly positive.
    widths: Vec<f64>,
    /// `Δ_i = (y_{i+1} - y_i) / h_i`, non-negative; `+∞` only for a subnormal `h_i`.
    secants: Vec<f64>,
    /// `d_i`, the knot slopes; `0` where a slope belongs only to linear segments.
    slopes: Vec<f64>,
    /// `L_floor = decode(y_0)`.
    floor: f64,
    /// The exact identity map: `(0, 0)` first, `(1, 1)` last and every point on the diagonal.
    identity: bool,
}

impl CurvePoints {
    /// Build the interpolant for an admissible point list: `2..=16` points, every coordinate
    /// finite and in `[0, 1]`, `x` strictly increasing and `y` non-decreasing, exactly what the
    /// host's `curve` kind accepts for the Tone curve field.
    ///
    /// # Panics
    ///
    /// On an inadmissible list.
    pub fn new(points: &[[f64; 2]]) -> Self {
        let n = points.len();
        assert!(
            (2..=MAX_POINTS).contains(&n),
            "a tone curve holds 2 to {MAX_POINTS} points, not {n}"
        );
        for (index, point) in points.iter().enumerate() {
            assert!(
                point
                    .iter()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
                "point {index} {point:?} is outside [0, 1]"
            );
        }
        for (index, pair) in points.windows(2).enumerate() {
            assert!(
                pair[1][0] > pair[0][0],
                "point {}'s x does not increase",
                index + 1
            );
            assert!(
                pair[1][1] >= pair[0][1],
                "point {}'s y decreases",
                index + 1
            );
        }

        let xs: Vec<f64> = points.iter().map(|p| p[0]).collect();
        let ys: Vec<f64> = points.iter().map(|p| p[1]).collect();
        let widths: Vec<f64> = xs.windows(2).map(|pair| pair[1] - pair[0]).collect();
        let secants: Vec<f64> = (0..n - 1)
            .map(|i| (ys[i + 1] - ys[i]) / widths[i])
            .collect();

        let mut slopes = vec![0.0; n];
        // End slopes: the adjacent secant, or 0 when that secant is not finite (its segment is
        // linear and the slope is never evaluated).
        slopes[0] = finite_or_zero(secants[0]);
        slopes[n - 1] = finite_or_zero(secants[n - 2]);
        for i in 1..n - 1 {
            let (before, after) = (secants[i - 1], secants[i]);
            slopes[i] = if before == 0.0 || after == 0.0 {
                0.0
            } else if !before.is_finite() && !after.is_finite() {
                // No finite adjacent secant: both segments are linear, never evaluated.
                0.0
            } else {
                let (h_before, h_after) = (widths[i - 1], widths[i]);
                let w1 = 2.0 * h_after + h_before;
                let w2 = h_after + 2.0 * h_before;
                // A non-finite secant enters the harmonic mean as +∞: its term w / Δ is 0.
                (w1 + w2) / (w1 / before + w2 / after)
            };
        }

        let identity = points[0] == [0.0, 0.0]
            && points[n - 1] == [1.0, 1.0]
            && points.iter().all(|p| p[0] == p[1]);

        Self {
            floor: decode_encoded(ys[0]),
            xs,
            ys,
            widths,
            secants,
            slopes,
            identity,
        }
    }

    /// The knot slopes `d_i`, one per point.
    pub fn knot_slopes(&self) -> &[f64] {
        &self.slopes
    }

    /// The largest value of `C'` on any segment of the span, from each segment's quadratic
    /// derivative: an exactly flat segment contributes `0`, a linear (non-finite secant) segment
    /// its secant, which is `+∞`, and a cubic segment the maximum of its derivative at its ends
    /// and, where the quadratic is concave, at its vertex. The flat holds (slope `0`) and the
    /// unit-slope tails (slope `1`) are not segments of the span.
    pub fn peak_slope(&self) -> f64 {
        (0..self.widths.len())
            .map(|i| self.segment_peak_slope(i))
            .fold(0.0, f64::max)
    }

    /// `L_floor = decode(C(0)) = decode(y_0)`, the curve's black level in linear light.
    pub fn floor(&self) -> f64 {
        self.floor
    }

    fn len(&self) -> usize {
        self.xs.len()
    }

    fn is_flat(&self, segment: usize) -> bool {
        self.ys[segment] == self.ys[segment + 1]
    }

    fn is_linear(&self, segment: usize) -> bool {
        !self.secants[segment].is_finite()
    }

    /// `C'` on a cubic segment at the fraction `t` across it:
    /// `d_i (3t² − 4t + 1) + d_{i+1} (3t² − 2t) + Δ_i · 6t (1 − t)`.
    fn cubic_slope(&self, segment: usize, t: f64) -> f64 {
        let (d0, d1, secant) = (
            self.slopes[segment],
            self.slopes[segment + 1],
            self.secants[segment],
        );
        d0 * (3.0 * t * t - 4.0 * t + 1.0)
            + d1 * (3.0 * t * t - 2.0 * t)
            + secant * 6.0 * t * (1.0 - t)
    }

    fn segment_peak_slope(&self, segment: usize) -> f64 {
        if self.is_flat(segment) {
            return 0.0;
        }
        if self.is_linear(segment) {
            return self.secants[segment];
        }
        let (d0, d1, secant) = (
            self.slopes[segment],
            self.slopes[segment + 1],
            self.secants[segment],
        );
        let a = 3.0 * d0 + 3.0 * d1 - 6.0 * secant;
        let b = -4.0 * d0 - 2.0 * d1 + 6.0 * secant;
        let mut peak = d0.max(d1);
        if a < 0.0 {
            let vertex = -b / (2.0 * a);
            if vertex > 0.0 && vertex < 1.0 {
                peak = peak.max(self.cubic_slope(segment, vertex));
            }
        }
        peak
    }

    /// `C` on segment `i` at `x`, for `x_i <= x <= x_{i+1}`.
    fn segment_value(&self, segment: usize, x: f64) -> f64 {
        let (y0, y1) = (self.ys[segment], self.ys[segment + 1]);
        if y0 == y1 {
            // An exactly constant segment, by a direct rule rather than the basis.
            return y0;
        }
        let h = self.widths[segment];
        let t = (x - self.xs[segment]) / h;
        if self.is_linear(segment) {
            return y0 + (y1 - y0) * t;
        }
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        y0 * h00 + h * self.slopes[segment] * h10 + y1 * h01 + h * self.slopes[segment + 1] * h11
    }
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

/// The curve `C` at an encoded luminance `x`, by the first rule that applies: the unit-slope tail
/// below `0` and above `1`, the flat hold inside `[0, 1]` beyond the span, and otherwise the
/// segment whose left knot is the last one at or below `x`.
pub fn curve(points: &CurvePoints, x: f64) -> f64 {
    let n = points.len();
    let (first, last) = (points.ys[0], points.ys[n - 1]);
    if x < 0.0 {
        return first + x;
    }
    if x > 1.0 {
        return last + (x - 1.0);
    }
    if x <= points.xs[0] {
        return first;
    }
    if x >= points.xs[n - 1] {
        return last;
    }
    let segment = (points.xs.partition_point(|&k| k <= x) - 1).min(n - 2);
    points.segment_value(segment, x)
}

/// The Tone curve unit on one linear-sRGB triple, unclamped: the floor-subtracted
/// luminance-ratio reconstruction of `C` applied to the pixel's encoded luminance.
///
/// The exact identity map returns the input unchanged, as production compiles it to no unit.
pub fn curve_pixel(points: &CurvePoints, rgb: [f64; 3]) -> [f64; 3] {
    curve_pixel_with_input_error(points, rgb, 0.0)
}

/// [`curve_pixel`] with the encoded input scaled by `1 + relative` before the curve, the rest of
/// the pixel rule unchanged. The backward tolerance evaluates it at `±2^-20` to bound what a
/// steep segment does to production's `f32` luminance and encode error.
pub fn curve_pixel_with_input_error(
    points: &CurvePoints,
    rgb: [f64; 3],
    relative: f64,
) -> [f64; 3] {
    if points.identity {
        return rgb;
    }
    let l_in = luminance(rgb);
    let l_out = decode_encoded(curve(points, encode_extended(l_in) * (1.0 + relative)));
    reconstruct_over_floor(rgb, l_in, l_out, points.floor)
}

/// The frozen luminance-ratio reconstruction applied to the floor-subtracted output: near black
/// the additive rule `rgb + (L_out - L)`, elsewhere `L_floor + rgb · (L_out - L_floor) / L`. With
/// no lifted black (`L_floor == 0`) it is the frozen rule, `rgb · L_out / L`, bit for bit.
fn reconstruct_over_floor(rgb: [f64; 3], l_in: f64, l_out: f64, l_floor: f64) -> [f64; 3] {
    if l_in.abs() < EPSILON_L {
        let delta = l_out - l_in;
        return rgb.map(|channel| channel + delta);
    }
    if l_floor == 0.0 {
        let ratio = l_out / l_in;
        return rgb.map(|channel| channel * ratio);
    }
    let scale = (l_out - l_floor) / l_in;
    rgb.map(|channel| l_floor + channel * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_points_hold_their_ends_and_extend_with_unit_slope() {
        let points = CurvePoints::new(&[[0.2, 0.1], [0.8, 0.9]]);
        assert_eq!(curve(&points, 0.0), 0.1);
        assert_eq!(curve(&points, 0.2), 0.1);
        assert_eq!(curve(&points, 0.8), 0.9);
        assert_eq!(curve(&points, 1.0), 0.9);
        assert_eq!(curve(&points, -0.5), 0.1 - 0.5);
        assert_eq!(curve(&points, 1.5), 0.9 + 0.5);
        assert!((curve(&points, 0.5) - 0.5).abs() < 1e-15);
    }

    #[test]
    #[should_panic(expected = "does not increase")]
    fn a_repeated_x_is_refused() {
        CurvePoints::new(&[[0.0, 0.0], [0.5, 0.2], [0.5, 0.4], [1.0, 1.0]]);
    }

    #[test]
    #[should_panic(expected = "decreases")]
    fn a_decreasing_y_is_refused() {
        CurvePoints::new(&[[0.0, 0.5], [1.0, 0.4]]);
    }

    #[test]
    #[should_panic(expected = "2 to 16 points")]
    fn seventeen_points_are_refused() {
        let points: Vec<[f64; 2]> = (0..17)
            .map(|k| [k as f64 / 16.0, k as f64 / 16.0])
            .collect();
        CurvePoints::new(&points);
    }

    #[test]
    fn an_unlifted_black_is_the_frozen_ratio() {
        let points = CurvePoints::new(&[[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]]);
        assert_eq!(points.floor(), 0.0);
        let rgb = [0.3, 0.1, 0.05];
        let l_in = luminance(rgb);
        let l_out = decode_encoded(curve(&points, encode_extended(l_in)));
        let ratio = l_out / l_in;
        assert_eq!(curve_pixel(&points, rgb), rgb.map(|c| c * ratio));
    }
}
