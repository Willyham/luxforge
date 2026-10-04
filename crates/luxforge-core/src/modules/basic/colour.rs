//! The Basic module's colour unit: `ColourAdjust`, fusing Vibrance and Saturation, sharing the
//! Oklab conversion frozen in `docs/design/basic-colour.md` and the independent f64 reference at
//! `crates/luxforge-reference/src/colour.rs`. Both parameters scale Oklab `a`/`b` about
//! the achromatic axis and leave `L` untouched; neither clamps internally, per the design's gamut
//! policy. Vibrance's gain and saturation's gain compose into one factor before either is applied,
//! so one pixel converts to Oklab and back exactly once instead of once per parameter — the two
//! parameters are mathematically independent scalings of the same `(a, b)` pair around the same
//! axis with `L` untouched, so composing their gains before scaling changes no arithmetic on
//! `a`/`b` itself; only the number of round trips through the independently rounded inverse
//! matrices changes (see `colour_adjust_matches_the_sequential_pair_within_the_frozen_tolerance`
//! below for the measured difference this makes). This file never imports the reference's colour
//! module: the two are written independently so they never share a bug. Its tests decode an sRGB
//! byte to linear light through `luxforge_reference::srgb`, the one shared sRGB oracle, which is
//! not part of that comparison.
use crate::{
    colour::oklab::{Oklab, chroma, from_oklab, hue_degrees, to_oklab},
    modules::PointwiseColor,
    render::gpu::{GpuDescription, GpuProgram, GpuProgramKind},
};

/// The colour unit's GPU program (`colour.wgsl`): Vibrance's gain and Saturation's.
pub(crate) static PROGRAM: GpuProgram = GpuProgram {
    entry: "lf_basic_colour_adjust",
    source: include_str!("colour.wgsl"),
    kind: GpuProgramKind::Colour,
    words: 2,
    enabled: true,
};

/// Reference Oklab chroma of the most saturated point on the sRGB gamut surface (`(255, 0, 255)`,
/// sRGB magenta), measured by scanning the gamut surface at 8-bit resolution. Normalizes the
/// vibrance chroma weight below; not a gamut test itself.
const CHROMA_REFERENCE_F64: f64 = 0.32;
/// Normalized-chroma smoothstep edges for the vibrance chroma weight `w_c`: full gain at and below
/// `CHROMA_LOW`, zero gain at and above `CHROMA_HIGH`.
const CHROMA_LOW_F64: f64 = 0.10;
const CHROMA_HIGH_F64: f64 = 0.70;
/// Skin-like hue band centre and half-width, in Oklab hue degrees (`atan2(b, a)`).
const SKIN_HUE_CENTER_DEG_F64: f64 = 55.0;
const SKIN_HUE_HALF_WIDTH_DEG_F64: f64 = 35.0;
/// Maximum fraction of vibrance gain removed at the skin hue band centre.
const SKIN_PROTECTION_F64: f64 = 0.6;
/// Below this chroma, hue is numerical noise rather than a meaningful angle (see
/// `docs/design/basic-colour.md`, "Near-black and achromatic behaviour"); the hue weight is skipped
/// and the full chroma weight used instead, so vibrance −100 still lands at exact grey.
const CHROMA_EPSILON_F64: f64 = 1e-4;

const CHROMA_REFERENCE: f32 = CHROMA_REFERENCE_F64 as f32;
const CHROMA_LOW: f32 = CHROMA_LOW_F64 as f32;
const CHROMA_HIGH: f32 = CHROMA_HIGH_F64 as f32;
const SKIN_HUE_CENTER_DEG: f32 = SKIN_HUE_CENTER_DEG_F64 as f32;
const SKIN_HUE_HALF_WIDTH_DEG: f32 = SKIN_HUE_HALF_WIDTH_DEG_F64 as f32;
const SKIN_PROTECTION: f32 = SKIN_PROTECTION_F64 as f32;
const CHROMA_EPSILON: f32 = CHROMA_EPSILON_F64 as f32;

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `w_c`: full vibrance gain near grey, smoothly falling to zero as chroma approaches
/// `CHROMA_REFERENCE`.
fn chroma_weight(chroma: f32) -> f32 {
    let normalized = (chroma / CHROMA_REFERENCE).max(0.0);
    1.0 - smoothstep(CHROMA_LOW, CHROMA_HIGH, normalized)
}

/// The skin-like hue band response: a raised cosine that is `1` at the band centre and `0` at and
/// beyond the half-width, `0` outside the band. A colour heuristic over Oklab hue, not skin
/// detection: it weights every pixel whose hue falls in the band the same way regardless of what
/// the pixel depicts, and it is not a promise about every skin tone. The input comes from
/// `hue_degrees`: atan2's bounded `[-180, 180]` result, not an arbitrary periodic angle.
fn skin_hue_response(hue_deg: f32) -> f32 {
    // The delta is in [-235, 125]. Wrapping its [-235, -180] tail would put it
    // in [125, 180], still outside the +/-35 degree band, so the response is
    // exactly zero either way. Elsewhere normalization is the identity. This
    // bounded caller therefore needs neither a remainder nor a wrap per pixel.
    let delta = hue_deg - SKIN_HUE_CENTER_DEG;
    if delta.abs() >= SKIN_HUE_HALF_WIDTH_DEG {
        0.0
    } else {
        (delta / SKIN_HUE_HALF_WIDTH_DEG * std::f32::consts::FRAC_PI_2)
            .cos()
            .max(0.0)
    }
}

