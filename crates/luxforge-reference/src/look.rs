//! Independent f64 reference for the RAW look's units (`docs/design/raw-looks.md`, "The look").
//!
//! A look maps a RAW photo's colour after Basic, linear sRGB with scene headroom above sensor
//! white, to a display rendition, by four units in order:
//!
//! 1. **Tone**, a monotone curve `T` over encoded luminance (the sRGB transfer continued past
//!    `[0, 1]`, [`crate::srgb::encode_extended`]) with the Tone curve's floor-subtracted
//!    luminance-ratio reconstruction, so hue is kept. `T` is the Tone curve's open PCHIP
//!    ([`crate::curve`]) through at most [`MAX_KNOTS`] knots spanning `[0, x_max]`, with the
//!    look's tails: below `0` the first segment continues linearly, above `x_max` the curve holds
//!    its last value, which is at most `1`.
//! 2. **Chroma**, a uniform Oklab chroma gain: Basic's saturation ([`crate::colour`]) at
//!    `s = 100 (c - 1)`, which scales Oklab `a` and `b` by exactly `c`.
//! 3. **Path to white**: a colour whose largest channel `m` passes the knee `k` is mixed toward the
//!    grey of its own luminance `Y` until its largest channel is `k + (1 - k)(1 - e^{-(m - k)/(1 - k)})`,
//!    which approaches `1` and never reaches it: `rgb' = Y + t (rgb - Y)` with
//!    `t = max(0, (target - Y) / (m - Y))`. Luminance is kept exactly, chroma never grows, and a
//!    colour at or below the knee is unchanged. A closed form, so the GPU cost is fixed.
//! 4. **Amount**, `out = in + a (look(in) - in)` in linear light, `a = amount / 100`.
//!
//! The Standard look's knots come from one log-logistic tone map in scene-linear luminance,
//! [`StandardParams`], sampled into knots by [`standard_knots`]. Its numbers are chosen by the
//! corpus study and frozen in [`STANDARD`].
//!
//! The study, the corpus measurement and the committed fixture (`fixtures/look/look-cases.json`)
//! live in `crates/luxforge-reference/tests/studies/look.rs`.

use crate::colour::apply_saturation;
use crate::curve::{EPSILON_L, Pchip, reconstruct_over_floor};
use crate::srgb::{decode_encoded, encode_extended};
use crate::tone::luminance;

/// The most knots a look's tone curve holds.
pub const MAX_KNOTS: usize = 24;

/// Display mid-grey in linear light: the value an 18% reflectance renders to.
pub const DISPLAY_GREY: f64 = 0.18;

/// A checked look tone curve with its interpolant built once.
#[derive(Clone, Debug, PartialEq)]
pub struct LookTone {
    pchip: Pchip,
    /// `L_floor = decode(y_0)`.
    floor: f64,
}

impl LookTone {
    /// Build the curve for an admissible knot list: `2..=MAX_KNOTS` knots, every coordinate
    /// finite, the first at `x = 0` with `y` in `[0, 1]`, `x` strictly increasing, `y`
    /// non-decreasing and at most `1`.
    ///
    /// # Panics
    ///
    /// On an inadmissible list.
    pub fn new(knots: &[[f64; 2]]) -> Self {
        let n = knots.len();
        assert!(
            (2..=MAX_KNOTS).contains(&n),
            "a look's tone curve holds 2 to {MAX_KNOTS} knots, not {n}"
        );
        assert!(
            knots.iter().flatten().all(|v| v.is_finite()),
            "a knot is not finite"
        );
        assert_eq!(knots[0][0], 0.0, "the first knot is not at x = 0");
        assert!(
            (0.0..=1.0).contains(&knots[0][1]),
            "the first knot's y is outside [0, 1]"
        );
        assert!(knots[n - 1][1] <= 1.0, "the last knot's y is above 1");
        for (index, pair) in knots.windows(2).enumerate() {
            assert!(
                pair[1][0] > pair[0][0],
                "knot {}'s x does not increase",
                index + 1
            );
            assert!(pair[1][1] >= pair[0][1], "knot {}'s y decreases", index + 1);
        }
        Self {
            floor: decode_encoded(knots[0][1]),
            pchip: Pchip::new(knots),
        }
    }

    /// `x_max`, the encoded luminance where the curve reaches its last value.
    pub fn x_max(&self) -> f64 {
        self.pchip.xs[self.pchip.len() - 1]
    }

    /// The largest slope on the span.
    pub fn peak_slope(&self) -> f64 {
        self.pchip.peak_slope()
    }

    /// `T` at an encoded luminance `x`: the linear continuation of the first segment below `0`,
    /// the hold above `x_max`, the interpolant between.
    pub fn value(&self, x: f64) -> f64 {
        let pchip = &self.pchip;
        let n = pchip.len();
        if x < 0.0 {
            return pchip.ys[0] + pchip.slopes[0] * x;
        }
        if x >= pchip.xs[n - 1] {
            return pchip.ys[n - 1];
        }
        pchip.value(x)
    }

