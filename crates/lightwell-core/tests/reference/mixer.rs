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
//! live in `crates/lightwell-core/tests/mixer_reference.rs`, which also
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
/// in `mixer_reference.rs` recomputes every one of these from the 8-bit codes
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
/// affected by the mixer. Below it the three effects fade smoothly to nothing,
/// reaching exactly zero on the achromatic axis.
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

/// The bounded rotation a range's hue slider reaches at `±100`: half the gap to
/// the neighbouring centre **in the direction of travel**. `positive` selects
/// the increasing-hue neighbour.
pub fn max_rotation_deg(range: usize, positive: bool) -> f64 {
    let gap = if positive {
        hue_gap_deg(range)
    } else {
        hue_gap_deg((range + RANGE_COUNT - 1) % RANGE_COUNT)
    };
    0.5 * gap
}

/// The eight range weights at an Oklab hue angle: a smooth partition of unity.
///
/// Exactly two are non-zero — the centres bracketing `hue_deg` — and they are a
/// raised cosine in the fraction `t` of the way across that pair's gap:
/// `w_i = (1 + cos(pi t)) / 2` and `w_{i+1} = 1 - w_i`, so the eight weights sum
/// to exactly `1.0` in f64 (see the study note's proof) and each lies in
/// `[0, 1]`. `w_i` is exactly `1.0` at centre `i`, and both the weights and
/// their first derivatives are continuous across every centre and across the
/// 360-degree seam.
pub fn range_weights(hue_deg: f64) -> [f64; RANGE_COUNT] {
    let hue = normalize_hue_deg(hue_deg);
    // The containing segment is the centre most recently passed going
    // counter-clockwise: the one with the smallest non-negative wrapped
    // distance. Choosing it by `argmin` rather than by an interval test leaves
    // no hue unassigned, whatever rounding does at a segment edge.
    let mut lower = 0usize;
    let mut lower_distance = f64::INFINITY;
    for (range, centre) in CENTRE_HUES_DEG.iter().enumerate() {
        let distance = normalize_hue_deg(hue - centre);
        if distance < lower_distance {
            lower_distance = distance;
            lower = range;
        }
    }
    let t = (lower_distance / hue_gap_deg(lower)).clamp(0.0, 1.0);
    let weight = 0.5 * (1.0 + (PI * t).cos());
    let mut weights = [0.0; RANGE_COUNT];
    weights[lower] = weight;
    weights[(lower + 1) % RANGE_COUNT] = 1.0 - weight;
    weights
}

/// The chroma ramp `w_c(C)`: exactly `0` on the achromatic axis, rising
/// smoothly (smoothstep) to exactly `1` at and above [`CHROMA_RAMP_EDGE`].
/// All three effects are scaled by it, so a grey pixel is untouched by every
/// slider.
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
/// **input** pixel's weights: the hue rotation in degrees, the chroma factor
/// and the luminance amount `m`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Amounts {
    pub rotation_deg: f64,
    pub chroma_factor: f64,
    pub luminance_amount: f64,
}

/// Evaluate the three weighted amounts for an Oklab colour.
pub fn amounts(lab: Oklab, params: &MixerParams) -> Amounts {
    let chroma = colour::chroma(lab);
    let weights = range_weights(colour::hue_degrees(lab));
    let ramp = chroma_ramp(chroma);

    let mut rotation = 0.0;
    let mut saturation = 0.0;
    let mut luminance = 0.0;
    for (range, weight) in weights.iter().enumerate() {
        let hue_amount = params.hue[range] / 100.0;
        rotation += weight * hue_amount * max_rotation_deg(range, hue_amount >= 0.0);
        saturation += weight * (params.saturation[range] / 100.0) * SATURATION_GAIN;
        luminance += weight * (params.luminance[range] / 100.0);
    }

    Amounts {
        rotation_deg: ramp * rotation,
        // The factor cannot be negative in exact arithmetic (`|sum| <= 1`
        // because the weights are a partition of unity and each slider term is
        // in `[-1, 1]`); the clamp is the frozen guard against a rounded sum
        // landing a hair below `-1`.
        chroma_factor: (1.0 + ramp * saturation).max(0.0),
        luminance_amount: ramp * luminance,
    }
}

/// The mixer unit: linear sRGB in, linear sRGB out, unclamped.
///
/// Hue first, then chroma, then luminance, every one of them driven by weights
/// evaluated once on the **input** pixel. The hue rotation and the chroma
/// scaling are applied to the Oklab `(a, b)` vector directly — rotate, then
/// scale — rather than by recomposing `C` and `h`, exactly as
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

    colour::from_oklab(Oklab { l, a, b })
}
