//! The colour mixer's one pointwise unit.
//!
//! This is the production transcription of the equations frozen in
//! `docs/design/mixer-study.md`: the eight range centres, the monotone hue warp the hue sliders
//! drive, the raised-cosine weights the saturation and luminance sliders are distributed by, the
//! chroma ramp, the chroma factor and the Oklab `L` gamma response, composed hue then chroma then
//! luminance from amounts evaluated once on the **input** pixel. The study is the only place the
//! justifications live; this file restates the constants and nothing else.
//!
//! The Oklab conversion is **not** restated here: it is the one accepted in
//! `docs/design/basic-colour.md` and implemented in [`crate::modules::basic::colour`], reused
//! unchanged so the two colour modules cannot drift apart and the matrices exist once. The one
//! mixer-specific step on the way back is [`reconstruct`], which gives an achromatic result three
//! bit-identical channels.
//!
//! Coefficients are computed in f64 — the frozen centres, the eight gaps derived from them, the
//! hue warp's knots, slopes and per-segment cubics, and the sixteen saturation and luminance
//! amounts — and cast once into the small fixed-size f32 arrays the unit holds. The per-pixel path
//! is f32 throughout, ignores the row coordinates and touches nothing but the pixel it was given
//! and those arrays.
use crate::modules::{
    PointwiseColor,
    basic::colour::{self, Oklab},
};

/// The eight hue ranges, in wheel order (ascending Oklab hue angle).
pub(super) const RANGE_COUNT: usize = 8;

/// Range names in wheel order, used by the parameter names and by [`PointwiseColor::describe`].
pub(super) const RANGE_NAMES: [&str; RANGE_COUNT] = [
    "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
];

/// The sRGB reference colour whose Oklab hue defines each range centre. The control rails are
/// built from these same codes, so the slider a person drags is painted in the colour whose hue
/// defines the range it addresses.
pub(super) const RANGE_REFERENCE_CODES: [[u8; 3]; RANGE_COUNT] = [
    [255, 0, 0],
    [255, 128, 0],
    [255, 255, 0],
    [0, 255, 0],
    [0, 255, 255],
    [0, 0, 255],
    [128, 0, 255],
    [255, 0, 255],
];

/// The frozen range centres from `docs/design/mixer-study.md`: the Oklab hue angle in degrees,
/// `[0, 360)`, of each [`RANGE_REFERENCE_CODES`] entry. The study's
/// `centres_match_the_reference_colours` recomputes every one of them from the 8-bit codes and
/// holds it to 1e-9, and `centres_match_the_frozen_study_table` below holds this copy to the same
/// literals, so neither table can move without the other failing.
const CENTRE_HUES_DEG: [f64; RANGE_COUNT] = [
    29.233_885_192_3,
    52.984_679_594_0,
    109.769_232_076_5,
    142.495_338_887_8,
    194.768_947_932_0,
    264.052_020_638_1,
    293.937_640_814_5,
    328.363_417_923_5,
];

/// Chroma ramp edge `C0`: the Oklab chroma at and above which a colour is fully affected. Below it
/// the hue rotation, the luminance amount and a chroma *increase* fade smoothly to nothing and
/// reach exactly zero on the achromatic axis, which is what keeps a grey, a near-grey and shadow
/// noise from being rotated, relit or coloured by any slider. A chroma decrease is not ramped.
const CHROMA_RAMP_EDGE_F64: f64 = 0.02;

/// Saturation gain `k_s`: `-100` at a range's own centre gives the factor `0` (exact grey) and
/// `+100` gives `2` (double chroma), the `[0, 2]` gain range Basic's saturation already uses.
const SATURATION_GAIN_F64: f64 = 1.0;

/// Luminance response base: the Oklab `L` exponent is `2^(-m)` for the weighted amount `m` in
/// `[-1, 1]`, so `+100` is a square root and `-100` a square.
const LUMINANCE_GAMMA_BASE_F64: f64 = 2.0;

/// Hue reach `KAPPA`: at `±100` a range's hue slider carries its centre colour this fraction of the
/// way to the neighbouring centre in the direction of travel. It is also the most two neighbouring
/// centres may close the gap between them together, so every gap keeps at least `1 - KAPPA` of its
/// width and the hue warp stays strictly increasing under any combination of sliders.
const HUE_REACH_F64: f64 = 0.85;

const CHROMA_RAMP_EDGE: f32 = CHROMA_RAMP_EDGE_F64 as f32;
const LUMINANCE_GAMMA_BASE: f32 = LUMINANCE_GAMMA_BASE_F64 as f32;

/// Wrap a difference of two angles already in `[0, 360)` — so in `(-360, 360)` — back into
/// `[0, 360)`. One addition suffices for that input, which keeps this usable in a const context.
const fn wrap_turn(degrees: f64) -> f64 {
    if degrees < 0.0 {
        degrees + 360.0
    } else {
        degrees
    }
}