    /// The tone unit on one linear-sRGB triple, unclamped.
    pub fn pixel(&self, rgb: [f64; 3]) -> [f64; 3] {
        let l_in = luminance(rgb);
        let l_out = decode_encoded(self.value(encode_extended(l_in)));
        reconstruct_over_floor(rgb, l_in, l_out, self.floor)
    }
}

/// A look: its tone curve, chroma gain, knee and amount.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub tone: LookTone,
    /// The Oklab chroma gain `c`; `1` changes nothing.
    pub chroma: f64,
    /// The path to white's knee `k`, in `(0, 1)`.
    pub knee: f64,
    /// `0..=200`; `100` is the look.
    pub amount: f64,
}

/// The chroma unit: Basic's saturation at `s = 100 (c - 1)`, a uniform Oklab chroma gain.
pub fn chroma_pixel(rgb: [f64; 3], gain: f64) -> [f64; 3] {
    if gain == 1.0 {
        return rgb;
    }
    apply_saturation(rgb, 100.0 * (gain - 1.0))
}

/// The largest channel the path to white leaves for a colour whose largest channel is `m`.
pub fn white_target(m: f64, knee: f64) -> f64 {
    if m <= knee {
        return m;
    }
    let room = 1.0 - knee;
    knee + room * (1.0 - (-(m - knee) / room).exp())
}

/// The path-to-white unit on one linear-sRGB triple.
pub fn path_to_white_pixel(rgb: [f64; 3], knee: f64) -> [f64; 3] {
    let m = rgb[0].max(rgb[1]).max(rgb[2]);
    if m <= knee {
        return rgb;
    }
    let y = luminance(rgb);
    if m <= y {
        // An achromatic colour: nothing to mix toward.
        return rgb;
    }
    let t = ((white_target(m, knee) - y) / (m - y)).max(0.0);
    rgb.map(|channel| y + t * (channel - y))
}

/// The whole look on one linear-sRGB triple, unclamped.
pub fn look_pixel(look: &Look, rgb: [f64; 3]) -> [f64; 3] {
    if look.amount == 0.0 {
        return rgb;
    }
    let toned = look.tone.pixel(rgb);
    let coloured = chroma_pixel(toned, look.chroma);
    let white = path_to_white_pixel(coloured, look.knee);
    let a = look.amount / 100.0;
    if a == 1.0 {
        return white;
    }
    [0, 1, 2].map(|c| rgb[c] + a * (white[c] - rgb[c]))
}

/// The parameters of the Standard look's tone map, a log-logistic curve in scene-linear
/// luminance normalized to reach display white at the headroom:
///
/// `f(Y) = W Y^p / (Y^p + s^p)`, `W = (Y_max^p + s^p) / Y_max^p`, `Y_max = 2^headroom_ev`,
///
/// with `s` solved so that scene grey `0.18 · 2^-lift_ev` renders to [`DISPLAY_GREY`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StandardParams {
    /// How far below display grey the development's scene grey sits, in EV.
    pub lift_ev: f64,
    /// `p`, the curve's contrast exponent.
    pub contrast: f64,
    /// Where the curve reaches white, in EV above sensor white.
    pub headroom_ev: f64,
    /// The Oklab chroma gain.
    pub chroma: f64,
    /// The path to white's knee.
    pub knee: f64,
}

/// The Standard look, frozen from the corpus study (`docs/design/raw-looks.md`, "The Standard
/// look").
pub const STANDARD: StandardParams = StandardParams {
    lift_ev: 1.15,
    contrast: 1.6,
    headroom_ev: 1.5,
    chroma: 1.2,
    knee: 0.8,
};

/// The stops, in EV relative to scene grey, at which [`standard_knots`] samples the tone map
/// besides `x = 0` and `x_max`: whole stops through the shadows, then half stops up to a quarter
/// stop below the headroom, at most `MAX_KNOTS - 2` of them.
fn knot_stops(params: &StandardParams) -> Vec<f64> {
    let top = (params.y_max() / params.scene_grey()).log2() - 0.25;
    let mut stops: Vec<f64> = (-9..=-4).map(f64::from).collect();
    let mut stop = -3.5;
    while stop <= top {
        stops.push(stop);
        stop += 0.5;
    }
    let keep = MAX_KNOTS - 2;
    if stops.len() > keep {
        stops.drain(..stops.len() - keep);
    }
    stops
}

impl StandardParams {
    /// `Y_max = 2^headroom_ev`.
    pub fn y_max(&self) -> f64 {
        2f64.powf(self.headroom_ev)
    }