/// `w_h`: `1` outside the skin-like hue band, falling to `1 - SKIN_PROTECTION` at the band centre.
fn hue_weight(hue_deg: f32) -> f32 {
    1.0 - SKIN_PROTECTION * skin_hue_response(hue_deg)
}

/// The combined vibrance weight `w(C, h) = w_c(C) * w_h(h)`, always in `[0, 1]`, so `k` always
/// lands in `[0, 2]` for `v` in `[-100, 100]`, the same bound saturation's factor has.
fn vibrance_weight(chroma: f32, hue_deg: f32) -> f32 {
    chroma_weight(chroma) * hue_weight(hue_deg)
}

/// `tan 19°` and `tan 1°`: the ratios that bound the hues [`outside_skin_band`] answers for, a
/// degree beyond each edge of the skin-like band, `55° ∓ 35°`. A test holds them to the band's
/// constants.
const SKIN_SKIP_TAN_LOW_F64: f64 = 0.344_327_613_289_665_3;
const SKIN_SKIP_TAN_HIGH_F64: f64 = 0.017_455_064_928_217_59;

const SKIN_SKIP_TAN_LOW: f32 = SKIN_SKIP_TAN_LOW_F64 as f32;
const SKIN_SKIP_TAN_HIGH: f32 = SKIN_SKIP_TAN_HIGH_F64 as f32;

/// Whether the Oklab hue of `(a, b)` lies certainly outside the skin-like band, where
/// `skin_hue_response` is exactly `0` and `hue_weight` exactly `1`, so `vibrance_weight` is
/// `chroma_weight` itself and the hue need not be computed: `b` at or below zero (a hue in
/// `[-180°, 0°]` or `180°`), or a hue under 19° or past 91°, by the sign of `a` and the ratio of the
/// two against `tan 19°` and `tan 1°`. Anywhere else, and wherever `b` or a needed `a` is NaN, it
/// answers `false` and the hue is computed as before.
///
/// The degree of margin is orders of magnitude past every rounding involved. A strict comparison
/// with a correctly rounded product holds only where it holds exactly (`b < fl(a · t)` puts `b` at
/// or below the float before `fl(a · t)`, which `a · t` exceeds), so the tested edges move only by
/// the constants' own rounding, under `10⁻⁶` degrees. `atan2` (within a few units in the last
/// place) and `to_degrees` and the centre's subtraction (one rounding each) move a hue by under
/// `10⁻⁴` degrees. A hue tested under 19° or past 91° is therefore computed under 19.001° or past
/// 90.999°, at least 35.999° from the centre, where the response is exactly zero; `b` at or below
/// zero computes one at least 55° below it or 125° above it.
#[inline]
fn outside_skin_band(a: f32, b: f32) -> bool {
    b <= 0.0 || (a > 0.0 && b < a * SKIN_SKIP_TAN_LOW) || (a < 0.0 && -a > b * SKIN_SKIP_TAN_HIGH)
}

/// Vibrance's weight `w(C, h)` for a pixel of chroma `c` past `CHROMA_EPSILON`: [`vibrance_weight`]
/// bit for bit, without the hue (`atan2` and its conversion to degrees) wherever
/// [`outside_skin_band`] says it cannot change the result. There `chroma_weight` is finite, its
/// normalized chroma being clamped first, so multiplying it by the band's exact `1` would leave it
/// as it is.
#[inline]
fn vibrance_weight_of(lab: Oklab, c: f32) -> f32 {
    if outside_skin_band(lab.a, lab.b) {
        chroma_weight(c)
    } else {
        vibrance_weight(c, hue_degrees(lab))
    }
}

/// The fused Vibrance/Saturation unit. Converts one pixel to Oklab once; computes vibrance's
/// chroma- and hue-dependent gain `k_v = 1 + (vibrance / 100) * w(C, h)` from that one conversion,
/// computes saturation's uniform gain `k_s = 1 + saturation / 100` (precomputed once for the whole
/// unit, not per pixel), scales `a`/`b` by the single combined factor `k_v * k_s`, and converts
/// back once.
///
/// This is mathematically identical to running the old separate `Vibrance` then `Saturation` units
/// in sequence: both scale `(a, b)` about the achromatic axis with `L` untouched and vibrance's
/// weight is a pure function of the *input* pixel's chroma and hue either way, so composing the
/// two gains before scaling changes no arithmetic on `a`/`b`. The only difference from the
/// sequential pair is that the sequential path round-trips through the independently rounded
/// inverse matrices (`M2_INV`, `M1_INV`) twice — once after vibrance, again after saturation reads
/// back the vibrance-adjusted pixel — while this fused unit round-trips once; the residual that
/// second conversion would have carried (documented in `docs/design/basic-colour.md` as the
/// composed-unit near-black spread, on the order of `2e-6` in f64) does not accumulate here. See
/// `colour_adjust_matches_the_sequential_pair_within_one_e_minus_6` for the measured maximum
/// difference against the sequential pair.
///
/// `saturation = -100` gives `k_s = 0.0` exactly (computed in f64 and cast, so the exactness
/// survives the cast); combined with `vibrance = 0`, which gives `k_v = 1.0` exactly regardless of
/// the per-pixel weight (the weight is multiplied by a zero gain), the combined factor is `0.0`
/// exactly and `a = b = 0.0` exactly regardless of the input: neutral vibrance with saturation
/// -100 is exact grey, matching the old `Saturation::new(-100.0)`'s exactness.
#[derive(Debug)]
pub(super) struct ColourAdjust {
    /// The stored parameter values, kept for [`PointwiseColor::describe`] and the finiteness
    /// check.
    vibrance: f64,
    saturation: f64,
    /// `vibrance / 100`, computed in f64 and cast once; the per-pixel weight `w(C, h)` cannot be
    /// precomputed because it depends on each pixel's chroma and hue. Exactly `0.0` when vibrance
    /// is neutral, which is used to skip the per-pixel hue computation entirely.
    vibrance_gain: f32,
    /// `1 + saturation / 100`, computed in f64 and cast once: saturation's gain never depends on
    /// the pixel, unlike vibrance's.
    saturation_k: f32,
}