/// The eight gaps, each from one centre to the next counter-clockwise, derived from
/// [`CENTRE_HUES_DEG`] rather than frozen a second time. They sum to 360 degrees.
const fn gaps_deg() -> [f64; RANGE_COUNT] {
    let mut gaps = [0.0; RANGE_COUNT];
    let mut range = 0;
    while range < RANGE_COUNT {
        gaps[range] =
            wrap_turn(CENTRE_HUES_DEG[(range + 1) % RANGE_COUNT] - CENTRE_HUES_DEG[range]);
        range += 1;
    }
    gaps
}

const GAPS_DEG: [f64; RANGE_COUNT] = gaps_deg();

/// The displacement in degrees a range's hue slider gives its own centre at `±100` when no
/// neighbour limits it: [`HUE_REACH_F64`] of the gap to the neighbouring centre **in the direction
/// of travel**, so `+100` means the same fraction of the way to the next colour everywhere.
fn full_travel_deg(range: usize, positive: bool) -> f64 {
    let gap = if positive {
        GAPS_DEG[range]
    } else {
        GAPS_DEG[(range + RANGE_COUNT - 1) % RANGE_COUNT]
    };
    HUE_REACH_F64 * gap
}

/// The signed displacement of each centre, degrees, after the limiting rule: two neighbouring
/// centres driven at each other may together close the gap between them by at most
/// `HUE_REACH * gap`, what one slider at full strength does alone, and where they would close it
/// further both displacements are scaled by the same factor. Both are measured in that same gap, so
/// this is "two opposing neighbours whose magnitudes sum past 100 share one slider's travel". Only
/// an opposing pair can exceed the limit and a centre has one sign, so each centre belongs to at
/// most one such pair and the rule needs no iteration.
fn knot_displacements(hue: &[f64; RANGE_COUNT]) -> [f64; RANGE_COUNT] {
    let raw: [f64; RANGE_COUNT] = std::array::from_fn(|range| {
        let amount = hue[range] / 100.0;
        amount * full_travel_deg(range, amount >= 0.0)
    });
    let mut scale = [1.0f64; RANGE_COUNT];
    for range in 0..RANGE_COUNT {
        let next = (range + 1) % RANGE_COUNT;
        let approach = raw[range] - raw[next];
        let limit = HUE_REACH_F64 * GAPS_DEG[range];
        if approach > limit {
            let shrink = limit / approach;
            scale[range] = scale[range].min(shrink);
            scale[next] = scale[next].min(shrink);
        }
    }
    std::array::from_fn(|range| raw[range] * scale[range])
}

/// The hue warp's per-segment cubics, in displacement form `D(h) = H(h) - h`.
///
/// `H` is the periodic monotone piecewise-cubic Hermite interpolant (PCHIP) through the eight knots
/// `(c_i, c_i + delta_i)`: every secant `m_i = 1 + (delta_{i+1} - delta_i) / g_i` is at least
/// `1 - HUE_REACH` after the limiting rule, and each knot's slope is the Fritsch–Butland weighted
/// harmonic mean of the secants either side, which lies strictly inside `(0, 3 min(m_{i-1}, m_i))`
/// and so satisfies the Fritsch–Carlson condition for a strictly increasing cubic on every segment.
///
/// Segment `i`, at the fraction `t` across it, is `c0 + t (c1 + t (c2 + t c3))` degrees. A segment
/// whose two knots have zero displacement and slope exactly one has four exact zeros, so it moves
/// no hue at all.
fn hue_warp(hue: &[f64; RANGE_COUNT]) -> [[f32; 4]; RANGE_COUNT] {
    let displacement = knot_displacements(hue);
    let secant: [f64; RANGE_COUNT] = std::array::from_fn(|range| {
        let next = (range + 1) % RANGE_COUNT;
        1.0 + (displacement[next] - displacement[range]) / GAPS_DEG[range]
    });
    // `d_i - 1`: the knot slope of `D` rather than of `H`, exactly zero where both neighbouring
    // secants are exactly one.
    let slope_offset: [f64; RANGE_COUNT] = std::array::from_fn(|range| {
        let before = (range + RANGE_COUNT - 1) % RANGE_COUNT;
        let (gap_before, gap_after) = (GAPS_DEG[before], GAPS_DEG[range]);
        let weight_before = 2.0 * gap_after + gap_before;
        let weight_after = gap_after + 2.0 * gap_before;
        let slope = (weight_before + weight_after)
            / (weight_before / secant[before] + weight_after / secant[range]);
        slope - 1.0
    });
    std::array::from_fn(|range| {
        let next = (range + 1) % RANGE_COUNT;
        let gap = GAPS_DEG[range];
        let (d0, d1) = (displacement[range], displacement[next]);
        let (s0, s1) = (gap * slope_offset[range], gap * slope_offset[next]);
        [
            d0 as f32,
            s0 as f32,
            (3.0 * (d1 - d0) - 2.0 * s0 - s1) as f32,
            (2.0 * (d0 - d1) + s0 + s1) as f32,
        ]
    })
}

