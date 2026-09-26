//! Frozen f64 reference for the colour mixer's single pointwise unit.
//!
//! This mirrors the equations and constants frozen in
//! `docs/design/mixer-study.md`, which is the place every justification lives;
//! this file is the literal transcription. It is intentionally independent of
//! any production implementation: it exists to give a later implementation an
//! oracle, and the two must never share a bug by sharing code. The Oklab
//! conversion itself is *not* duplicated here — it is the one accepted in
//! `docs/design/basic-colour.md` and implemented in
//! [`super::colour`], reused unchanged so both colour studies cannot drift
//! apart.
//!
//! The property proofs and the dense measurements quoted in the study note
//! live in `crates/luxforge-reference/tests/studies/mixer.rs`, which also
//! generates and re-verifies `fixtures/mixer/mixer-cases.json`.

use super::colour::{self, Oklab};
use std::f64::consts::PI;

/// The eight hue ranges, in wheel order (ascending Oklab hue angle).
pub const RANGE_COUNT: usize = 8;

/// Range names in wheel order, matching the control order in
/// `docs/design/presence-mixer-vignette.md`.
pub const RANGE_NAMES: [&str; RANGE_COUNT] = [
    "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
];

/// The sRGB reference colour whose Oklab hue defines each range centre.
pub const RANGE_REFERENCE_CODES: [[u8; 3]; RANGE_COUNT] = [
    [255, 0, 0],
    [255, 128, 0],
    [255, 255, 0],
    [0, 255, 0],
    [0, 255, 255],
    [0, 0, 255],
    [128, 0, 255],
    [255, 0, 255],
];

/// Frozen range centres: the Oklab hue angle (degrees, `[0, 360)`) of each
/// [`RANGE_REFERENCE_CODES`] entry decoded through the reference sRGB transfer
/// function and converted by [`super::colour::to_oklab`]. `centres_match_the_reference_colours`
/// in `studies/mixer.rs` recomputes every one of these from the 8-bit codes
/// and holds it to 1e-9, so these literals are frozen *and* checked, never
/// merely asserted.
pub const CENTRE_HUES_DEG: [f64; RANGE_COUNT] = [
    29.233_885_192_3,
    52.984_679_594_0,
    109.769_232_076_5,
    142.495_338_887_8,
    194.768_947_932_0,
    264.052_020_638_1,
    293.937_640_814_5,
    328.363_417_923_5,
];

/// Chroma ramp edge: the Oklab chroma at and above which a colour is fully
/// affected by the mixer. Below it the hue rotation, the luminance amount and a
/// chroma increase fade smoothly to nothing, reaching exactly zero on the
/// achromatic axis; a chroma decrease is not ramped.
pub const CHROMA_RAMP_EDGE: f64 = 0.02;

/// Saturation gain `k_s`: `saturation_i = -100` multiplies that range's chroma
/// contribution by `1 - 1 = 0` (exact grey) and `+100` by `1 + 1 = 2` (double
/// chroma), the same `[0, 2]` gain range the Basic saturation unit uses.
pub const SATURATION_GAIN: f64 = 1.0;

/// Luminance response base: the Oklab `L` gamma exponent is
/// `LUMINANCE_GAMMA_BASE^(-m)` for the weighted slider amount `m` in `[-1, 1]`,
/// so `+100` is a square root and `-100` a square.
pub const LUMINANCE_GAMMA_BASE: f64 = 2.0;

/// The mixer's twenty-four parameters: hue, saturation and luminance per hue
/// range, each in `[-100, 100]`, indexed in the wheel order of [`RANGE_NAMES`].
/// The default is the neutral payload: every slider at zero, which is the exact
/// identity in Oklab.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MixerParams {
    pub hue: [f64; RANGE_COUNT],
    pub saturation: [f64; RANGE_COUNT],
    pub luminance: [f64; RANGE_COUNT],
}

impl MixerParams {
    /// Every slider at zero.
    pub fn neutral() -> Self {
        Self::default()
    }

    /// One range's hue slider set, everything else neutral.
    pub fn with_hue(range: usize, value: f64) -> Self {
        let mut params = Self::neutral();
        params.hue[range] = value;
        params
    }