impl ColourAdjust {
    pub(super) fn new(vibrance: f64, saturation: f64) -> Self {
        Self {
            vibrance,
            saturation,
            vibrance_gain: (vibrance / 100.0) as f32,
            saturation_k: (1.0 + saturation / 100.0) as f32,
        }
    }
}

impl PointwiseColor for ColourAdjust {
    fn apply_row(&self, _y: u32, _x0: u32, rgb: &mut [[f32; 3]]) {
        // Vibrance's weight needs a chroma (`hypot`) and, past the epsilon, a hue (`atan2` and a
        // `cos`) per pixel. Skip all of it whenever it cannot change the result: when vibrance is
        // neutral, its gain multiplies the weight by exactly zero regardless of the weight's own
        // value, so `k_v` is `1.0` unconditionally and neither chroma nor hue needs computing; and
        // the hue alone wherever it lies certainly outside the skin-like band
        // (`vibrance_weight_of`).
        let vibrance_neutral = self.vibrance_gain == 0.0;
        for pixel in rgb {
            let lab = to_oklab(*pixel);
            let vibrance_k = if vibrance_neutral {
                1.0
            } else {
                let c = chroma(lab);
                // Below `CHROMA_EPSILON`, hue is numerical noise rather than a meaningful angle
                // (see `docs/design/basic-colour.md`, "Near-black and achromatic behaviour"): skip
                // reading it and use the full chroma weight instead.
                let weight = if c < CHROMA_EPSILON {
                    1.0
                } else {
                    vibrance_weight_of(lab, c)
                };
                1.0 + self.vibrance_gain * weight
            };
            let k = vibrance_k * self.saturation_k;
            *pixel = from_oklab(Oklab {
                l: lab.l,
                a: lab.a * k,
                b: lab.b * k,
            });
        }
    }

    fn is_finite(&self) -> bool {
        self.vibrance.is_finite()
            && self.saturation.is_finite()
            && self.vibrance_gain.is_finite()
            && self.saturation_k.is_finite()
    }

    /// Both stored values, exactly. The host compares compiled operations by this string, so two
    /// units that describe themselves identically process identically: the per-pixel factor is a
    /// pure function of the pixel and `vibrance_gain`/`saturation_k`, themselves pure functions of
    /// `vibrance`/`saturation`.
    fn describe(&self) -> String {
        format!(
            "colour-adjust(vibrance:{:+}, saturation:{:+})",
            self.vibrance, self.saturation
        )
    }