    /// The development's scene grey: `0.18 · 2^-lift_ev`.
    pub fn scene_grey(&self) -> f64 {
        DISPLAY_GREY * 2f64.powf(-self.lift_ev)
    }

    fn map_with(&self, y: f64, s: f64) -> f64 {
        if y <= 0.0 {
            return 0.0;
        }
        let p = self.contrast;
        let y_max = self.y_max();
        let w = (y_max.powf(p) + s.powf(p)) / y_max.powf(p);
        let yp = y.powf(p);
        w * yp / (yp + s.powf(p))
    }

    /// `s`, solved by bisection in log space so that scene grey renders to display grey.
    pub fn pivot(&self) -> f64 {
        let grey = self.scene_grey();
        let (mut lo, mut hi) = (-40.0f64, 40.0f64);
        // f(grey) decreases as s grows.
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            if self.map_with(grey, mid.exp2()) > DISPLAY_GREY {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (0.5 * (lo + hi)).exp2()
    }

    /// The tone map `f`, scene-linear luminance to display-linear luminance, on `[0, Y_max]`.
    pub fn map(&self, y: f64) -> f64 {
        self.map_with(y.min(self.y_max()), self.pivot())
    }

    /// The tone map's slope at scene grey in log–log terms: `d ln f / d ln Y`.
    pub fn grey_slope(&self) -> f64 {
        let p = self.contrast;
        let s = self.pivot();
        let gp = self.scene_grey().powf(p);
        p * s.powf(p) / (gp + s.powf(p))
    }
}

/// The Standard look's knots: `(0, 0)`, the tone map sampled in encoded luminance at scene grey
/// times `2^stop` for each of [`knot_stops`], and `(encode(Y_max), 1)`.
pub fn standard_knots(params: &StandardParams) -> Vec<[f64; 2]> {
    let grey = params.scene_grey();
    let y_max = params.y_max();
    let s = params.pivot();
    let mut knots = vec![[0.0, 0.0]];
    for stop in knot_stops(params) {
        let y = grey * stop.exp2();
        knots.push([encode_extended(y), encode_extended(params.map_with(y, s))]);
    }
    knots.push([encode_extended(y_max), 1.0]);
    knots
}

/// The Standard look at `amount`.
pub fn standard_look(params: &StandardParams, amount: f64) -> Look {
    Look {
        tone: LookTone::new(&standard_knots(params)),
        chroma: params.chroma,
        knee: params.knee,
        amount,
    }
}

/// Near-black threshold of the reconstruction, re-exported for the study.
pub const NEAR_BLACK: f64 = EPSILON_L;

/// The measuring instrument of the look study: the monotone quantile map between two luminance
/// samples (`source`, a development's, and `target`, a camera preview's), at each of `quantiles`
/// in `(0, 1)`. Each input is sorted; the answer is `(source quantile, target quantile)` pairs.
pub fn quantile_pairs(source: &mut [f64], target: &mut [f64], quantiles: &[f64]) -> Vec<[f64; 2]> {
    source.sort_by(f64::total_cmp);
    target.sort_by(f64::total_cmp);
    let at = |values: &[f64], q: f64| {
        let position = q * (values.len() - 1) as f64;
        let low = position.floor() as usize;
        let high = (low + 1).min(values.len() - 1);
        let t = position - low as f64;
        values[low] + t * (values[high] - values[low])
    };
    quantiles
        .iter()
        .map(|&q| [at(source, q), at(target, q)])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_tone_map_puts_scene_grey_on_display_grey_and_white_at_the_headroom() {
        let params = STANDARD;
        assert!((params.map(params.scene_grey()) - DISPLAY_GREY).abs() < 1e-12);
        assert!((params.map(params.y_max()) - 1.0).abs() < 1e-12);
        assert_eq!(params.map(0.0), 0.0);
    }

    #[test]
    fn the_standard_knots_are_admissible() {
        let knots = standard_knots(&STANDARD);
        assert!(knots.len() <= MAX_KNOTS);
        LookTone::new(&knots);
    }

    #[test]
    fn the_tails_continue_below_zero_and_hold_above_x_max() {
        let tone = LookTone::new(&[[0.0, 0.0], [0.5, 0.6], [1.2, 1.0]]);
        assert!(tone.value(-0.1) < 0.0);
        assert_eq!(tone.value(1.2), 1.0);
        assert_eq!(tone.value(5.0), 1.0);
    }

    #[test]
    fn the_path_to_white_keeps_luminance_and_leaves_colours_below_the_knee() {
        let rgb = [0.5, 0.3, 0.1];
        assert_eq!(path_to_white_pixel(rgb, 0.8), rgb);
        let bright = [1.6, 0.4, 0.2];
        let out = path_to_white_pixel(bright, 0.8);
        assert!((luminance(out) - luminance(bright)).abs() < 1e-12);
        assert!(out[0] < 1.0);
    }
}