    /// One range's saturation slider set, everything else neutral.
    pub fn with_saturation(range: usize, value: f64) -> Self {
        let mut params = Self::neutral();
        params.saturation[range] = value;
        params
    }

    /// One range's luminance slider set, everything else neutral.
    pub fn with_luminance(range: usize, value: f64) -> Self {
        let mut params = Self::neutral();
        params.luminance[range] = value;
        params
    }

    /// True when every one of the twenty-four sliders is exactly zero.
    pub fn is_neutral(&self) -> bool {
        self.hue.iter().all(|v| *v == 0.0)
            && self.saturation.iter().all(|v| *v == 0.0)
            && self.luminance.iter().all(|v| *v == 0.0)
    }
}

/// Wrap an angle in degrees to `[0, 360)`.
pub fn normalize_hue_deg(hue_deg: f64) -> f64 {
    let wrapped = hue_deg % 360.0;
    if wrapped < 0.0 {
        wrapped + 360.0
    } else {
        wrapped
    }
}

/// The angular gap, in degrees, from centre `range` to the next centre
/// counter-clockwise (increasing hue). The eight gaps sum to 360 degrees.
pub fn hue_gap_deg(range: usize) -> f64 {
    let next = CENTRE_HUES_DEG[(range + 1) % RANGE_COUNT];
    normalize_hue_deg(next - CENTRE_HUES_DEG[range])
}

/// Hue reach `KAPPA`: at `±100` a range's hue slider moves that range's
/// centre colour this fraction of the way to the neighbouring centre in the
/// direction of travel. It is also the most two neighbouring knots may close
/// the gap between them together, so every gap keeps at least `1 - KAPPA` of
/// its width (see [`knot_displacements`]).
pub const HUE_REACH: f64 = 0.85;

/// The gap a range's hue displacement is measured in: the gap to the next
/// centre when travelling positive (increasing hue), the gap to the previous
/// centre when travelling negative.
pub fn travel_gap_deg(range: usize, positive: bool) -> f64 {
    if positive {
        hue_gap_deg(range)
    } else {
        hue_gap_deg((range + RANGE_COUNT - 1) % RANGE_COUNT)
    }
}

/// The displacement, in degrees, a range's hue slider gives its own centre at
/// `±100` when no neighbour limits it: [`HUE_REACH`] of the gap in the
/// direction of travel.
pub fn full_travel_deg(range: usize, positive: bool) -> f64 {
    HUE_REACH * travel_gap_deg(range, positive)
}

/// The signed displacement of each range centre (a knot of the hue warp), in
/// degrees, after the limiting rule.
///
/// Unlimited, knot `i` moves `(hue_i / 100) * HUE_REACH * gap` in its direction
/// of travel. Two neighbouring knots driven at each other (knot `i` positive,
/// knot `i + 1` negative) would together close the gap `g_i` between them by
/// `approach = delta_i - delta_{i+1}`; where that exceeds `HUE_REACH * g_i`
/// (what one slider at full strength does alone), both are scaled by
/// `HUE_REACH * g_i / approach`, so every gap keeps at least
/// `(1 - HUE_REACH) * g_i` and the knots stay strictly increasing. Because
/// both displacements are measured in that same gap, the rule is equivalently
/// "two opposing neighbours whose magnitudes sum past 100 are both scaled by
/// `100 / (|hue_i| + |hue_{i+1}|)`". Only an opposing pair can exceed the
/// limit, and a knot has one sign, so it belongs to at most one such pair: the
/// scaling is order-independent and needs no iteration.
pub fn knot_displacements(params: &MixerParams) -> [f64; RANGE_COUNT] {
    let raw: [f64; RANGE_COUNT] = std::array::from_fn(|range| {
        let amount = params.hue[range] / 100.0;
        amount * full_travel_deg(range, amount >= 0.0)
    });
    let mut scale = [1.0_f64; RANGE_COUNT];
    for range in 0..RANGE_COUNT {
        let next = (range + 1) % RANGE_COUNT;
        let approach = raw[range] - raw[next];
        let limit = HUE_REACH * hue_gap_deg(range);
        if approach > limit {
            let shrink = limit / approach;
            scale[range] = scale[range].min(shrink);
            scale[next] = scale[next].min(shrink);
        }
    }
    std::array::from_fn(|range| raw[range] * scale[range])
}