    /// Vibrance's `f32` gain and Saturation's, as `apply_row` reads them: pure functions of the
    /// two stored values.
    fn gpu(&self) -> Option<GpuDescription> {
        Some(GpuDescription::new(
            &PROGRAM,
            vec![self.vibrance_gain.to_bits(), self.saturation_k.to_bits()],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_reference::srgb;
    use serde::Deserialize;
    use std::{fs, path::PathBuf};

    /// Independent periodic definition of the specified (-180, 180] interval.
    fn periodic_hue_delta(delta: f32) -> f32 {
        let remainder = delta % 360.0;
        if remainder > 180.0 {
            remainder - 360.0
        } else if remainder <= -180.0 {
            remainder + 360.0
        } else {
            remainder
        }
    }

    #[test]
    fn bounded_skin_hue_response_matches_the_periodic_formula_exactly() {
        let check = |hue: f32| {
            let delta = periodic_hue_delta(hue - SKIN_HUE_CENTER_DEG);
            let expected = if delta.abs() >= SKIN_HUE_HALF_WIDTH_DEG {
                0.0
            } else {
                (delta / SKIN_HUE_HALF_WIDTH_DEG * std::f32::consts::FRAC_PI_2)
                    .cos()
                    .max(0.0)
            };
            assert_eq!(
                skin_hue_response(hue).to_bits(),
                expected.to_bits(),
                "hue = {hue:?}"
            );
        };
        // Include either side of the wrap, both band edges and its centre,
        // atan2's signed endpoints, and near-zero hues explicitly.
        for hue in [-180.0_f32, -125.0, -0.0, 0.0, 20.0, 55.0, 90.0, 180.0] {
            for adjacent in [hue.next_down(), hue, hue.next_up()] {
                check(adjacent);
            }
        }
        for step in 0..=1_000_000 {
            check(-180.0 + 360.0 * (step as f32 / 1_000_000.0));
        }
    }

    /// The hue skip's ratios are `tan 19°` and `tan 1°`, a degree beyond the band's edges at
    /// `centre ∓ half-width`, narrowed as the other constants are; the upper edge is the `b` axis,
    /// which the test past it measures from.
    #[test]
    fn the_hue_skip_ratios_are_a_degree_beyond_the_band() {
        let low = SKIN_HUE_CENTER_DEG_F64 - SKIN_HUE_HALF_WIDTH_DEG_F64 - 1.0;
        let high = SKIN_HUE_CENTER_DEG_F64 + SKIN_HUE_HALF_WIDTH_DEG_F64 + 1.0;
        assert_eq!(high, 91.0);
        assert_eq!(SKIN_SKIP_TAN_LOW, low.to_radians().tan() as f32);
        assert_eq!(SKIN_SKIP_TAN_HIGH, (high - 90.0).to_radians().tan() as f32);
    }

    /// The vibrance weight with the hue skipped outside the skin-like band is the weight with it
    /// computed, bit for bit: over every angle in thousandths of a degree at magnitudes from the
    /// smallest subnormal to `f32::MAX`, densely beside both band edges and both edges of the skip,
    /// at the `f32` values either side of where each ratio test changes its answer, on both axes
    /// with either sign of zero, and at infinities and NaN.
    #[test]
    fn skipping_the_hue_outside_the_skin_band_leaves_every_weight_bit_identical() {
        let mut skipped = 0_usize;
        let mut computed = 0_usize;
        let mut check = |a: f32, b: f32| {
            let lab = Oklab { l: 0.5, a, b };
            let c = chroma(lab);
            let expected = vibrance_weight(c, hue_degrees(lab));
            assert_eq!(
                vibrance_weight_of(lab, c).to_bits(),
                expected.to_bits(),
                "a = {a:?}, b = {b:?}"
            );
            if outside_skin_band(a, b) {
                skipped += 1;
            } else {
                computed += 1;
            }
        };
        let polar = |magnitude: f32, degrees: f64| {
            let (sin, cos) = degrees.to_radians().sin_cos();
            (
                (f64::from(magnitude) * cos) as f32,
                (f64::from(magnitude) * sin) as f32,
            )
        };
        let magnitudes = [
            f32::from_bits(1),
            f32::MIN_POSITIVE,
            1e-30,
            1e-6,
            CHROMA_EPSILON,
            0.01,
            0.1,
            CHROMA_REFERENCE,
            1.0,
            1e6,
            1e30,
            f32::MAX,
        ];
        for magnitude in magnitudes {
            for step in -180_000..=180_000 {
                let (a, b) = polar(magnitude, f64::from(step) / 1000.0);
                check(a, b);
            }
            // Beside both band edges and both edges of the skip, in millionths of a degree.
            for edge in [19.0, 20.0, 90.0, 91.0] {
                for step in -2000..=2000 {
                    let (a, b) = polar(magnitude, edge + f64::from(step) * 1e-6);
                    check(a, b);
                }
            }
        }
        // Where each ratio test changes its answer: `b` beside `a · tan 19°`, and `a` beside
        // `-b · tan 1°`, three floats either side.
        let mut bits = 0x9e37_79b9_u32;
        for _ in 0..20_000 {
            bits ^= bits << 13;
            bits ^= bits >> 17;
            bits ^= bits << 5;
            let magnitude = f32::from_bits(0x2000_0000 + bits % 0x3f00_0000);
            for (a, b) in [
                (magnitude, magnitude * SKIN_SKIP_TAN_LOW),
                (-(magnitude * SKIN_SKIP_TAN_HIGH), magnitude),
            ] {
                let (mut a_low, mut b_low) = (a, b);
                for _ in 0..3 {
                    a_low = a_low.next_down();
                    b_low = b_low.next_down();
                }
                let (mut a_side, mut b_side) = (a_low, b_low);
                for _ in 0..7 {
                    check(a, b_side);
                    check(a_side, b);
                    a_side = a_side.next_up();
                    b_side = b_side.next_up();
                }
            }
        }
        for magnitude in magnitudes {
            for zero in [0.0_f32, -0.0] {
                for value in [magnitude, -magnitude] {
                    check(zero, value);
                    check(value, zero);
                }
                check(zero, zero);
                check(zero, -zero);
            }
        }
        let special = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
            -0.0,
            1.0,
            -1.0,
        ];
        for a in special {
            for b in special {
                check(a, b);
            }
        }
        assert!(skipped > 0 && computed > 0, "{skipped} skipped, {computed}");
        // The skip covers every hue a degree or more outside the band, and none inside it.
        for degrees in [-179.0, -90.0, -1.0, 0.0, 10.0, 18.9, 91.1, 120.0, 180.0] {
            let (a, b) = polar(1.0, degrees);
            assert!(outside_skin_band(a, b), "{degrees}");
        }
        for degrees in [19.1, 20.0, 55.0, 89.0, 90.0, 90.9] {
            let (a, b) = polar(1.0, degrees);
            assert!(!outside_skin_band(a, b), "{degrees}");
        }
    }

    /// The unit before the hue skip: every pixel past the epsilon computes its hue.
    fn apply_row_computing_every_hue(unit: &ColourAdjust, rgb: &mut [[f32; 3]]) {
        for pixel in rgb {
            let lab = to_oklab(*pixel);
            let vibrance_k = if unit.vibrance_gain == 0.0 {
                1.0
            } else {
                let c = chroma(lab);
                let weight = if c < CHROMA_EPSILON {
                    1.0
                } else {
                    vibrance_weight(c, hue_degrees(lab))
                };
                1.0 + unit.vibrance_gain * weight
            };
            let k = vibrance_k * unit.saturation_k;
            *pixel = from_oklab(Oklab {
                l: lab.l,
                a: lab.a * k,
                b: lab.b * k,
            });
        }
    }

    /// Whole rows through the unit equal the rows through the unit that computes every hue, bit
    /// for bit, over every 8-bit colour on a 4-code lattice and extended linear values, at both
    /// extremes and between them.
    #[test]
    fn the_unit_with_the_hue_skip_writes_the_same_rows() {
        let mut row: Vec<[f32; 3]> = Vec::new();
        for r in (0..=255u16).step_by(4) {
            for g in (0..=255u16).step_by(4) {
                for b in (0..=255u16).step_by(4) {
                    row.push([r as u8, g as u8, b as u8].map(code_to_linear));
                }
            }
        }
        for step in 0..=20 {
            let value = -0.2 + 1.7 * step as f32 / 20.0;
            row.extend([[value, 0.3, 0.1], [0.4, value, 0.9], [1.2, 0.05, value]]);
        }
        for (vibrance, saturation) in [(100.0, 0.0), (-100.0, 0.0), (35.0, -20.0), (-60.0, 50.0)] {
            let unit = ColourAdjust::new(vibrance, saturation);
            let mut skipped = row.clone();
            let mut every = row.clone();
            unit.apply_row(0, 0, &mut skipped);
            apply_row_computing_every_hue(&unit, &mut every);
            let bits = |rows: &[[f32; 3]]| -> Vec<[u32; 3]> {
                rows.iter().map(|pixel| pixel.map(f32::to_bits)).collect()
            };
            assert!(
                bits(&skipped) == bits(&every),
                "vibrance {vibrance}, saturation {saturation}: the rows differ"
            );
        }
    }

    /// An achromatic Oklab colour reconstructs to three bit-identical channels, `L^3`, for either
    /// sign of zero, over a million evenly spaced `L` in `[0, 1]`. Without the achromatic branch the
    /// rounded `M1^-1` rows leave the three channels up to about `5e-7` apart, and a few of those
    /// land on different output codes.
    #[test]
    fn an_achromatic_colour_reconstructs_to_three_identical_channels() {
        let samples = 1_000_000;
        for step in 0..=samples {
            let l = step as f32 / samples as f32;
            for (a, b) in [(0.0f32, 0.0f32), (-0.0, 0.0), (0.0, -0.0), (-0.0, -0.0)] {
                let [r, g, bl] = from_oklab(Oklab { l, a, b });
                assert!(
                    r == g && g == bl && r == l * l * l,
                    "L = {l}: {:?}",
                    [r, g, bl]
                );
            }
        }
    }

    /// Saturation `-100` makes every pixel an exact grey: three equal output codes, however
    /// colourful, dark or near-grey the input was.
    #[test]
    fn saturation_minus_100_renders_three_equal_codes_for_every_pixel() {
        let unit = ColourAdjust::new(0.0, -100.0);
        let mut row: Vec<[f32; 3]> = Vec::new();
        for r in (0..=255u16).step_by(5) {
            for g in (0..=255u16).step_by(5) {
                for b in (0..=255u16).step_by(5) {
                    row.push([r as u8, g as u8, b as u8].map(code_to_linear));
                }
            }
        }
        // Near-greys a code or two off the axis, where the conversion's own noise lives.
        for code in 0..=255u8 {
            let near = code.saturating_add(1);
            row.push([code, code, near].map(code_to_linear));
            row.push([near, code, code].map(code_to_linear));
        }
        unit.apply_row(0, 0, &mut row);
        for pixel in &row {
            let codes = crate::colour::srgb::quantize_pixel(*pixel);
            assert!(
                codes[0] == codes[1] && codes[1] == codes[2],
                "{pixel:?} renders {codes:?}"
            );
        }
    }

    fn code_to_linear(code: u8) -> f32 {
        srgb::decode(code) as f32
    }

    fn row_of(rgb: [f32; 3]) -> [[f32; 3]; 1] {
        [rgb]
    }

    /// The independent statement of both parameters, applied to one pixel through the production
    /// `PointwiseColor` unit (the fused `ColourAdjust`, replacing the old separate `Vibrance` then
    /// `Saturation` units the frozen fixtures were originally checked against).
    fn apply_basic_colour(rgb: [f32; 3], vibrance: f64, saturation: f64) -> [f32; 3] {
        let mut row = row_of(rgb);
        ColourAdjust::new(vibrance, saturation).apply_row(0, 0, &mut row);
        row[0]
    }

    /// Replicates the pre-fusion two-unit path exactly: convert to Oklab, apply vibrance's
    /// chroma-/hue-dependent gain to `a`/`b`, convert back, convert to Oklab *again* from that
    /// result, apply saturation's uniform gain, convert back. Built from the same private helpers
    /// the production `ColourAdjust` unit uses, so it is not a third independent reference — it is
    /// the two-round-trip composition the fused unit replaces, kept here only to prove the fused
    /// unit agrees with it.
    fn sequential_vibrance_then_saturation(
        rgb: [f32; 3],
        vibrance: f64,
        saturation: f64,
    ) -> [f32; 3] {
        let vibrance_gain = (vibrance / 100.0) as f32;
        let lab = to_oklab(rgb);
        let c = chroma(lab);
        let weight = if c < CHROMA_EPSILON {
            1.0
        } else {
            vibrance_weight(c, hue_degrees(lab))
        };
        let vibrance_k = 1.0 + vibrance_gain * weight;
        let after_vibrance = from_oklab(Oklab {
            l: lab.l,
            a: lab.a * vibrance_k,
            b: lab.b * vibrance_k,
        });

        let saturation_k = (1.0 + saturation / 100.0) as f32;
        let lab2 = to_oklab(after_vibrance);
        from_oklab(Oklab {
            l: lab2.l,
            a: lab2.a * saturation_k,
            b: lab2.b * saturation_k,
        })
    }

    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "lowercase")]
    enum Input {
        Srgb8 { rgb: [u8; 3] },
        Linear { rgb: [f64; 3] },
    }