const fn as_f32(values: [f64; RANGE_COUNT]) -> [f32; RANGE_COUNT] {
    let mut out = [0.0f32; RANGE_COUNT];
    let mut range = 0;
    while range < RANGE_COUNT {
        out[range] = values[range] as f32;
        range += 1;
    }
    out
}

const CENTRES: [f32; RANGE_COUNT] = as_f32(CENTRE_HUES_DEG);
const GAPS: [f32; RANGE_COUNT] = as_f32(GAPS_DEG);

/// The chroma ramp `w_c(C)`: exactly `0` on the achromatic axis, smoothstep to exactly `1` at and
/// above [`CHROMA_RAMP_EDGE`]. It scales the rotation, the luminance amount and a chroma increase.
fn chroma_ramp(chroma: f32) -> f32 {
    let t = (chroma / CHROMA_RAMP_EDGE).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// An Oklab hue angle, which [`colour::hue_degrees`] reports in `(-180, 180]`, wrapped to
/// `[0, 360)`.
fn normalize_hue_deg(hue_deg: f32) -> f32 {
    if hue_deg < 0.0 {
        hue_deg + 360.0
    } else {
        hue_deg
    }
}

/// The segment a hue falls in: the centre most recently passed going counter-clockwise, and the
/// fraction `t` of the way across that pair's gap. The hue warp's cubic and the saturation and
/// luminance weights are both evaluated at this one `(segment, t)`.
///
/// Chosen by `argmin` over the wrapped distances rather than by an interval test, exactly as the
/// reference does, so no hue is left unassigned whatever rounding does at a segment edge: a hue a
/// hair *below* a centre lands in the previous segment at `t = 1`, where that segment's weight is
/// `0` and the whole weight goes to the centre itself.
fn segment(hue_deg: f32) -> (usize, f32) {
    let mut lower = 0usize;
    let mut lower_distance = f32::INFINITY;
    for (range, centre) in CENTRES.iter().enumerate() {
        let distance = normalize_hue_deg(hue_deg - centre);
        if distance < lower_distance {
            lower_distance = distance;
            lower = range;
        }
    }
    (lower, (lower_distance / GAPS[lower]).clamp(0.0, 1.0))
}

/// The Oklab `L` response for a weighted amount `m`: a gamma on the `[0, 1]` part of `L` with any
/// excess passed through, so `0` and `1` are exact fixed points and out-of-gamut `L` is preserved
/// rather than folded back.
fn luminance_response(l: f32, amount: f32) -> f32 {
    let gamma = LUMINANCE_GAMMA_BASE.powf(-amount);
    let core = l.clamp(0.0, 1.0);
    core.powf(gamma) + (l - core)
}

/// The unit's reconstruction from Oklab: [`colour::from_oklab`], except that an achromatic colour
/// (`a = b = 0`, either zero's sign) reconstructs to `L^3` in all three channels, which is what the
/// exact matrices give it. The rounded published `M1^-1` rows do not sum to bit-identical values in
/// f32, so the general path returns three channels a few ulps apart, and where they straddle an
/// output code threshold a grey renders with one channel a code off.
fn reconstruct(lab: Oklab) -> [f32; 3] {
    if lab.a == 0.0 && lab.b == 0.0 {
        let grey = lab.l * lab.l * lab.l;
        [grey, grey, grey]
    } else {
        colour::from_oklab(lab)
    }
}

/// The colour mixer unit: twenty-four sliders reduced to the hue warp's eight cubics and two
/// per-range f32 coefficient arrays.
///
/// The stored f64 values are kept only for [`PointwiseColor::describe`] and the finiteness check;
/// nothing per-pixel reads them. The coefficient arrays are the whole of the unit's state: 48 f32
/// values, a fixed 192 bytes, computed once when a layer compiles and never again.
#[derive(Debug)]
pub(super) struct Mixer {
    hue: [f64; RANGE_COUNT],
    saturation: [f64; RANGE_COUNT],
    luminance: [f64; RANGE_COUNT],
    /// The hue warp's displacement `D(h) = H(h) - h` on each segment, as the power-basis cubic
    /// `[c0, c1, c2, c3]` in the fraction `t` across it, degrees.
    hue_warp: [[f32; 4]; RANGE_COUNT],
    /// `(saturation_i / 100) * k_s`: this range's contribution to the saturation sum.
    chroma_gain: [f32; RANGE_COUNT],
    /// `luminance_i / 100`: this range's contribution to the luminance amount `m`.
    luminance_amount: [f32; RANGE_COUNT],
}

impl Mixer {
    /// Build the unit from the twenty-four slider values, in the wheel order of [`RANGE_NAMES`].
    pub(super) fn new(
        hue: [f64; RANGE_COUNT],
        saturation: [f64; RANGE_COUNT],
        luminance: [f64; RANGE_COUNT],
    ) -> Self {
        let chroma_gain =
            std::array::from_fn(|range| (saturation[range] / 100.0 * SATURATION_GAIN_F64) as f32);
        let luminance_amount = std::array::from_fn(|range| (luminance[range] / 100.0) as f32);
        Self {
            hue_warp: hue_warp(&hue),
            hue,
            saturation,
            luminance,
            chroma_gain,
            luminance_amount,
        }
    }

    /// The three amounts one pixel sees, evaluated once on that pixel: the rotation in degrees, the
    /// chroma factor and the luminance amount `m`.
    ///
    /// The rotation is the hue warp's cubic on the pixel's segment, scaled by the ramp. Exactly two
    /// range weights are non-zero, so the saturation and luminance sums read two entries of each
    /// coefficient array rather than summing over eight; the other six terms are `+0.0` in the
    /// frozen equations and adding `+0.0` is exact.
    fn amounts(&self, ramp: f32, hue_deg: f32) -> (f32, f32, f32) {
        let (lower, t) = segment(hue_deg);
        let upper = (lower + 1) % RANGE_COUNT;
        let [c0, c1, c2, c3] = self.hue_warp[lower];
        let rotation = ramp * (c0 + t * (c1 + t * (c2 + t * c3)));
        let weight = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
        let other = 1.0 - weight;
        let saturation = weight * self.chroma_gain[lower] + other * self.chroma_gain[upper];
        // A decrease is applied in full and only an increase is ramped: the ramp keeps a slider
        // from colouring a near-grey, not from greying it. `weight + other` is exactly one, so
        // every saturation slider at -100 makes this sum exactly -1 and the factor exactly 0.
        let factor = if saturation < 0.0 {
            1.0 + saturation
        } else {
            1.0 + ramp * saturation
        };
        let amount =
            ramp * (weight * self.luminance_amount[lower] + other * self.luminance_amount[upper]);
        // The factor cannot be negative in exact arithmetic; the clamp is the frozen guard against
        // a rounded sum landing a hair below -1.
        (rotation, factor.max(0.0), amount)
    }
}

impl PointwiseColor for Mixer {
    /// One Oklab round trip per pixel, with the row coordinates ignored: the mixer is pointwise in
    /// the strict sense.
    ///
    /// The branches are exactness, not approximation. A pixel with `a = b = 0` exactly — black,
    /// for one — has a ramp of zero, so its rotation and luminance amount are exactly zero and any
    /// chroma factor leaves `(0, 0)` where it is: the frozen equations reconstruct it as `L^3`, and
    /// the branch computes that without the hue angle. A zero rotation skips `sin_cos` where the
    /// equations multiply by `cos 0 = 1` and `sin 0 = 0`, and a zero luminance amount skips the
    /// response where they raise to the power `1`.
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            let lab = colour::to_oklab(*pixel);
            if lab.a == 0.0 && lab.b == 0.0 {
                *pixel = reconstruct(lab);
                continue;
            }
            let ramp = chroma_ramp(colour::chroma(lab));
            let (rotation, factor, amount) =
                self.amounts(ramp, normalize_hue_deg(colour::hue_degrees(lab)));
            // Hue, then chroma, applied to the `(a, b)` vector directly — rotate, then scale —
            // rather than by recomposing `C` and `h`: a rotation and a non-negative scalar commute,
            // so this realizes the frozen `(C, h)` equations exactly while leaving a zero rotation
            // and a unit factor as the exact identity in Oklab.
            let (a, b) = if rotation == 0.0 {
                (factor * lab.a, factor * lab.b)
            } else {
                let (sin, cos) = rotation.to_radians().sin_cos();
                (
                    factor * (lab.a * cos - lab.b * sin),
                    factor * (lab.a * sin + lab.b * cos),
                )
            };
            let l = if amount == 0.0 {
                lab.l
            } else {
                luminance_response(lab.l, amount)
            };
            *pixel = reconstruct(Oklab { l, a, b });
        }
    }

    fn is_finite(&self) -> bool {
        let stored = self
            .hue
            .iter()
            .chain(&self.saturation)
            .chain(&self.luminance)
            .all(|value| value.is_finite());
        let derived = self
            .hue_warp
            .iter()
            .flatten()
            .chain(&self.chroma_gain)
            .chain(&self.luminance_amount)
            .all(|value| value.is_finite());
        stored && derived
    }

    /// The unit and its non-neutral sliders, at the parameters' declared display precision. The
    /// host compares compiled operations by this string, so two units that describe themselves
    /// identically must process identically: every coefficient is a pure function of the values
    /// named here, and a slider left out is exactly zero.
    fn describe(&self) -> String {
        let mut fields = Vec::new();
        for (property, values) in [
            ("hue", &self.hue),
            ("saturation", &self.saturation),
            ("luminance", &self.luminance),
        ] {
            for (range, value) in values.iter().enumerate() {
                if *value != 0.0 {
                    fields.push(format!("{}-{property}:{value:+.0}", RANGE_NAMES[range]));
                }
            }
        }
        if fields.is_empty() {
            return "mixer(neutral)".into();
        }
        format!("mixer({})", fields.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::{fs, path::PathBuf};

    /// The frozen study's own centre table, restated here so this file's copy cannot drift from
    /// the document without a test naming it.
    const STUDY_CENTRES: [f64; RANGE_COUNT] = [
        29.233_885_192_3,
        52.984_679_594_0,
        109.769_232_076_5,
        142.495_338_887_8,
        194.768_947_932_0,
        264.052_020_638_1,
        293.937_640_814_5,
        328.363_417_923_5,
    ];

    /// The frozen study's gap table, to six decimals as the document prints it.
    const STUDY_GAPS: [f64; RANGE_COUNT] = [
        23.750_794, 56.784_552, 32.726_107, 52.273_609, 69.283_073, 29.885_620, 34.425_777,
        60.870_467,
    ];

    fn code_to_linear(code: u8) -> f64 {
        let encoded = f64::from(code) / 255.0;
        if encoded <= 0.040_45 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    }

    fn apply(rgb: [f32; 3], unit: &Mixer) -> [f32; 3] {
        let mut row = [rgb];
        unit.apply_row(0, 0, &mut row);
        row[0]
    }

    fn neutral() -> Mixer {
        Mixer::new([0.0; RANGE_COUNT], [0.0; RANGE_COUNT], [0.0; RANGE_COUNT])
    }

    #[test]
    fn centres_match_the_frozen_study_table() {
        assert_eq!(CENTRE_HUES_DEG, STUDY_CENTRES);
        for (range, gap) in GAPS_DEG.iter().enumerate() {
            assert!(
                (gap - STUDY_GAPS[range]).abs() < 5e-7,
                "{}: derived gap {gap} against the study's {}",
                RANGE_NAMES[range],
                STUDY_GAPS[range]
            );
        }
        let total: f64 = GAPS_DEG.iter().sum();
        assert!((total - 360.0).abs() < 1e-9, "the eight gaps span one turn");
    }

    fn hue_only(hue: [f64; RANGE_COUNT]) -> Mixer {
        Mixer::new(hue, [0.0; RANGE_COUNT], [0.0; RANGE_COUNT])
    }

    fn srgb_codes(codes: [u8; 3]) -> [f32; 3] {
        codes.map(|code| code_to_linear(code) as f32)
    }

    fn hue_of(rgb: [f32; 3]) -> f64 {
        f64::from(normalize_hue_deg(colour::hue_degrees(colour::to_oklab(
            rgb,
        ))))
    }

    /// `+100` displaces a centre by the reach of the gap to the neighbour in the direction of
    /// travel, and `-100` by the reach of the gap behind: read off the knot values the unit holds,
    /// then measured on the reference colour itself through the whole unit.
    #[test]
    fn a_full_hue_slider_moves_its_centre_the_reach_of_the_gap_in_the_direction_of_travel() {
        for range in 0..RANGE_COUNT {
            let colour = srgb_codes(RANGE_REFERENCE_CODES[range]);
            let start = hue_of(colour);
            for (value, gap) in [
                (100.0, GAPS_DEG[range]),
                (-100.0, -GAPS_DEG[(range + RANGE_COUNT - 1) % RANGE_COUNT]),
            ] {
                let mut hue = [0.0; RANGE_COUNT];
                hue[range] = value;
                let unit = hue_only(hue);
                let expected = HUE_REACH_F64 * gap;
                assert!(
                    (f64::from(unit.hue_warp[range][0]) - expected).abs() < 1e-5,
                    "{} at {value}: knot displacement {} against {expected}",
                    RANGE_NAMES[range],
                    unit.hue_warp[range][0]
                );
                let travelled =
                    (hue_of(apply(colour, &unit)) - start + 540.0).rem_euclid(360.0) - 180.0;
                assert!(
                    (travelled - expected).abs() < 2e-3,
                    "{} at {value}: the reference colour travelled {travelled} degrees, expected \
                     {expected}",
                    RANGE_NAMES[range]
                );
            }
        }
    }

    /// Two neighbours driven at each other share one slider's travel: at `+100`/`-100` each moves
    /// half the reach, at `+60`/`-60` the pair is scaled by `100 / 120`, and at `+50`/`-50` the
    /// limit is exactly met and nothing is scaled.
    #[test]
    fn opposing_neighbours_share_one_sliders_travel() {
        for range in 0..RANGE_COUNT {
            let next = (range + 1) % RANGE_COUNT;
            let reach = HUE_REACH_F64 * GAPS_DEG[range];
            for (magnitude, expected_each) in [
                (100.0, 0.5 * reach),
                (60.0, 0.5 * reach),
                (50.0, 0.5 * reach),
                (30.0, 0.3 * reach),
            ] {
                let mut hue = [0.0; RANGE_COUNT];
                hue[range] = magnitude;
                hue[next] = -magnitude;
                let unit = hue_only(hue);
                assert!(
                    (f64::from(unit.hue_warp[range][0]) - expected_each).abs() < 1e-5
                        && (f64::from(unit.hue_warp[next][0]) + expected_each).abs() < 1e-5,
                    "{} +{magnitude} against {} -{magnitude}: displacements {} and {}, expected \
                     ±{expected_each}",
                    RANGE_NAMES[range],
                    RANGE_NAMES[next],
                    unit.hue_warp[range][0],
                    unit.hue_warp[next][0]
                );
            }
        }
    }

    /// The warp `H(h) = h + D(h)` the unit evaluates, in f32 exactly as the pixel path does, is
    /// strictly increasing around the whole circle for every one of the 3^8 combinations of the
    /// eight hue sliders at `-100`, `0` and `+100`, sampled 450 times per segment.
    #[test]
    fn the_hue_warp_is_strictly_increasing_for_every_three_level_combination() {
        const SAMPLES: u32 = 450;
        let mut worst = f64::INFINITY;
        for combination in 0..3usize.pow(RANGE_COUNT as u32) {
            let mut code = combination;
            let hue: [f64; RANGE_COUNT] = std::array::from_fn(|_| {
                let level = code % 3;
                code /= 3;
                [-100.0, 0.0, 100.0][level]
            });
            let unit = hue_only(hue);
            let mut previous = f64::NEG_INFINITY;
            for (range, [c0, c1, c2, c3]) in unit.hue_warp.iter().enumerate() {
                for step in 0..SAMPLES {
                    let t = step as f32 / SAMPLES as f32;
                    let displacement = c0 + t * (c1 + t * (c2 + t * c3));
                    let input = CENTRE_HUES_DEG[range] + f64::from(t) * GAPS_DEG[range];
                    let output = input + f64::from(displacement);
                    assert!(
                        output > previous,
                        "{hue:?}: the warp folds at {input} degrees ({output} after {previous})"
                    );
                    if previous.is_finite() {
                        worst = worst.min(output - previous);
                    }
                    previous = output;
                }
            }
            let wrapped = CENTRE_HUES_DEG[0] + 360.0 + f64::from(unit.hue_warp[0][0]);
            assert!(
                wrapped > previous,
                "{hue:?}: the warp folds across the seam"
            );
        }
        assert!(worst > 0.0);
    }

    /// A single hue slider moves the knot it owns and changes the slopes of that knot and its two
    /// neighbours, so its warp is confined to the four segments between the centres two ranges
    /// either side. Every other segment's cubic is four exact zeros, so it rotates nothing.
    #[test]
    fn a_hue_slider_leaves_every_segment_beyond_its_neighbours_exactly_unmoved() {
        for range in 0..RANGE_COUNT {
            for value in [-100.0, -35.0, 20.0, 100.0] {
                let mut hue = [0.0; RANGE_COUNT];
                hue[range] = value;
                let unit = hue_only(hue);
                for (segment, coefficients) in unit.hue_warp.iter().enumerate() {
                    let offset = (segment + RANGE_COUNT - range) % RANGE_COUNT;
                    let inside = matches!(offset, 0 | 1 | 6 | 7);
                    assert_eq!(
                        coefficients.iter().all(|c| *c == 0.0),
                        !inside,
                        "{} at {value}: segment {segment} holds {coefficients:?}",
                        RANGE_NAMES[range]
                    );
                }
            }
        }
    }

    /// Every one of the 256 grey codes is returned bit for bit by every hue and luminance slider
    /// and every saturation increase at both extremes: those three are ramped, the ramp on a grey's
    /// noise chroma rounds every amount to its identity in f32, and the only arithmetic left is the
    /// round trip the neutral unit does. A saturation decrease is not ramped, so it may shrink a
    /// grey's noise chroma further; the grey then still renders as the same code in all three
    /// channels.
    #[test]
    fn greys_are_unchanged_under_every_slider() {
        let neutral = neutral();
        for code in 0u8..=255 {
            let linear = code_to_linear(code) as f32;
            let grey = [linear, linear, linear];
            let expected = apply(grey, &neutral);
            for range in 0..RANGE_COUNT {
                for value in [-100.0, 100.0] {
                    for property in 0..3 {
                        let mut sliders = [[0.0; RANGE_COUNT]; 3];
                        sliders[property][range] = value;
                        let unit = Mixer::new(sliders[0], sliders[1], sliders[2]);
                        let produced = apply(grey, &unit);
                        if property == 1 && value < 0.0 {
                            assert_eq!(
                                crate::render::quantize_pixel(produced),
                                [code; 3],
                                "grey {code} changed code under {} saturation {value}",
                                RANGE_NAMES[range]
                            );
                        } else {
                            assert_eq!(
                                produced, expected,
                                "grey {code} moved under {} {property} at {value}",
                                RANGE_NAMES[range]
                            );
                        }
                    }
                }
            }
        }
    }

    /// Hue and saturation reach a colour two centres away through their shared segment weights
    /// and luminance does too; each is exactly `+0.0` beyond them, so saturation and luminance
    /// change nothing two centres away. The hue warp's support is one segment wider (see
    /// `a_hue_slider_leaves_every_segment_beyond_its_neighbours_exactly_unmoved`), so a hue slider
    /// is checked three centres away. Equality, not a tolerance.
    #[test]
    fn a_colour_outside_a_sliders_support_is_bit_identical_under_it() {
        let neutral = neutral();
        for range in 0..RANGE_COUNT {
            for property in 0..3 {
                let offsets = if property == 0 {
                    [3usize, RANGE_COUNT - 3]
                } else {
                    [2usize, RANGE_COUNT - 2]
                };
                for offset in offsets {
                    let rgb = srgb_codes(RANGE_REFERENCE_CODES[(range + offset) % RANGE_COUNT]);
                    let expected = apply(rgb, &neutral);
                    for value in [-100.0, 50.0, 100.0] {
                        let mut sliders = [[0.0; RANGE_COUNT]; 3];
                        sliders[property][range] = value;
                        let unit = Mixer::new(sliders[0], sliders[1], sliders[2]);
                        assert_eq!(
                            apply(rgb, &unit),
                            expected,
                            "{} at {value} (property {property}) moved the colour {offset} \
                             centres away",
                            RANGE_NAMES[range]
                        );
                    }
                }
            }
        }
    }

    /// A pseudo-random stream of pixels: saturated, pastel, near-grey, very dark and out of the
    /// `[0, 1]` range.
    fn awkward_pixels(count: usize) -> Vec<[f32; 3]> {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 40) as f32 / (1u64 << 24) as f32
        };
        (0..count)
            .map(|index| match index % 4 {
                0 => [next(), next(), next()],
                1 => {
                    let grey = next();
                    let noise = 0.01 * grey;
                    [
                        grey + noise * (next() - 0.5),
                        grey + noise * (next() - 0.5),
                        grey + noise * (next() - 0.5),
                    ]
                }
                2 => [0.004 * next(), 0.004 * next(), 0.004 * next()],
                _ => [1.6 * next() - 0.2, 1.6 * next() - 0.2, 1.6 * next() - 0.2],
            })
            .collect()
    }

    /// Every saturation slider at `-100` gives every pixel a chroma factor of exactly zero —
    /// near-greys and the darkest pixels included, because a decrease is not ramped — and the
    /// achromatic result reconstructs to three bit-identical channels.
    #[test]
    fn every_saturation_slider_at_minus_100_makes_every_pixel_exactly_grey() {
        let unit = Mixer::new(
            [0.0; RANGE_COUNT],
            [-100.0; RANGE_COUNT],
            [0.0; RANGE_COUNT],
        );
        for step in 0..36_000 {
            let hue = step as f32 / 100.0;
            for ramp in [0.0, 1e-6, 0.3, 1.0] {
                assert_eq!(unit.amounts(ramp, hue).1, 0.0, "hue {hue}, ramp {ramp}");
            }
        }
        for (index, pixel) in awkward_pixels(200_000).into_iter().enumerate() {
            let [r, g, b] = apply(pixel, &unit);
            assert!(
                r == g && g == b,
                "pixel {index} {pixel:?} came out as {:?}",
                [r, g, b]
            );
        }
        // The same with every luminance slider set too: the grey then carries the response.
        let lit = Mixer::new(
            [0.0; RANGE_COUNT],
            [-100.0; RANGE_COUNT],
            [60.0; RANGE_COUNT],
        );
        for pixel in awkward_pixels(20_000) {
            let [r, g, b] = apply(pixel, &lit);
            assert!(r == g && g == b, "{pixel:?} came out as {:?}", [r, g, b]);
        }
    }

    /// A decrease is applied in full whatever the chroma, and only an increase is scaled by the
    /// ramp: at a range's own centre, `-50` gives the factor `0.5` at any ramp and `+50` gives
    /// `1 + 0.5 * ramp`.
    #[test]
    fn a_saturation_decrease_is_not_ramped_and_an_increase_is() {
        for range in 0..RANGE_COUNT {
            let centre = CENTRES[range];
            for (value, ramp, expected) in [
                (-50.0, 0.0, 0.5),
                (-50.0, 0.25, 0.5),
                (-50.0, 1.0, 0.5),
                (50.0, 0.0, 1.0),
                (50.0, 0.25, 1.125),
                (50.0, 1.0, 1.5),
            ] {
                let mut saturation = [0.0; RANGE_COUNT];
                saturation[range] = value;
                let unit = Mixer::new([0.0; RANGE_COUNT], saturation, [0.0; RANGE_COUNT]);
                let factor = unit.amounts(ramp, centre).1;
                assert!(
                    (factor - expected).abs() < 1e-6,
                    "{} at {value}, ramp {ramp}: factor {factor}, expected {expected}",
                    RANGE_NAMES[range]
                );
            }
        }
    }

    /// An achromatic Oklab colour reconstructs to three bit-identical channels, `L^3`, while the
    /// shared conversion on its own leaves them a few ulps apart; the figure printed is how many of
    /// a million evenly spaced `L` in `[0, 1]` it would render with a split code.
    #[test]
    fn an_achromatic_colour_reconstructs_to_three_identical_channels() {
        let samples = 1_000_000;
        let mut split = 0usize;
        let mut worst = 0.0f32;
        for step in 0..=samples {
            let l = step as f32 / samples as f32;
            for zero in [0.0f32, -0.0] {
                let lab = Oklab {
                    l,
                    a: zero,
                    b: -zero,
                };
                let [r, g, b] = reconstruct(lab);
                assert!(
                    r == g && g == b && r == l * l * l,
                    "L = {l}: {:?}",
                    [r, g, b]
                );
            }
            let shared = colour::from_oklab(Oklab { l, a: 0.0, b: 0.0 });
            worst = worst.max(
                (shared[0] - shared[1])
                    .abs()
                    .max((shared[1] - shared[2]).abs()),
            );
            let codes = crate::render::quantize_pixel(shared);
            if codes[0] != codes[1] || codes[1] != codes[2] {
                split += 1;
            }
        }
        // Printed with --nocapture: the evidence that the dedicated path is needed.
        println!(
            "the shared from_oklab splits {split} of {} achromatic L values across an output code; \
             largest channel difference {worst:e}",
            samples + 1
        );
    }

    /// `describe` names the unit and exactly its non-neutral fields, and two different payloads
    /// never describe themselves the same way.
    #[test]
    fn describe_names_the_non_neutral_fields() {
        assert_eq!(neutral().describe(), "mixer(neutral)");
        let mut hue = [0.0; RANGE_COUNT];
        hue[0] = 20.0;
        let mut luminance = [0.0; RANGE_COUNT];
        luminance[4] = -15.0;
        let unit = Mixer::new(hue, [0.0; RANGE_COUNT], luminance);
        assert_eq!(unit.describe(), "mixer(red-hue:+20, aqua-luminance:-15)");
        assert!(unit.is_finite());
    }

    // -------------------------------------------------------------------------------------------
    // The frozen fixtures
    // -------------------------------------------------------------------------------------------

    fn fixture_file() -> Value {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mixer/mixer-cases.json");
        serde_json::from_str(&fs::read_to_string(&path).expect("the mixer fixtures"))
            .expect("valid fixture JSON")
    }

    fn slider_array(set: &Value, property: &str) -> [f64; RANGE_COUNT] {
        let values = set[property].as_array().expect("a slider array");
        std::array::from_fn(|range| values[range].as_f64().expect("a number"))
    }

    /// Production against every one of the 576 frozen cases, in linear light, within the study's
    /// frozen tolerance `1e-5 + 1e-5 * |reference|`. The 8-bit inputs are decoded in f64 exactly as
    /// the reference decodes them and cast to f32, so what this measures is the unit's own f32
    /// error and nothing else; the quantization boundary is checked through the real render path in
    /// `tests/mixer_module.rs`.
    #[test]
    fn production_matches_every_frozen_fixture_case_within_the_frozen_tolerance() {
        let file = fixture_file();
        assert_eq!(
            file["range_order"]
                .as_array()
                .expect("the range order")
                .iter()
                .map(|name| name.as_str().expect("a name"))
                .collect::<Vec<_>>(),
            RANGE_NAMES,
            "the fixture's range order is the unit's wheel order"
        );
        let sets = file["parameter_sets"].as_array().expect("the sets");
        let cases = file["cases"].as_array().expect("the cases");
        assert_eq!(cases.len(), 576, "every frozen case is checked");

        let mut worst = 0.0f64;
        let mut worst_case = String::new();
        for case in cases {
            let name = case["name"].as_str().expect("a case name");
            let set = sets
                .iter()
                .find(|set| set["name"] == case["parameters"])
                .unwrap_or_else(|| panic!("{name}: the named parameter set"));
            let unit = Mixer::new(
                slider_array(set, "hue"),
                slider_array(set, "saturation"),
                slider_array(set, "luminance"),
            );
            assert!(unit.is_finite(), "{name}");
            let input = &case["input"];
            let rgb: [f32; 3] = match input["kind"].as_str().expect("an input kind") {
                "srgb8" => {
                    let codes = input["rgb"].as_array().expect("three codes");
                    std::array::from_fn(|channel| {
                        code_to_linear(codes[channel].as_u64().expect("a code") as u8) as f32
                    })
                }
                "linear" => {
                    let values = input["rgb"].as_array().expect("three values");
                    std::array::from_fn(|channel| values[channel].as_f64().expect("a value") as f32)
                }
                other => panic!("{name}: unknown input kind {other}"),
            };
            let produced = apply(rgb, &unit);
            let expected = case["expected_linear"].as_array().expect("three values");
            for channel in 0..3 {
                let reference = expected[channel].as_f64().expect("a value");
                let deviation = (f64::from(produced[channel]) - reference).abs();
                let tolerance = 1e-5 + 1e-5 * reference.abs();
                assert!(
                    deviation <= tolerance,
                    "{name} channel {channel}: production {} against reference {reference}, \
                     deviation {deviation} over the tolerance {tolerance}",
                    produced[channel]
                );
                if deviation > worst {
                    worst = deviation;
                    worst_case = format!("{name} channel {channel}");
                }
            }
        }
        // Printed with --nocapture so the handoff can quote a measured figure rather than a bound.
        println!("maximum observed deviation {worst} at {worst_case}");
        assert!(worst < 1e-5 + 1e-5, "the worst case stays inside the bound");
    }
}