/// The hue warp `H`: a periodic, strictly increasing, C1 map of the hue circle
/// onto itself, interpolating the eight knots `(c_i, c_i + delta_i)` with a
/// monotone piecewise-cubic Hermite interpolant (PCHIP: Fritsch-Carlson
/// monotone slopes by the Fritsch-Butland weighted harmonic mean).
///
/// It is held in displacement form, `D(h) = H(h) - h`, which is itself
/// periodic: the knot values are the displacements `delta_i` and the knot
/// slopes are `d_i - 1`, so a knot whose displacement and slope offset are both
/// exactly zero contributes exactly nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HueWarp {
    /// `delta_i`: the displacement of centre `i`, degrees.
    pub displacement: [f64; RANGE_COUNT],
    /// `d_i`: the slope `dH/dh` at centre `i`, dimensionless, always in
    /// `(0, 3 * min(m_{i-1}, m_i))`, strictly.
    pub slope: [f64; RANGE_COUNT],
}

impl HueWarp {
    pub fn new(params: &MixerParams) -> Self {
        let displacement = knot_displacements(params);
        let secant: [f64; RANGE_COUNT] =
            std::array::from_fn(|range| segment_secant(&displacement, range));
        let slope = std::array::from_fn(|range| {
            let before = (range + RANGE_COUNT - 1) % RANGE_COUNT;
            let (h_before, h_after) = (hue_gap_deg(before), hue_gap_deg(range));
            let (m_before, m_after) = (secant[before], secant[range]);
            // Every secant is at least 1 - HUE_REACH > 0 after the limiting
            // rule, so the PCHIP sign test (a zero slope where the secants
            // change sign) never fires; the weighted harmonic mean is taken
            // unconditionally.
            let w_before = 2.0 * h_after + h_before;
            let w_after = h_after + 2.0 * h_before;
            (w_before + w_after) / (w_before / m_before + w_after / m_after)
        });
        Self {
            displacement,
            slope,
        }
    }

    /// `D(h) = H(h) - h` in degrees for an input hue, on the segment the hue
    /// falls in, from the cubic Hermite basis in the fraction `t` across it.
    pub fn displacement_deg(&self, hue_deg: f64) -> f64 {
        let (lower, t) = segment(hue_deg);
        let upper = (lower + 1) % RANGE_COUNT;
        let gap = hue_gap_deg(lower);
        let (t2, t3) = (t * t, t * t * t);
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        self.displacement[lower] * h00
            + gap * (self.slope[lower] - 1.0) * h10
            + self.displacement[upper] * h01
            + gap * (self.slope[upper] - 1.0) * h11
    }

    /// `dH/dh` on segment `range` at the fraction `t` across it.
    pub fn slope_at(&self, range: usize, t: f64) -> f64 {
        let next = (range + 1) % RANGE_COUNT;
        let gap = hue_gap_deg(range);
        let rise = (self.displacement[next] - self.displacement[range]) / gap;
        let (s0, s1) = (self.slope[range] - 1.0, self.slope[next] - 1.0);
        1.0 + rise * (6.0 * t - 6.0 * t * t)
            + s0 * (3.0 * t * t - 4.0 * t + 1.0)
            + s1 * (3.0 * t * t - 2.0 * t)
    }

    /// The exact minimum of `dH/dh` over segment `range`: the slope is a
    /// quadratic in `t`, so the minimum is at an end or at its vertex.
    pub fn minimum_slope_on(&self, range: usize) -> f64 {
        let next = (range + 1) % RANGE_COUNT;
        let rise = (self.displacement[next] - self.displacement[range]) / hue_gap_deg(range);
        let (s0, s1) = (self.slope[range] - 1.0, self.slope[next] - 1.0);
        let a = -6.0 * rise + 3.0 * s0 + 3.0 * s1;
        let b = 6.0 * rise - 4.0 * s0 - 2.0 * s1;
        let mut minimum = self.slope_at(range, 0.0).min(self.slope_at(range, 1.0));
        if a > 0.0 {
            let vertex = -b / (2.0 * a);
            if vertex > 0.0 && vertex < 1.0 {
                minimum = minimum.min(self.slope_at(range, vertex));
            }
        }
        minimum
    }