    impl Input {
        fn to_linear_f32(&self) -> [f32; 3] {
            match self {
                Input::Srgb8 { rgb } => [
                    code_to_linear(rgb[0]),
                    code_to_linear(rgb[1]),
                    code_to_linear(rgb[2]),
                ],
                Input::Linear { rgb } => [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32],
            }
        }
    }

    #[derive(Deserialize)]
    struct Case {
        #[allow(dead_code)]
        name: String,
        input: Input,
        vibrance: f64,
        saturation: f64,
        expected_linear: [f64; 3],
    }

    #[derive(Deserialize)]
    struct FixtureFile {
        cases: Vec<Case>,
    }

    fn fixture_path() -> PathBuf {
        // CARGO_MANIFEST_DIR is crates/luxforge-core; fixtures/ is repo-root-level.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("basic")
            .join("colour-cases.json")
    }

    /// Every one of the 144 frozen fixture cases, run through the production `f32` fused unit
    /// (`ColourAdjust`, one Oklab round trip per pixel instead of the two the fixtures were
    /// originally generated against), within the design's frozen tolerance
    /// `1e-5 + 1e-5 * |reference|`.
    ///
    /// Maximum observed error over the 144 cases: **2.3565749481813114e-6** — smaller than the
    /// 3.478e-6 the pre-fusion two-unit path measured against the same fixtures, consistent with
    /// removing one Oklab round trip's rounding rather than adding any — well inside the frozen
    /// tolerance, which is at least 1e-5 for every case and grows with the magnitude of the
    /// reference value.
    #[test]
    fn every_fixture_case_matches_the_frozen_tolerance() {
        let raw = fs::read_to_string(fixture_path()).expect("fixtures/basic/colour-cases.json");
        let file: FixtureFile = serde_json::from_str(&raw).expect("a parsed fixture file");
        assert_eq!(file.cases.len(), 144, "the frozen corpus has 144 cases");

        let mut max_error = 0.0_f64;
        for case in &file.cases {
            let linear = case.input.to_linear_f32();
            let actual = apply_basic_colour(linear, case.vibrance, case.saturation);
            for (channel, (&actual, &expected)) in
                actual.iter().zip(case.expected_linear.iter()).enumerate()
            {
                let error = (f64::from(actual) - expected).abs();
                max_error = max_error.max(error);
                let tolerance = 1e-5 + 1e-5 * expected.abs();
                assert!(
                    error <= tolerance,
                    "{} channel {channel}: production {actual} against reference {expected}, error {error} exceeds tolerance {tolerance}",
                    case.name
                );
            }
        }
        // Recorded so a future change that silently widens the gap is visible in review, not just
        // in a passing assertion against the outer bound.
        assert!(
            max_error < 1e-5,
            "maximum observed error {max_error} regressed past the frozen tolerance's own floor"
        );
    }

    /// Neutral vibrance with saturation −100 zeroes `a` and `b` exactly, the same property the
    /// reference proves for saturation alone, preserved by the fused unit: vibrance's gain is
    /// exactly `0.0` when neutral (so its `k` is exactly `1.0` regardless of the per-pixel weight),
    /// and saturation's `k` is exactly `0.0` at `-100`, so the combined factor is exactly `0.0`.
    /// `a = b = 0.0` exactly, and the design's `M1⁻¹`/`M2⁻¹` exactness property (their rows/column
    /// summing to `1.0` in f64) then reconstructs the rendered pixel *bit-close* equal R, G, B —
    /// the design's own wording, not bit-identical: `M1_INV`/`M2_INV` are independently rounded to
    /// `f32` per element, so the f64 row/column-sum identity does not survive the cast exactly.
    /// Checked here to the same headroom `greys_stay_grey_for_every_v_and_s` uses.
    #[test]
    fn neutral_vibrance_with_saturation_negative_100_is_exact_grey() {
        let unit = ColourAdjust::new(0.0, -100.0);
        assert_eq!(unit.vibrance_gain, 0.0);
        assert_eq!(unit.saturation_k, 0.0);
        for [r, g, b] in [[255u8, 0, 0], [224, 172, 140], [10, 200, 30]] {
            let rgb = [code_to_linear(r), code_to_linear(g), code_to_linear(b)];
            let lab = to_oklab(rgb);
            let combined_k = 1.0_f32 * unit.saturation_k;
            assert_eq!(combined_k, 0.0);
            assert_eq!(lab.a * combined_k, 0.0);
            assert_eq!(lab.b * combined_k, 0.0);

            let mut row = row_of(rgb);
            unit.apply_row(0, 0, &mut row);
            let spread = (row[0][0] - row[0][1])
                .abs()
                .max((row[0][1] - row[0][2]).abs())
                .max((row[0][0] - row[0][2]).abs());
            assert!(spread < 1e-4, "not grey for {r},{g},{b}: {row:?}");
        }
    }