    /// The exact minimum of `dH/dh` over the whole circle.
    pub fn minimum_slope(&self) -> f64 {
        (0..RANGE_COUNT)
            .map(|range| self.minimum_slope_on(range))
            .fold(f64::INFINITY, f64::min)
    }
}

/// The secant slope `m_i` of `H` over segment `i`: `1 + (delta_{i+1} - delta_i) / g_i`.
pub fn segment_secant(displacement: &[f64; RANGE_COUNT], range: usize) -> f64 {
    let next = (range + 1) % RANGE_COUNT;
    1.0 + (displacement[next] - displacement[range]) / hue_gap_deg(range)
}

/// The segment a hue falls in: the centre most recently passed going
/// counter-clockwise (the smallest non-negative wrapped distance), and the
/// fraction `t` of the way across that pair's gap. Choosing it by `argmin`
/// rather than by an interval test leaves no hue unassigned, whatever rounding
/// does at a segment edge.
pub fn segment(hue_deg: f64) -> (usize, f64) {
    let hue = normalize_hue_deg(hue_deg);
    let mut lower = 0usize;
    let mut lower_distance = f64::INFINITY;
    for (range, centre) in CENTRE_HUES_DEG.iter().enumerate() {
        let distance = normalize_hue_deg(hue - centre);
        if distance < lower_distance {
            lower_distance = distance;
            lower = range;
        }
    }
    (lower, (lower_distance / hue_gap_deg(lower)).clamp(0.0, 1.0))
}

/// The eight range weights at an Oklab hue angle: a smooth partition of unity
/// that distributes the saturation and luminance sliders. The hue sliders do
/// not use it; they drive the [`HueWarp`].
///
/// Exactly two are non-zero — the centres bracketing `hue_deg` — and they are a
/// raised cosine in the fraction `t` of the way across that pair's gap:
/// `w_i = (1 + cos(pi t)) / 2` and `w_{i+1} = 1 - w_i`, so the eight weights sum
/// to exactly `1.0` in f64 (see the study note's proof) and each lies in
/// `[0, 1]`. `w_i` is exactly `1.0` at centre `i`, and both the weights and
/// their first derivatives are continuous across every centre and across the
/// 360-degree seam.
pub fn range_weights(hue_deg: f64) -> [f64; RANGE_COUNT] {
    let (lower, t) = segment(hue_deg);
    let weight = 0.5 * (1.0 + (PI * t).cos());
    let mut weights = [0.0; RANGE_COUNT];
    weights[lower] = weight;
    weights[(lower + 1) % RANGE_COUNT] = 1.0 - weight;
    weights
}

/// The chroma ramp `w_c(C)`: exactly `0` on the achromatic axis, rising
/// smoothly (smoothstep) to exactly `1` at and above [`CHROMA_RAMP_EDGE`].
/// It scales the hue rotation, the luminance amount and a chroma *increase*,
/// so no slider can rotate, relight or colour a grey or near-grey pixel; a
/// chroma decrease is not ramped, so desaturation reaches near-greys too.
pub fn chroma_ramp(chroma: f64) -> f64 {
    let t = (chroma / CHROMA_RAMP_EDGE).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The luminance response applied to Oklab `L` for a weighted slider amount
/// `m` in `[-1, 1]`:
///
/// ```text
/// gamma = LUMINANCE_GAMMA_BASE^(-m)          // in [0.5, 2]
/// core  = clamp(L, 0, 1)
/// L_out = core^gamma + (L - core)
/// ```
///
/// It is the identity at `m = 0` (`gamma = 1`), fixes `L = 0` and `L = 1`
/// exactly, keeps every in-gamut `L` inside `[0, 1]`, never reaches `0` for a
/// non-black input, is strictly increasing in `m` for `L` in `(0, 1)` and
/// strictly increasing in `L`. Outside `[0, 1]` the excess passes through
/// unchanged, so an out-of-gamut `L` is preserved rather than folded back.
pub fn luminance_response(l: f64, m: f64) -> f64 {
    let gamma = LUMINANCE_GAMMA_BASE.powf(-m);
    let core = l.clamp(0.0, 1.0);
    core.powf(gamma) + (l - core)
}

/// The three weighted slider amounts a pixel sees, computed once from the
/// **input** pixel: the hue rotation in degrees, the chroma factor and the
/// luminance amount `m`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Amounts {
    pub rotation_deg: f64,
    pub chroma_factor: f64,
    pub luminance_amount: f64,
}