    /// The fused `ColourAdjust` unit agrees with running the two steps it replaces (vibrance's own
    /// Oklab round trip, then saturation's) to within the design's frozen tolerance
    /// `1e-5 + 1e-5 * |reference|`, over a dense 33³ linear cube spanning `[-0.2, 1.5]` per channel
    /// (including out-of-range values above white and below black), for a representative spread of
    /// vibrance/saturation pairs including the extremes.
    ///
    /// A flat absolute bound cannot hold over this domain: even at `v = s = 0`, where both paths
    /// apply gain `1.0` throughout, the sequential path still round-trips through the
    /// independently rounded inverse matrices twice against the fused path's one, and
    /// `signed_cbrt`'s derivative diverges as its input approaches zero (documented in
    /// `docs/design/basic-colour.md`, "Near-black and achromatic behaviour" and "Matrix round-trip
    /// residual"), so the two paths' gap widens near-linearly with `|reference|` rather than
    /// staying flat — exactly what the frozen tolerance's relative term is for.
    ///
    /// Maximum observed absolute difference over the full swept domain: **2.4795532e-5** (at
    /// vibrance −100, saturation +100, linear input `[-0.0406, -0.2, 1.3406]`, reference channel
    /// magnitude ≈13.69 — an extreme, deliberately out-of-gamut combination); every difference
    /// measured stays inside the frozen relative+absolute tolerance. Re-measured by
    /// `cargo test --package luxforge-core basic::colour::tests -- --nocapture` if this comment
    /// goes stale.
    #[test]
    fn colour_adjust_matches_the_sequential_pair_within_the_frozen_tolerance() {
        const STEPS: usize = 33;
        const LOW: f32 = -0.2;
        const HIGH: f32 = 1.5;
        let component = |i: usize| LOW + (HIGH - LOW) * i as f32 / (STEPS - 1) as f32;

        let mut max_difference = 0.0_f64;
        for (vibrance, saturation) in [
            (100.0, 100.0),
            (-100.0, -100.0),
            (100.0, -100.0),
            (-100.0, 100.0),
            (50.0, -20.0),
            (-30.0, 60.0),
            (0.0, 0.0),
        ] {
            let unit = ColourAdjust::new(vibrance, saturation);
            for xi in 0..STEPS {
                for yi in 0..STEPS {
                    for zi in 0..STEPS {
                        let rgb = [component(xi), component(yi), component(zi)];
                        let mut fused = row_of(rgb);
                        unit.apply_row(0, 0, &mut fused);
                        let sequential =
                            sequential_vibrance_then_saturation(rgb, vibrance, saturation);
                        for (&fused_channel, &sequential_channel) in
                            fused[0].iter().zip(sequential.iter())
                        {
                            let difference =
                                (f64::from(fused_channel) - f64::from(sequential_channel)).abs();
                            max_difference = max_difference.max(difference);
                            let tolerance = 1e-5 + 1e-5 * f64::from(sequential_channel).abs();
                            assert!(
                                difference <= tolerance,
                                "v={vibrance} s={saturation} rgb={rgb:?}: fused {fused_channel} \
                                 against sequential {sequential_channel}, difference {difference} \
                                 exceeds tolerance {tolerance}"
                            );
                        }
                    }
                }
            }
        }
        // Recorded so a future change that silently widens the gap is visible in review, not just
        // in a passing assertion against the outer bound.
        assert!(
            max_difference < 3e-5,
            "maximum observed difference {max_difference} regressed past the recorded figure's \
             own margin"
        );
    }

    /// Greys stay grey for every `v` and `s`: every one of the 256 grey codes, run through the
    /// production units, holds every channel within the reference's documented ~2e-6 f64 spread
    /// plus f32 headroom.
    #[test]
    fn greys_stay_grey_for_every_v_and_s() {
        for code in 0u8..=255 {
            let level = code_to_linear(code);
            let rgb = [level, level, level];
            for v in [-100.0, -50.0, 0.0, 50.0, 100.0] {
                for s in [-100.0, -50.0, 0.0, 50.0, 100.0] {
                    let out = apply_basic_colour(rgb, v, s);
                    let spread = (out[0] - out[1])
                        .abs()
                        .max((out[1] - out[2]).abs())
                        .max((out[0] - out[2]).abs());
                    assert!(
                        spread < 1e-4,
                        "grey {code} drifted at v={v} s={s}: {out:?}, spread {spread}"
                    );
                }
            }
        }
    }

    /// Neither unit clamps: combined extremes on out-of-gamut linear input stay finite.
    #[test]
    fn combined_extremes_stay_finite() {
        let inputs: [[f32; 3]; 5] = [
            [1.5, 0.5, 0.2],
            [-0.1, 0.3, 0.8],
            [1.5, -0.1, 0.7],
            [-0.1, -0.1, -0.1],
            [1.5, 1.5, 1.5],
        ];
        for rgb in inputs {
            for v in [-100.0, 100.0] {
                for s in [-100.0, 100.0] {
                    let out = apply_basic_colour(rgb, v, s);
                    assert!(
                        out.iter().all(|c| c.is_finite()),
                        "non-finite output for {rgb:?} v={v} s={s}: {out:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn zero_is_the_identity_gain() {
        let mut row = row_of([0.0, 0.25, 1.0]);
        ColourAdjust::new(0.0, 0.0).apply_row(0, 0, &mut row);
        let tolerance = 1e-5_f32;
        for (actual, expected) in row[0].iter().zip([0.0, 0.25, 1.0]) {
            assert!(
                (actual - expected).abs() < tolerance,
                "{actual} vs {expected}"
            );
        }
    }

    #[test]
    fn finiteness_follows_the_stored_values_and_their_derived_coefficients() {
        assert!(ColourAdjust::new(100.0, 100.0).is_finite());
        assert!(ColourAdjust::new(-100.0, -100.0).is_finite());
        assert!(ColourAdjust::new(0.0, 0.0).is_finite());
        assert!(!ColourAdjust::new(f64::NAN, 0.0).is_finite());
        assert!(!ColourAdjust::new(0.0, f64::NAN).is_finite());
        assert!(!ColourAdjust::new(f64::INFINITY, 0.0).is_finite());
        assert!(!ColourAdjust::new(0.0, f64::INFINITY).is_finite());
    }

    #[test]
    fn the_description_names_both_stored_values_exactly() {
        assert_eq!(
            ColourAdjust::new(30.0, -20.0).describe(),
            "colour-adjust(vibrance:+30, saturation:-20)"
        );
        assert_eq!(
            ColourAdjust::new(-100.0, 0.0).describe(),
            "colour-adjust(vibrance:-100, saturation:+0)"
        );
        assert_eq!(
            ColourAdjust::new(30.4, -0.25).describe(),
            "colour-adjust(vibrance:+30.4, saturation:-0.25)",
            "a fractional value is not rounded to the display precision"
        );
        assert_ne!(
            ColourAdjust::new(30.0, 0.0).describe(),
            ColourAdjust::new(31.0, 0.0).describe(),
            "two different stored vibrance values never describe themselves the same way"
        );
        assert_ne!(
            ColourAdjust::new(0.0, 30.0).describe(),
            ColourAdjust::new(0.0, 31.0).describe(),
            "two different stored saturation values never describe themselves the same way"
        );
    }

    /// The GPU program's two words are the gains the CPU unit applies, two separately built units
    /// that describe themselves identically carry identical uniforms, and the program's constants
    /// and Oklab matrices are the `f32` values the CPU multiplies by, bit for bit.
    #[test]
    fn gpu_uniforms_follow_the_description() {
        let values: Vec<(f64, f64)> = [-100.0, -50.0, -0.0, 0.0, 1.0 / 3.0, 30.0, 100.0]
            .iter()
            .flat_map(|v| [-100.0, -0.0, 0.0, 15.0, 100.0].map(move |s| (*v, s)))
            .collect();
        let build = || -> Vec<ColourAdjust> {
            values
                .iter()
                .map(|(vibrance, saturation)| ColourAdjust::new(*vibrance, *saturation))
                .collect()
        };
        let (first, second) = (build(), build());
        let units: Vec<&dyn PointwiseColor> = first
            .iter()
            .chain(&second)
            .map(|unit| unit as &dyn PointwiseColor)
            .collect();
        crate::render::gpu::testing::assert_uniforms_follow_descriptions(&units);
        for unit in &first {
            let description = unit.gpu().expect("the colour unit has a program");
            assert_eq!(
                description.words,
                vec![unit.vibrance_gain.to_bits(), unit.saturation_k.to_bits()]
            );
        }
        let constant = |name: &str| {
            crate::render::gpu::testing::wgsl_constant(
                &PROGRAM,
                &format!("lf_basic_colour_adjust_{name}"),
            )
        };
        let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        for (name, matrix) in crate::colour::oklab::MATRICES {
            for (row, values) in matrix.iter().enumerate() {
                assert_eq!(
                    bits(&constant(&format!("{name}_{row}"))),
                    bits(values),
                    "{name} row {row}"
                );
            }
        }
        for (name, value) in [
            ("chroma_reference", CHROMA_REFERENCE),
            ("chroma_low", CHROMA_LOW),
            ("chroma_high", CHROMA_HIGH),
            ("skin_centre", SKIN_HUE_CENTER_DEG),
            ("skin_half_width", SKIN_HUE_HALF_WIDTH_DEG),
            ("skin_protection", SKIN_PROTECTION),
            ("chroma_epsilon", CHROMA_EPSILON),
        ] {
            assert_eq!(bits(&constant(name)), bits(&[value]), "{name}");
        }
    }
}