/// The chroma factor for the weighted saturation sum `s` in `[-1, 1]` and the
/// chroma ramp `w_c`: a decrease is applied in full, `1 + s`, and only an
/// increase is ramped, `1 + w_c * s`. Continuous at `s = 0`, never negative in
/// exact arithmetic; the clamp is the frozen guard against a rounded sum
/// landing a hair below `-1`.
pub fn chroma_factor(saturation_sum: f64, ramp: f64) -> f64 {
    let factor = if saturation_sum < 0.0 {
        1.0 + saturation_sum
    } else {
        1.0 + ramp * saturation_sum
    };
    factor.max(0.0)
}

/// Evaluate the three weighted amounts for an Oklab colour.
pub fn amounts(lab: Oklab, params: &MixerParams) -> Amounts {
    let chroma = colour::chroma(lab);
    let hue = colour::hue_degrees(lab);
    let weights = range_weights(hue);
    let ramp = chroma_ramp(chroma);

    let mut saturation = 0.0;
    let mut luminance = 0.0;
    for (range, weight) in weights.iter().enumerate() {
        saturation += weight * (params.saturation[range] / 100.0) * SATURATION_GAIN;
        luminance += weight * (params.luminance[range] / 100.0);
    }

    Amounts {
        rotation_deg: ramp * HueWarp::new(params).displacement_deg(hue),
        chroma_factor: chroma_factor(saturation, ramp),
        luminance_amount: ramp * luminance,
    }
}

/// The unit's reconstruction from Oklab: [`colour::from_oklab`], except that
/// an achromatic colour (`a = b = 0`, including negative zeros) reconstructs to
/// `L^3` in all three channels, which is what the exact matrices give it (the
/// first column of `M2^-1` is exactly one and each row of `M1^-1` sums to one).
/// The rounded published `M1^-1` rows do not sum to bit-identical values, so
/// the general path would give three slightly different channels, and at an
/// output code threshold that is a visible split of one code.
pub fn reconstruct(lab: Oklab) -> [f64; 3] {
    if lab.a == 0.0 && lab.b == 0.0 {
        let grey = lab.l * lab.l * lab.l;
        [grey, grey, grey]
    } else {
        colour::from_oklab(lab)
    }
}

/// The mixer unit: linear sRGB in, linear sRGB out, unclamped.
///
/// Hue first, then chroma, then luminance, every one of them driven by
/// amounts evaluated once on the **input** pixel. The hue rotation and the
/// chroma scaling are applied to the Oklab `(a, b)` vector directly — rotate,
/// then scale — rather than by recomposing `C` and `h`, exactly as
/// `docs/design/basic-colour.md` scales `a` and `b` rather than re-deriving
/// chroma: it realizes the same `(C, h)` equations and makes the neutral
/// payload the *exact* identity in Oklab.
pub fn mix(rgb_linear: [f64; 3], params: &MixerParams) -> [f64; 3] {
    let lab = colour::to_oklab(rgb_linear);
    let amounts = amounts(lab, params);

    let radians = amounts.rotation_deg.to_radians();
    let (sin, cos) = (radians.sin(), radians.cos());
    let a = amounts.chroma_factor * (lab.a * cos - lab.b * sin);
    let b = amounts.chroma_factor * (lab.a * sin + lab.b * cos);
    let l = luminance_response(lab.l, amounts.luminance_amount);

    reconstruct(Oklab { l, a, b })
}
